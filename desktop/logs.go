package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	// maxLogLines is how many lines the bridge keeps for the window; the oldest go first.
	// The buffer may overshoot by logTrimSlack so trimming does not run on every line.
	maxLogLines  = 5000
	logTrimSlack = 500
)

var (
	ansiSequence = regexp.MustCompile(`\x1b\[[0-9;]*[A-Za-z]`)
	// The Rust core logs "2006-01-02 15:04:05  INFO message" (tracing's default layout).
	logHeader    = regexp.MustCompile(`^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\s+(TRACE|DEBUG|INFO|WARN|ERROR)(?:\s+(.*))?$`)
	chapterTotal = regexp.MustCompile(`^共读取到 (\d+) 个章节任务`)
)

// LogLine is one entry of the Rust core's output.
type LogLine struct {
	// Time is "2006-01-02 15:04:05"; empty for lines the core printed without one.
	Time string `json:"time"`
	// Level is info, warn, error, debug or trace. It is empty for plain lines, such as
	// the final error message the core prints before it exits.
	Level string `json:"level"`
	// Kind marks routine lines the window can dim or highlight: start, progress or done.
	Kind string `json:"kind,omitempty"`
	Text string `json:"text"`
}

// LogUpdate is the part of the log the window has not seen yet, plus the totals for its
// status bar.
type LogUpdate struct {
	Progress []TaskProgress `json:"progress"`
	Courses  []CourseView   `json:"courses"`
	Chapters []ChapterView  `json:"chapters"`
	Run      int            `json:"run"`    // changes whenever a new study starts
	Lines    []LogLine      `json:"lines"`  // entries after the cursor the window asked for
	Next     int            `json:"next"`   // the cursor to ask for next time
	Total    int            `json:"total"`  // chapters found, 0 while unknown
	Done     int            `json:"done"`   // chapters finished or skipped
	Failed   int            `json:"failed"` // chapters that gave up
	Warns    int            `json:"warns"`
	Errors   int            `json:"errors"`
	File     string         `json:"file"` // the core's full log file
}

type logTotals struct{ total, done, failed, warns, errors int }

// CourseView is one course's progress in the window's left column.
type CourseView struct {
	Course   string `json:"course"`
	Clazz    string `json:"clazz"`
	Chapters int    `json:"chapters"`
	Done     int    `json:"done"`
	Failed   int    `json:"failed"`
	Warnings int    `json:"warnings"`
	State    string `json:"state"`
}

// AnswerView is one question answered inside a chapter test.
type AnswerView struct {
	Index     int    `json:"index"`
	Total     int    `json:"total"`
	Kind      string `json:"kind"`
	Title     string `json:"title"`
	Source    string `json:"source"`
	Answer    string `json:"answer"`
	Hit       bool   `json:"hit"`
	Submitted bool   `json:"submitted"`
	Time      string `json:"time"`
}

// ChapterView groups a chapter's questions and warnings for the log panel.
type ChapterView struct {
	Key       string       `json:"key"`
	Course    string       `json:"course"`
	Clazz     string       `json:"clazz"`
	Title     string       `json:"title"`
	State     string       `json:"state"`
	Questions int          `json:"questions"`
	Covered   int          `json:"covered"`
	Submitted bool         `json:"submitted"`
	Answers   []AnswerView `json:"answers"`
	Warnings  int          `json:"warnings"`
	Errors    int          `json:"errors"`
}

type uiEvent struct {
	Type      string `json:"type"`
	Course    string `json:"course"`
	Clazz     string `json:"clazz"`
	Chapter   string `json:"chapter"`
	State     string `json:"state"`
	Chapters  int    `json:"chapters"`
	Questions int    `json:"questions"`
	Covered   int    `json:"covered"`
	Index     int    `json:"index"`
	Total     int    `json:"total"`
	Kind      string `json:"kind"`
	Title     string `json:"title"`
	Source    string `json:"source"`
	Answer    string `json:"answer"`
	Hit       bool   `json:"hit"`
	Submitted bool   `json:"submitted"`
}

type TaskProgress struct {
	ID       string `json:"id"`
	Label    string `json:"label"`
	Position uint64 `json:"position"`
	Total    uint64 `json:"total"`
	State    string `json:"state"`
}

// logBuffer turns the Rust core's output into log lines and keeps the totals. It is an
// io.Writer, so one value can take both stdout and stderr of the child process.
type logBuffer struct {
	mu       sync.Mutex
	run      int
	file     string
	lines    []LogLine
	first    int    // absolute index of lines[0]; cursors count from the start of the run
	partial  []byte // bytes after the last newline
	totals   logTotals
	progress map[string]TaskProgress
	courses  []CourseView
	chapters []ChapterView
}

// Reset starts a new run. The window notices the new run number and drops what it showed.
func (l *logBuffer) Reset() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.run++
	l.file = ""
	l.lines, l.first, l.partial = nil, 0, nil
	l.totals = logTotals{}
	l.progress = make(map[string]TaskProgress)
	l.courses, l.chapters = nil, nil
}

func (l *logBuffer) SetFile(path string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.file = path
}

func (l *logBuffer) FilePath() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.file
}

// A killed child cannot emit final events; clear its remaining progress bars.
func (l *logBuffer) EndProgress() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.progress = make(map[string]TaskProgress)
	for i := range l.chapters {
		if l.chapters[i].State == "start" {
			l.chapters[i].State = "stopped"
		}
	}
}

func (l *logBuffer) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.partial = append(l.partial, p...)
	for {
		end := bytes.IndexByte(l.partial, '\n')
		if end < 0 {
			break
		}
		l.add(string(l.partial[:end]))
		l.partial = l.partial[end+1:]
	}
	if len(l.partial) == 0 {
		l.partial = nil
	} else {
		l.partial = append([]byte(nil), l.partial...)
	}
	return len(p), nil
}

// Flush turns the last line, if the core did not end it with a newline, into an entry.
func (l *logBuffer) Flush() {
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.partial) > 0 {
		l.add(string(l.partial))
		l.partial = nil
	}
}

func (l *logBuffer) add(raw string) {
	line := strings.TrimRight(ansiSequence.ReplaceAllString(raw, ""), " \t\r")
	if strings.TrimSpace(line) == "" {
		return
	}
	if m := logHeader.FindStringSubmatch(line); m != nil {
		l.push(LogLine{Time: m[1], Level: strings.ToLower(m[2]), Text: m[3]})
		return
	}
	// Messages the core wraps over several lines continue with an indented line.
	if n := len(l.lines); n > 0 && (line[0] == ' ' || line[0] == '\t') {
		l.lines[n-1].Text += "\n" + line
		return
	}
	l.push(LogLine{Text: line})
}

func (l *logBuffer) push(line LogLine) {
	if data, ok := strings.CutPrefix(line.Text, "[ui] "); ok {
		var event uiEvent
		if json.Unmarshal([]byte(data), &event) == nil {
			l.applyUI(event, line.Time)
			return
		}
	}
	if data, ok := strings.CutPrefix(line.Text, "[progress] "); ok {
		var progress TaskProgress
		if json.Unmarshal([]byte(data), &progress) == nil && progress.ID != "" && progress.Label != "" &&
			(progress.State == "running" || progress.State == "done" || progress.State == "failed") {
			if progress.State == "running" {
				if l.progress == nil {
					l.progress = make(map[string]TaskProgress)
				}
				l.progress[progress.ID] = progress
				return
			}
			delete(l.progress, progress.ID)
			line.Text = "任务完成：" + progress.Label
			if progress.State == "failed" {
				line.Text = "任务结束：" + progress.Label + "（未完成）"
			}
		}
	}
	l.attachIssue(line)
	line.Kind = lineKind(line.Text)
	switch line.Level {
	case "warn":
		l.totals.warns++
	case "error":
		l.totals.errors++
	}
	switch {
	case line.Kind == "done":
		l.totals.done++
	case strings.HasPrefix(line.Text, "章节失败："):
		l.totals.failed++
	}
	if m := chapterTotal.FindStringSubmatch(line.Text); m != nil {
		l.totals.total, _ = strconv.Atoi(m[1])
	}
	l.lines = append(l.lines, line)
	if len(l.lines) > maxLogLines+logTrimSlack {
		drop := len(l.lines) - maxLogLines
		l.lines = append([]LogLine(nil), l.lines[drop:]...)
		l.first += drop
	}
}

// applyUI folds one structured event from the Rust core into the course and chapter views.
func (l *logBuffer) applyUI(event uiEvent, stamp string) {
	switch event.Type {
	case "course":
		if index := l.courseIndex(event.Course, event.Clazz); index >= 0 {
			l.courses[index].Chapters = event.Chapters
			return
		}
		l.courses = append(l.courses, CourseView{Course: event.Course, Clazz: event.Clazz, Chapters: event.Chapters, State: "ready"})
	case "chapter":
		index := l.chapterIndex(event.Course, event.Clazz, event.Chapter)
		previous := ""
		if index < 0 {
			l.chapters = append(l.chapters, ChapterView{
				Key: chapterKey(event.Course, event.Clazz, event.Chapter), Course: event.Course,
				Clazz: event.Clazz, Title: event.Chapter, State: event.State, Answers: []AnswerView{},
			})
			index = len(l.chapters) - 1
		} else {
			previous = l.chapters[index].State
			l.chapters[index].State = event.State
		}
		if event.State == "done" && previous != "done" {
			l.courseDone(event.Course, event.Clazz)
		}
		if event.State == "failed" && previous != "failed" {
			l.courseFailed(event.Course, event.Clazz)
		}
	case "work":
		if index := l.chapterIndex(event.Course, event.Clazz, event.Chapter); index >= 0 {
			l.chapters[index].Questions = event.Questions
			l.chapters[index].Covered = event.Covered
			l.chapters[index].Submitted = event.Submitted
		}
	case "answer":
		index := l.chapterIndex(event.Course, event.Clazz, event.Chapter)
		if index < 0 {
			l.chapters = append(l.chapters, ChapterView{
				Key: chapterKey(event.Course, event.Clazz, event.Chapter), Course: event.Course,
				Clazz: event.Clazz, Title: event.Chapter, State: "start", Answers: []AnswerView{},
			})
			index = len(l.chapters) - 1
		}
		if stamp != "" {
			if parsed, err := time.ParseInLocation("2006-01-02 15:04:05", stamp, time.Local); err == nil {
				stamp = parsed.Format("15:04:05")
			}
		}
		l.chapters[index].Answers = append(l.chapters[index].Answers, AnswerView{
			Index: event.Index, Total: event.Total, Kind: event.Kind, Title: event.Title,
			Source: event.Source, Answer: event.Answer, Hit: event.Hit, Submitted: event.Submitted, Time: stamp,
		})
	}
}

func chapterKey(course, clazz, chapter string) string {
	return course + "\x00" + clazz + "\x00" + chapter
}

func (l *logBuffer) courseIndex(course, clazz string) int {
	for i := range l.courses {
		if l.courses[i].Course == course && l.courses[i].Clazz == clazz {
			return i
		}
	}
	return -1
}

func (l *logBuffer) chapterIndex(course, clazz, chapter string) int {
	for i := range l.chapters {
		if l.chapters[i].Course == course && l.chapters[i].Clazz == clazz && l.chapters[i].Title == chapter {
			return i
		}
	}
	return -1
}

func (l *logBuffer) courseFailed(course, clazz string) {
	for i := range l.courses {
		if l.courses[i].Course == course && l.courses[i].Clazz == clazz {
			l.courses[i].Failed++
			return
		}
	}
}

func (l *logBuffer) courseDone(course, clazz string) {
	for i := range l.courses {
		if l.courses[i].Course == course && l.courses[i].Clazz == clazz {
			l.courses[i].Done++
			return
		}
	}
}

// attachIssue turns a chapter-specific warning into a count on its card.
func (l *logBuffer) attachIssue(line LogLine) {
	if line.Level != "warn" && line.Level != "error" {
		return
	}
	for i := range l.chapters {
		prefix := l.chapters[i].Course + " / " + l.chapters[i].Title
		if !strings.Contains(line.Text, prefix) {
			continue
		}
		if line.Level == "warn" {
			l.chapters[i].Warnings++
		} else {
			l.chapters[i].Errors++
		}
		for k := range l.courses {
			if l.courses[k].Course == l.chapters[i].Course {
				l.courses[k].Warnings++
			}
		}
		return
	}
}

// lineKind recognizes the milestones the window highlights. The prefixes mirror the
// messages in src/runner.rs and src/study/mod.rs.
func lineKind(text string) string {
	switch {
	case strings.HasPrefix(text, "开始任务："):
		return "start"
	case strings.HasPrefix(text, "媒体进度："):
		return "progress"
	case strings.HasPrefix(text, "完成章节："):
		return "done"
	}
	return ""
}

// Since returns the lines from the absolute index after on, and the totals of the run.
func (l *logBuffer) Since(after int) LogUpdate {
	l.mu.Lock()
	defer l.mu.Unlock()
	next := l.first + len(l.lines)
	start := min(max(after, l.first), next)
	lines := make([]LogLine, next-start)
	copy(lines, l.lines[start-l.first:])
	progress := make([]TaskProgress, 0, len(l.progress))
	for _, item := range l.progress {
		progress = append(progress, item)
	}
	sort.Slice(progress, func(i, j int) bool { return progress[i].ID < progress[j].ID })
	courses := make([]CourseView, len(l.courses))
	copy(courses, l.courses)
	for i := range courses {
		courses[i].State = courseState(courses[i])
	}
	chapters := make([]ChapterView, len(l.chapters))
	for i, chapter := range l.chapters {
		chapters[i] = chapter
		chapters[i].Answers = append([]AnswerView(nil), chapter.Answers...)
	}
	return LogUpdate{
		Progress: progress, Courses: courses, Chapters: chapters,
		Run: l.run, Lines: lines, Next: next, File: l.file,
		Total: l.totals.total, Done: l.totals.done, Failed: l.totals.failed,
		Warns: l.totals.warns, Errors: l.totals.errors,
	}
}

func courseState(course CourseView) string {
	if course.Chapters > 0 && course.Done >= course.Chapters {
		return "done"
	}
	if course.Warnings > 0 || course.Failed > 0 {
		return "warn"
	}
	if course.Done > 0 {
		return "running"
	}
	return "ready"
}

// Text renders the kept lines the way the core logs them, for pasting into a bug report.
func (l *logBuffer) Text() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	var out strings.Builder
	for _, line := range l.lines {
		if line.Time != "" {
			out.WriteString(line.Time)
			out.WriteByte(' ')
		}
		if line.Level != "" {
			fmt.Fprintf(&out, "%5s ", strings.ToUpper(line.Level))
		}
		out.WriteString(line.Text)
		out.WriteByte('\n')
	}
	return out.String()
}

// Failure says why the core failed: the plain message it prints before exiting, or else
// its last error line. It is empty when the output has neither.
func (l *logBuffer) Failure() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	end := len(l.lines)
	start := end
	for start > 0 && l.lines[start-1].Level == "" {
		start--
	}
	if start < end {
		parts := make([]string, 0, end-start)
		for _, line := range l.lines[start:end] {
			parts = append(parts, line.Text)
		}
		return clip(strings.Join(parts, "\n"), 600)
	}
	for i := end - 1; i >= 0; i-- {
		if l.lines[i].Level == "error" {
			return clip(l.lines[i].Text, 600)
		}
	}
	return ""
}

func clip(text string, limit int) string {
	if runes := []rune(text); len(runes) > limit {
		return string(runes[:limit]) + "…"
	}
	return text
}
