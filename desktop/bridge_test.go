package main

import (
	"context"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo"
)

func init() {
	if os.Getenv("CHAOXING_BRIDGE_TEST_CHILD") != "1" {
		return
	}
	want := strings.Split(os.Getenv("CHAOXING_BRIDGE_TEST_ARGS"), "\x1f")
	if !reflect.DeepEqual(os.Args[1:], want) {
		os.Stderr.WriteString("unexpected Rust arguments")
		os.Exit(2)
	}
	if os.Getenv("CHAOXING_BRIDGE_TEST_UI") == "1" && os.Getenv("FKCHAOXING_UI_EVENTS") != "1" {
		os.Stderr.WriteString("missing structured UI events flag")
		os.Exit(2)
	}
	if cwd := os.Getenv("CHAOXING_BRIDGE_TEST_CWD"); cwd != "" {
		actual, err := os.Getwd()
		if err != nil || actual != cwd {
			os.Stderr.WriteString("unexpected Rust working directory")
			os.Exit(2)
		}
	}
	if os.Getenv("CHAOXING_BRIDGE_TEST_WAIT") == "1" {
		time.Sleep(30 * time.Second)
	}
	if output := os.Getenv("CHAOXING_BRIDGE_TEST_OUTPUT"); output != "" {
		os.Stdout.WriteString(output)
	} else {
		os.Stdout.WriteString("B\n")
	}
	if code, err := strconv.Atoi(os.Getenv("CHAOXING_BRIDGE_TEST_EXIT")); err == nil {
		os.Exit(code)
	}
	os.Exit(0)
}

func TestAskPassesQuestionToRustAndReturnsAnswer(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	config := absoluteConfig(t, "config.ini")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{
		"-c", config, "ask", "中国的首都是？", "-t", "multiple", "-o", "A. 上海", "-o", "B. 北京", "--course", "地理", "--use-cache",
	}, "\x1f"))
	bridge := testBridge(t)
	answer, err := bridge.Ask(context.Background(), AskRequest{
		Config: "config.ini", Title: "中国的首都是？", Type: "multiple",
		Options: []string{"A. 上海", "B. 北京"}, Course: "地理", UseCache: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if answer != "B" {
		t.Fatalf("answer = %q, want B", answer)
	}
}

func TestAskRejectsEmptyQuestion(t *testing.T) {
	bridge := testBridge(t)
	_, err := bridge.Ask(context.Background(), AskRequest{Title: "   "})
	if err == nil || !strings.Contains(err.Error(), "题目") {
		t.Fatalf("error = %v, want question validation", err)
	}
}

func TestCoursesParsesRustJSON(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "courses", "--json"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", `[{"courseId":"1","clazzId":"2","title":"数学"}]`)
	bridge := testBridge(t)
	courses, err := bridge.Courses(context.Background(), "config.ini")
	if err != nil {
		t.Fatal(err)
	}
	if len(courses) != 1 || courses[0].Title != "数学" {
		t.Fatalf("courses = %#v", courses)
	}
}

func TestCoursesUsesAbsoluteConfigAndPersistentDataDirectory(t *testing.T) {
	dataDir := t.TempDir()
	mygo.App.SetPath(mygo.PathUserData, dataDir)
	t.Cleanup(func() { mygo.App.SetPath(mygo.PathUserData, "") })
	config := filepath.Join(t.TempDir(), "config.ini")
	relative, err := filepath.Rel(mustWorkingDirectory(t), config)
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", config, "courses", "--json"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_CWD", dataDir)
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", `[]`)
	bridge := &Bridge{corePath: os.Args[0]}
	if _, err := bridge.Courses(context.Background(), relative); err != nil {
		t.Fatal(err)
	}
}

func TestCoursesRunsRelativeCorePathFromDataDirectory(t *testing.T) {
	dataDir := t.TempDir()
	mygo.App.SetPath(mygo.PathUserData, dataDir)
	t.Cleanup(func() { mygo.App.SetPath(mygo.PathUserData, "") })
	core, err := filepath.Rel(mustWorkingDirectory(t), os.Args[0])
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "courses", "--json"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_CWD", dataDir)
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", `[]`)
	bridge := &Bridge{corePath: core}
	if _, err := bridge.Courses(context.Background(), "config.ini"); err != nil {
		t.Fatal(err)
	}
}

func mustWorkingDirectory(t *testing.T) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestStudyPassesSelectedCourseIDs(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "study", "--course-ids", "1,2"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_UI", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", "2026-11-01 23:33:58  INFO 共读取到 2 个章节任务\n2026-11-01 23:34:01  INFO 完成章节：数学 / 1.1\n")
	bridge := testBridge(t)
	if err := bridge.Study(context.Background(), "config.ini", []string{"1", "2"}); err != nil {
		t.Fatal(err)
	}
	dataDir, err := mygo.App.Path(mygo.PathUserData)
	if err != nil {
		t.Fatal(err)
	}
	update := bridge.StudyLog(0)
	if len(update.Lines) != 2 || update.Lines[1].Text != "完成章节：数学 / 1.1" || update.Lines[1].Kind != "done" {
		t.Fatalf("lines = %#v", update.Lines)
	}
	if update.Total != 2 || update.Done != 1 || update.Next != 2 {
		t.Fatalf("update = %+v", update)
	}
	if want := filepath.Join(dataDir, "fkChaoxing.log"); update.File != want {
		t.Fatalf("log file = %q, want %q", update.File, want)
	}
}

func TestStudyFailureReportsTheReasonInsteadOfTheWholeLog(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "study", "--course-ids", "1"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", "2026-11-01 23:33:58  INFO 开始任务：数学\n2026-11-01 23:33:59 ERROR 章节失败：数学 / 1.1: 超时\n有 1 个章节未完成，请查看日志\n")
	t.Setenv("CHAOXING_BRIDGE_TEST_EXIT", "1")
	bridge := testBridge(t)
	err := bridge.Study(context.Background(), "config.ini", []string{"1"})
	if err == nil || err.Error() != "Rust 学习任务失败：有 1 个章节未完成，请查看日志" {
		t.Fatalf("error = %v", err)
	}
	if update := bridge.StudyLog(0); len(update.Lines) != 3 || update.Errors != 1 || update.Failed != 1 {
		t.Fatalf("the log should stay readable after a failure: %+v", update)
	}
}

func TestEachStudyStartsAFreshLog(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "study", "--course-ids", "1"}, "\x1f"))
	bridge := testBridge(t)
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", "2026-11-01 23:33:58  INFO 第一次\n")
	if err := bridge.Study(context.Background(), "config.ini", []string{"1"}); err != nil {
		t.Fatal(err)
	}
	first := bridge.StudyLog(0)
	t.Setenv("CHAOXING_BRIDGE_TEST_OUTPUT", "2026-11-01 23:40:00  INFO 第二次\n")
	if err := bridge.Study(context.Background(), "config.ini", []string{"1"}); err != nil {
		t.Fatal(err)
	}
	// The window keeps its old cursor until it sees the run number change, then starts over.
	if stale := bridge.StudyLog(first.Next); stale.Run == first.Run {
		t.Fatalf("run = %d for both studies; the window could not tell the log was reset", stale.Run)
	}
	second := bridge.StudyLog(0)
	if len(second.Lines) != 1 || second.Lines[0].Text != "第二次" {
		t.Fatalf("second log = %#v", second.Lines)
	}
}

func TestStudyCanBeStopped(t *testing.T) {
	t.Setenv("CHAOXING_BRIDGE_TEST_CHILD", "1")
	t.Setenv("CHAOXING_BRIDGE_TEST_ARGS", strings.Join([]string{"-c", absoluteConfig(t, "config.ini"), "study", "--course-ids", "1"}, "\x1f"))
	t.Setenv("CHAOXING_BRIDGE_TEST_WAIT", "1")
	bridge := testBridge(t)
	finished := make(chan error, 1)
	go func() { finished <- bridge.Study(context.Background(), "", []string{"1"}) }()
	deadline := time.After(3 * time.Second)
	for {
		bridge.mu.Lock()
		running := bridge.cancel != nil
		bridge.mu.Unlock()
		if running {
			break
		}
		select {
		case <-deadline:
			t.Fatal("study did not start")
		case <-time.After(5 * time.Millisecond):
		}
	}
	bridge.StopStudy()
	select {
	case err := <-finished:
		if err == nil || !strings.Contains(err.Error(), "已停止") {
			t.Fatalf("error = %v", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("child process was not stopped")
	}
}

// mygo.Bind inspects every exported method when the app starts and panics on a type
// JSON cannot encode, which would crash the window before it opens.
func TestBridgeMethodsCanBeBoundToTheWindow(t *testing.T) {
	defer func() {
		if recovered := recover(); recovered != nil {
			t.Fatalf("mygo.Bind(&Bridge{}) panicked: %v", recovered)
		}
	}()
	mygo.Bind(&Bridge{})
}

func testBridge(t *testing.T) *Bridge {
	t.Helper()
	mygo.App.SetPath(mygo.PathUserData, t.TempDir())
	t.Cleanup(func() { mygo.App.SetPath(mygo.PathUserData, "") })
	return &Bridge{corePath: os.Args[0]}
}

func absoluteConfig(t *testing.T, path string) string {
	t.Helper()
	absolute, err := filepath.Abs(path)
	if err != nil {
		t.Fatal(err)
	}
	return absolute
}

func TestShouldUseX11InMixedSession(t *testing.T) {
	if !shouldUseX11("", ":1", "wayland-1") {
		t.Fatal("mixed session should prefer X11")
	}
}

func TestShouldUseX11HonorsExplicitPreference(t *testing.T) {
	if shouldUseX11("wayland", ":1", "wayland-1") || shouldUseX11("", "", "wayland-1") {
		t.Fatal("explicit backend or missing X11 should not trigger relaunch")
	}
}
