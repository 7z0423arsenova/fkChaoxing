package main

import (
	"fmt"
	"reflect"
	"strings"
	"testing"
)

func newLog(t *testing.T, output string) *logBuffer {
	t.Helper()
	var log logBuffer
	log.Reset()
	if _, err := log.Write([]byte(output)); err != nil {
		t.Fatal(err)
	}
	log.Flush()
	return &log
}

func TestLogBufferParsesCoreOutput(t *testing.T) {
	log := newLog(t, strings.Join([]string{
		"2026-11-01 23:33:58  INFO 共读取到 3 个章节任务",
		"2026-11-01 23:33:59  INFO 开始任务：数学 / 1.1 / 视频",
		"2026-11-01 23:34:30  INFO 媒体进度：30 / 600 秒（视频）",
		"2026-11-01 23:34:31  WARN 媒体状态为 waiting，1 秒后重新查询",
		"2026-11-01 23:34:40 ERROR 言溪题库查询失败:",
		"\t剩余查询数5:",
		"\t消息:余额不足",
		"2026-11-01 23:35:00  INFO 完成章节：数学 / 1.1",
		"2026-11-01 23:35:01 ERROR 章节失败：数学 / 1.2: 超时",
		"有 1 个章节未完成，请查看日志",
		"",
	}, "\n"))

	update := log.Since(0)

	want := []LogLine{
		{Time: "2026-11-01 23:33:58", Level: "info", Text: "共读取到 3 个章节任务"},
		{Time: "2026-11-01 23:33:59", Level: "info", Kind: "start", Text: "开始任务：数学 / 1.1 / 视频"},
		{Time: "2026-11-01 23:34:30", Level: "info", Kind: "progress", Text: "媒体进度：30 / 600 秒（视频）"},
		{Time: "2026-11-01 23:34:31", Level: "warn", Text: "媒体状态为 waiting，1 秒后重新查询"},
		{Time: "2026-11-01 23:34:40", Level: "error", Text: "言溪题库查询失败:\n\t剩余查询数5:\n\t消息:余额不足"},
		{Time: "2026-11-01 23:35:00", Level: "info", Kind: "done", Text: "完成章节：数学 / 1.1"},
		{Time: "2026-11-01 23:35:01", Level: "error", Text: "章节失败：数学 / 1.2: 超时"},
		{Text: "有 1 个章节未完成，请查看日志"},
	}
	if !reflect.DeepEqual(update.Lines, want) {
		t.Fatalf("lines =\n%#v\nwant\n%#v", update.Lines, want)
	}
	if update.Next != len(want) {
		t.Fatalf("next = %d, want %d", update.Next, len(want))
	}
	if update.Total != 3 || update.Done != 1 || update.Failed != 1 || update.Warns != 1 || update.Errors != 2 {
		t.Fatalf("totals = %+v", update)
	}
}

func TestLogBufferReturnsOnlyLinesAfterTheCursor(t *testing.T) {
	log := newLog(t, "2026-11-01 23:33:58  INFO 一\n2026-11-01 23:33:59  INFO 二\n")
	first := log.Since(0)
	log.Write([]byte("2026-11-01 23:34:00  INFO 三\n"))

	update := log.Since(first.Next)

	if len(update.Lines) != 1 || update.Lines[0].Text != "三" || update.Next != 3 {
		t.Fatalf("update = %+v", update)
	}
	if empty := log.Since(update.Next); empty.Lines == nil || len(empty.Lines) != 0 {
		t.Fatalf("an up-to-date window should get an empty, non-nil list (JSON null breaks the page): %#v", empty.Lines)
	}
	if beyond := log.Since(99); len(beyond.Lines) != 0 || beyond.Next != 3 {
		t.Fatalf("a cursor past the end = %+v", beyond)
	}
}

func TestLogBufferJoinsLinesSplitAcrossWrites(t *testing.T) {
	var log logBuffer
	log.Reset()
	log.Write([]byte("2026-11-01 23:33:58  INFO 前"))
	if got := log.Since(0); len(got.Lines) != 0 {
		t.Fatalf("an unfinished line must wait for its newline: %#v", got.Lines)
	}
	log.Write([]byte("半 后半\r\n2026-11-01 23:33:59  INFO 没有换行"))
	log.Flush()

	update := log.Since(0)

	if len(update.Lines) != 2 || update.Lines[0].Text != "前半 后半" || update.Lines[1].Text != "没有换行" {
		t.Fatalf("lines = %#v", update.Lines)
	}
}

func TestLogBufferDropsColorCodes(t *testing.T) {
	log := newLog(t, "\x1b[2m2026-11-01 23:33:58\x1b[0m \x1b[32m INFO\x1b[0m 带颜色\n")

	line := log.Since(0).Lines[0]

	if line.Time != "2026-11-01 23:33:58" || line.Level != "info" || line.Text != "带颜色" {
		t.Fatalf("line = %#v", line)
	}
}

func TestLogBufferKeepsTotalsAndCursorsWhenOldLinesAreDropped(t *testing.T) {
	var log logBuffer
	log.Reset()
	count := maxLogLines + logTrimSlack + 20
	for i := 0; i < count; i++ {
		log.Write([]byte(fmt.Sprintf("2026-11-01 23:33:58  INFO 完成章节：第 %d 章\n", i)))
	}

	all := log.Since(0)

	if all.Next != count {
		t.Fatalf("next = %d, want %d (cursors count from the start of the run)", all.Next, count)
	}
	if len(all.Lines) > maxLogLines+logTrimSlack || len(all.Lines) < maxLogLines {
		t.Fatalf("kept %d lines, want between %d and %d", len(all.Lines), maxLogLines, maxLogLines+logTrimSlack)
	}
	if last := all.Lines[len(all.Lines)-1].Text; last != fmt.Sprintf("完成章节：第 %d 章", count-1) {
		t.Fatalf("last line = %q", last)
	}
	if all.Done != count {
		t.Fatalf("done = %d, want %d: dropping old lines must not change the totals", all.Done, count)
	}
	if recent := log.Since(count - 3); len(recent.Lines) != 3 {
		t.Fatalf("recent lines = %d, want 3", len(recent.Lines))
	}
}

func TestLogBufferResetStartsANewRun(t *testing.T) {
	log := newLog(t, "2026-11-01 23:33:58  WARN 旧的\n")
	before := log.Since(0)

	log.Reset()
	after := log.Since(before.Next)

	if after.Run == before.Run {
		t.Fatal("the window needs a new run number to know it must start over")
	}
	if len(after.Lines) != 0 || after.Warns != 0 || after.Next != 0 {
		t.Fatalf("after reset = %+v", after)
	}
}

func TestLogBufferFailureSummary(t *testing.T) {
	tests := []struct{ name, output, want string }{
		{"plain message the core prints on exit", "2026-11-01 23:33:58 ERROR 章节失败：a: b\n登录失败: 密码错误\n", "登录失败: 密码错误"},
		{"several plain lines are kept together", "2026-11-01 23:33:58  INFO x\npanic\n  at src/main.rs:1\n", "panic\n  at src/main.rs:1"},
		{"falls back to the last error", "2026-11-01 23:33:58 ERROR 第一个\n2026-11-01 23:33:59 ERROR 第二个\n2026-11-01 23:34:00  INFO 结束\n", "第二个"},
		{"nothing to report", "2026-11-01 23:33:58  INFO 一切正常\n", ""},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := newLog(t, test.output).Failure(); got != test.want {
				t.Fatalf("Failure() = %q, want %q", got, test.want)
			}
		})
	}
}

func TestLogBufferTextMatchesTheCoreFormat(t *testing.T) {
	output := "2026-11-01 23:33:58  INFO 开始\n2026-11-01 23:33:59 ERROR 出错:\n\t详情\n单独一行\n"

	if got := newLog(t, output).Text(); got != output {
		t.Fatalf("Text() =\n%q\nwant\n%q", got, output)
	}
}

func TestProgressUpdatesAreIndependentAndDoNotAppendLogLines(t *testing.T) {
	var log logBuffer
	log.Reset()
	write := func(id string, position int, state string) {
		output := fmt.Sprintf("2026-11-02 12:00:00 INFO [progress] {\"id\":%q,\"label\":\"同名视频\",\"position\":%d,\"total\":100,\"state\":%q}\n", id, position, state)
		// Exercise a JSON event arriving across pipe writes.
		log.Write([]byte(output[:45]))
		log.Write([]byte(output[45:]))
	}
	write("1", 10, "running")
	write("2", 20, "running")
	first := log.Since(0)
	for i := 11; i <= 80; i++ {
		write("1", i, "running")
	}
	update := log.Since(first.Next)
	if len(update.Lines) != 0 || update.Next != 0 || len(update.Progress) != 2 || update.Progress[0].Position != 80 || update.Progress[1].Position != 20 {
		t.Fatalf("progress must replace by ID without advancing log cursor: %+v", update)
	}
	if first.Progress[0].Position != 10 {
		t.Fatal("snapshot changed after subsequent writes")
	}
	write("1", 100, "done")
	update = log.Since(0)
	if len(update.Progress) != 1 || update.Progress[0].ID != "2" || len(update.Lines) != 1 || update.Done != 0 {
		t.Fatalf("finishing a media task must leave the other task and chapter totals alone: %+v", update)
	}
	log.EndProgress()
	if len(log.Since(0).Progress) != 0 {
		t.Fatal("stopped process left progress running")
	}
	write("3", 0, "running")
	log.Reset()
	if len(log.Since(0).Progress) != 0 {
		t.Fatal("new run retained old task progress")
	}
}

func TestInvalidProgressRemainsVisibleAsLogOutput(t *testing.T) {
	log := newLog(t, "2026-11-02 12:00:00 INFO [progress] {broken json}\n")
	update := log.Since(0)
	if len(update.Lines) != 1 || len(update.Progress) != 0 {
		t.Fatalf("invalid event was lost: %+v", update)
	}
}

func TestUIEventsBuildCoursesAndChapters(t *testing.T) {
	log := newLog(t, strings.Join([]string{
		"2026-11-02 12:00:00  INFO [ui] {\"type\":\"course\",\"course\":\"高数\",\"clazz\":\"101\",\"chapters\":2}",
		"2026-11-02 12:00:01  INFO [ui] {\"type\":\"chapter\",\"course\":\"高数\",\"clazz\":\"101\",\"chapter\":\"1.1 极限\",\"state\":\"start\"}",
		"2026-11-02 12:00:02  WARN 章节未开放：高数 / 1.1 极限",
		"2026-11-02 12:00:03  INFO [ui] {\"type\":\"answer\",\"course\":\"高数\",\"clazz\":\"101\",\"chapter\":\"1.1 极限\",\"index\":1,\"total\":2,\"kind\":\"单选题\",\"title\":\"求极限\",\"source\":\"言溪题库\",\"hit\":true,\"answer\":\"A\",\"submitted\":true}",
		"2026-11-02 12:00:04  INFO [ui] {\"type\":\"answer\",\"course\":\"高数\",\"clazz\":\"101\",\"chapter\":\"1.1 极限\",\"index\":2,\"total\":2,\"kind\":\"判断题\",\"title\":\"连续\",\"source\":\"随机兜底\",\"hit\":false,\"answer\":\"正确\",\"submitted\":false}",
		"2026-11-02 12:00:05  INFO [ui] {\"type\":\"work\",\"course\":\"高数\",\"clazz\":\"101\",\"chapter\":\"1.1 极限\",\"questions\":2,\"covered\":1,\"submitted\":false}",
		"2026-11-02 12:00:06  INFO [ui] {\"type\":\"chapter\",\"course\":\"高数\",\"clazz\":\"101\",\"chapter\":\"1.1 极限\",\"state\":\"done\"}",
	}, "\n"))

	update := log.Since(0)
	if len(update.Lines) != 1 || !strings.HasPrefix(update.Lines[0].Text, "章节未开放") {
		t.Fatalf("ui events must not show as log rows: %#v", update.Lines)
	}
	if len(update.Courses) != 1 || update.Courses[0].Chapters != 2 || update.Courses[0].Done != 1 || update.Courses[0].Warnings != 1 {
		t.Fatalf("courses = %+v", update.Courses)
	}
	if len(update.Chapters) != 1 {
		t.Fatalf("chapters = %+v", update.Chapters)
	}
	chapter := update.Chapters[0]
	if chapter.State != "done" || chapter.Questions != 2 || chapter.Covered != 1 || chapter.Submitted || chapter.Warnings != 1 || len(chapter.Answers) != 2 {
		t.Fatalf("chapter = %+v", chapter)
	}
	if chapter.Answers[0].Source != "言溪题库" || !chapter.Answers[0].Hit || chapter.Answers[0].Time != "12:00:03" || chapter.Answers[1].Hit {
		t.Fatalf("answers = %+v", chapter.Answers)
	}
}

func TestCourseNeedsEveryChapterBeforeItIsDone(t *testing.T) {
	log := newLog(t, strings.Join([]string{
		"2026-11-02 12:00:00  INFO [ui] {\"type\":\"course\",\"course\":\"英语\",\"clazz\":\"1\",\"chapters\":2}",
		"2026-11-02 12:00:01  INFO [ui] {\"type\":\"chapter\",\"course\":\"英语\",\"clazz\":\"1\",\"chapter\":\"Unit 1\",\"state\":\"done\"}",
	}, "\n"))
	if state := log.Since(0).Courses[0].State; state != "running" {
		t.Fatalf("partly finished course state = %q", state)
	}
	log.Write([]byte("2026-11-02 12:00:02  INFO [ui] {\"type\":\"chapter\",\"course\":\"英语\",\"clazz\":\"1\",\"chapter\":\"Unit 2\",\"state\":\"done\"}\n"))
	update := log.Since(0)
	if update.Courses[0].State != "done" || update.Courses[0].Done != 2 {
		t.Fatalf("finished course = %+v", update.Courses[0])
	}
}
