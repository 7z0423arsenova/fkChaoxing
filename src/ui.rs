//! 桌面端使用的结构化事件。
//!
//! 只有桌面程序会设置 `FKCHAOXING_UI_EVENTS=1`；普通终端运行不输出这些行。
//! 事件写入标准输出，格式为 `[ui] {json}`，由 Go 桥接层解析成课程、章节和答题记录。

use std::sync::LazyLock;

use serde_json::{Value, json};

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("FKCHAOXING_UI_EVENTS").is_ok_and(|value| value == "1"));

pub fn enabled() -> bool {
    *ENABLED
}

pub fn emit(event: Value) {
    if enabled() {
        println!("[ui] {event}");
    }
}

pub fn course(course: &str, clazz: &str, chapters: usize) {
    emit(json!({
        "type": "course", "course": course, "clazz": clazz, "chapters": chapters,
    }));
}

pub fn chapter(course: &str, clazz: &str, chapter: &str, state: &str) {
    emit(json!({
        "type": "chapter", "course": course, "clazz": clazz, "chapter": chapter, "state": state,
    }));
}

pub fn work(
    course: &str,
    clazz: &str,
    chapter: &str,
    questions: usize,
    covered: usize,
    submitted: bool,
) {
    emit(json!({
        "type": "work", "course": course, "clazz": clazz, "chapter": chapter,
        "questions": questions, "covered": covered, "submitted": submitted,
    }));
}

#[allow(clippy::too_many_arguments)]
pub fn answer(
    course: &str,
    clazz: &str,
    chapter: &str,
    index: usize,
    total: usize,
    kind: &str,
    title: &str,
    source: &str,
    hit: bool,
    answer: &str,
    submitted: bool,
) {
    emit(json!({
        "type": "answer", "course": course, "clazz": clazz, "chapter": chapter,
        "index": index, "total": total, "kind": kind, "title": title,
        "source": source, "hit": hit, "answer": answer, "submitted": submitted,
    }));
}
