package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
)

// AskRequest is the question sent from the MyGo window to the Rust core.
type AskRequest struct {
	Config   string   `json:"config"`
	Title    string   `json:"title"`
	Type     string   `json:"type"`
	Options  []string `json:"options"`
	Course   string   `json:"course"`
	UseCache bool     `json:"useCache"`
}

type Bridge struct {
	corePath string
	mu       sync.Mutex
	cancel   context.CancelFunc
	log      logBuffer
}

// StudyLog returns the log lines after the cursor (0 for all of them) and the totals of
// the current run. The window polls it while a study runs.
func (b *Bridge) StudyLog(after int) LogUpdate {
	return b.log.Since(after)
}

// OpenLogFile opens the core's log file in the system's default application.
func (b *Bridge) OpenLogFile() error {
	path := b.log.FilePath()
	if path == "" {
		return errors.New("还没有日志文件")
	}
	if err := mygo.Shell.OpenPath(path); err != nil {
		return fmt.Errorf("打开日志文件失败：%w", err)
	}
	return nil
}

// CopyLog puts the whole log on the clipboard, for pasting into a bug report.
func (b *Bridge) CopyLog() error {
	text := b.log.Text()
	if text == "" {
		return errors.New("还没有日志可以复制")
	}
	// ErrClipboardPersistence only says the text will not outlive this window.
	if err := mygo.Clipboard.Write(transfer.TextData(text)); err != nil && !errors.Is(err, mygo.ErrClipboardPersistence) {
		return fmt.Errorf("复制日志失败：%w", err)
	}
	return nil
}

type Course struct {
	CourseID string `json:"courseId"`
	ClazzID  string `json:"clazzId"`
	Title    string `json:"title"`
	Teacher  string `json:"teacher"`
}

func (b *Bridge) ChooseConfig() (string, error) {
	paths, err := mygo.Dialog.Open(mygo.OpenDialogOptions{
		Title:   "选择超星配置文件",
		Filters: []mygo.FileFilter{{Name: "INI 配置文件", Extensions: []string{"ini"}}},
	})
	if err != nil || len(paths) == 0 {
		return "", err
	}
	if err := rememberConfig(paths[0]); err != nil {
		return "", err
	}
	return paths[0], nil
}

func (b *Bridge) core() (string, error) {
	if b.corePath != "" {
		return b.corePath, nil
	}
	return findCore()
}

func (b *Bridge) Courses(ctx context.Context, config string) ([]Course, error) {
	core, err := b.core()
	if err != nil {
		return nil, err
	}
	args, err := configArgs(config)
	if err != nil {
		return nil, err
	}
	args = append(args, "courses", "--json")
	cmd, err := b.command(ctx, core, args...)
	if err != nil {
		return nil, err
	}
	output, err := cmd.Output()
	if err != nil {
		return nil, commandError(err, cmd)
	}
	var courses []Course
	if err := json.Unmarshal(output, &courses); err != nil {
		return nil, fmt.Errorf("解析课程列表失败：%w", err)
	}
	return courses, nil
}

// Study runs the selected courses and blocks until the Rust core exits. Its output is
// kept in the log the window reads with StudyLog; an error carries only the reason.
func (b *Bridge) Study(ctx context.Context, config string, courseIDs []string) error {
	core, err := b.core()
	if err != nil {
		return err
	}
	if len(courseIDs) == 0 {
		return errors.New("请先选择课程")
	}
	studyCtx, cancel := context.WithCancel(ctx)
	b.mu.Lock()
	if b.cancel != nil {
		b.mu.Unlock()
		cancel()
		return errors.New("学习任务已经在运行")
	}
	b.cancel = cancel
	b.log.Reset()
	b.mu.Unlock()
	defer func() { b.mu.Lock(); b.cancel = nil; b.mu.Unlock(); cancel() }()
	args, err := configArgs(config)
	if err != nil {
		return err
	}
	args = append(args, "study", "--course-ids", strings.Join(courseIDs, ","))
	cmd, err := b.command(studyCtx, core, args...)
	if err != nil {
		return err
	}
	// The window groups chapters and questions from these events; the terminal does not print them.
	cmd.Env = append(os.Environ(), "FKCHAOXING_UI_EVENTS=1")
	b.log.SetFile(filepath.Join(cmd.Dir, "fkChaoxing.log"))
	cmd.Stdout = &b.log
	cmd.Stderr = &b.log
	err = cmd.Run()
	b.log.Flush()
	b.log.EndProgress()
	if err != nil {
		if studyCtx.Err() != nil {
			return errors.New("学习任务已停止")
		}
		message := b.log.Failure()
		if message == "" {
			message = err.Error()
		}
		return fmt.Errorf("Rust 学习任务失败：%s", message)
	}
	return nil
}

func (b *Bridge) StopStudy() {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.cancel != nil {
		b.cancel()
	}
}

func configArgs(config string) ([]string, error) {
	if config = strings.TrimSpace(config); config == "" {
		config = "config.ini"
	}
	absolute, err := filepath.Abs(config)
	if err != nil {
		return nil, fmt.Errorf("解析配置文件路径失败：%w", err)
	}
	return []string{"-c", absolute}, nil
}

func (b *Bridge) command(ctx context.Context, core string, args ...string) (*exec.Cmd, error) {
	dataDir, err := mygo.App.Path(mygo.PathUserData)
	if err != nil {
		return nil, fmt.Errorf("创建应用数据目录失败：%w", err)
	}
	// cmd.Dir changes how a relative core path is resolved, and the core is found
	// relative to the window's working directory. Pin it down before changing Dir.
	if !filepath.IsAbs(core) {
		core, err = filepath.Abs(core)
		if err != nil {
			return nil, fmt.Errorf("解析 Rust 核心路径失败：%w", err)
		}
	}
	cmd := exec.CommandContext(ctx, core, args...)
	cmd.Dir = dataDir
	return cmd, nil
}

func commandError(err error, cmd *exec.Cmd) error {
	if exit, ok := err.(*exec.ExitError); ok {
		message := strings.TrimSpace(string(exit.Stderr))
		if message != "" {
			return fmt.Errorf("Rust 命令失败：%s", message)
		}
	}
	return fmt.Errorf("Rust 命令失败：%w", err)
}

// Ask runs a single Rust question lookup. The caller's context cancels the child process
// when the window closes or navigates away.
func (b *Bridge) Ask(ctx context.Context, question AskRequest) (string, error) {
	question.Title = strings.TrimSpace(question.Title)
	if question.Title == "" {
		return "", errors.New("请先填写题目")
	}
	corePath, err := b.core()
	if err != nil {
		return "", err
	}

	args := make([]string, 0, 8+len(question.Options)*2)
	config, err := configArgs(question.Config)
	if err != nil {
		return "", err
	}
	args = append(args, config...)
	args = append(args, "ask", question.Title)
	if question.Type != "" {
		args = append(args, "-t", question.Type)
	}
	for _, option := range question.Options {
		if option = strings.TrimSpace(option); option != "" {
			args = append(args, "-o", option)
		}
	}
	if course := strings.TrimSpace(question.Course); course != "" {
		args = append(args, "--course", course)
	}
	if question.UseCache {
		args = append(args, "--use-cache")
	}

	cmd, err := b.command(ctx, corePath, args...)
	if err != nil {
		return "", err
	}
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	stdout, err := cmd.Output()
	if err != nil {
		message := strings.TrimSpace(stderr.String())
		if message == "" {
			message = err.Error()
		}
		return "", fmt.Errorf("Rust 查询失败：%s", message)
	}
	answer := strings.TrimSpace(string(stdout))
	if answer == "" {
		return "", errors.New("Rust 查询没有返回答案")
	}
	return answer, nil
}

func findCore() (string, error) {
	if path := os.Getenv("FKCHAOXING_CORE_BIN"); path != "" {
		return path, nil
	}
	if path := os.Getenv("CHAOXING_CORE_BIN"); path != "" {
		return path, nil
	}
	name := "fkChaoxing-core"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	if resources, err := mygo.App.Path(mygo.PathResources); err == nil {
		path := filepath.Join(resources, "bin", name)
		if _, err := os.Stat(path); err == nil {
			return path, nil
		}
	}
	if executable, err := os.Executable(); err == nil {
		path := filepath.Join(filepath.Dir(executable), name)
		if _, err := os.Stat(path); err == nil {
			return path, nil
		}
	}
	developmentName := "fkChaoxing"
	if runtime.GOOS == "windows" {
		developmentName += ".exe"
	}
	for _, path := range []string{
		filepath.Join("..", "target", "debug", developmentName),
		filepath.Join("target", "debug", developmentName),
	} {
		if _, err := os.Stat(path); err == nil {
			return path, nil
		}
	}
	return "", errors.New("找不到 Rust 核心；请先运行 cargo build，或设置 FKCHAOXING_CORE_BIN")
}
