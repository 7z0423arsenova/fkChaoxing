//! 终端输出协调：日志与交互式输入。
//!
//! 多个章节并发执行时，日志会同时输出；当需要用户在终端输入（手动答题、
//! 启动时的账号输入等）时，必须缓存后台日志，输入结束后再统一输出，
//! 否则提示信息会被刷屏淹没。

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU64, Ordering},
};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

pub static CONSOLE: LazyLock<Console> = LazyLock::new(Console::new);

static NEXT_PROGRESS_ID: AtomicU64 = AtomicU64::new(1);

/// 每个并发任务持有自己的进度条；非终端输出使用结构化事件供桌面端更新。
pub struct TaskProgress {
    id: u64,
    label: String,
    bar: ProgressBar,
    finished: bool,
}

impl TaskProgress {
    pub fn new(label: String, total: u64) -> Self {
        let bar = {
            let _state = CONSOLE.state();
            CONSOLE.multi.add(ProgressBar::new(total))
        };
        bar.set_style(
            ProgressStyle::with_template(
                "{prefix:.green.bold} [{bar:28.cyan/blue}] {pos}/{len} {msg}",
            )
            .expect("valid progress template")
            .progress_chars("=>-"),
        );
        bar.set_prefix("学习中");
        bar.set_message(label.clone());
        let progress = Self {
            id: NEXT_PROGRESS_ID.fetch_add(1, Ordering::Relaxed),
            label,
            bar,
            finished: false,
        };
        progress.emit("running");
        progress
    }

    pub fn update(&self, position: u64, total: u64) {
        if self.bar.length() == Some(total) && self.bar.position() == position.min(total) {
            return;
        }
        self.bar.set_length(total);
        self.bar.set_position(position.min(total));
        self.emit("running");
    }

    pub fn finish(&mut self, success: bool) {
        if success {
            self.bar.set_position(self.bar.length().unwrap_or(0));
        }
        self.finished = true;
        self.emit(if success { "done" } else { "failed" });
        self.bar.finish_and_clear();
        CONSOLE.multi.remove(&self.bar);
    }

    fn emit(&self, state: &str) {
        // TTY 由 indicatif 原地刷新；管道中不能发送 ANSI 光标控制序列。
        if !io::stderr().is_terminal() {
            let event = serde_json::json!({
                "id": self.id.to_string(), "label": self.label,
                "position": self.bar.position(), "total": self.bar.length().unwrap_or(0),
                "state": state,
            });
            tracing::info!("[progress] {event}");
        }
    }
}

impl Drop for TaskProgress {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("failed");
            self.bar.finish_and_clear();
            CONSOLE.multi.remove(&self.bar);
        }
    }
}

/// 交互期间最多缓存的日志条数。
const MAX_BUFFERED_LOGS: usize = 1000;

pub struct Console {
    multi: MultiProgress,
    state: Mutex<State>,
    interactive: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    paused: bool,
    buffer: Vec<Vec<u8>>,
}

impl Console {
    fn new() -> Self {
        Self {
            multi: MultiProgress::with_draw_target(ProgressDrawTarget::stderr()),
            state: Mutex::new(State::default()),
            interactive: tokio::sync::Mutex::new(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 输出一条格式化好的日志。交互期间先缓存，结束后补发。
    pub fn write_log(&self, line: &[u8]) {
        let mut state = self.state();
        if state.paused {
            if state.buffer.len() < MAX_BUFFERED_LOGS {
                state.buffer.push(line.to_vec());
            }
            return;
        }
        let pending = std::mem::take(&mut state.buffer);
        self.multi.suspend(|| {
            let mut err = io::stderr().lock();
            for buffered in pending {
                let _ = err.write_all(&buffered);
            }
            let _ = err.write_all(line);
            let _ = err.flush();
        });
    }

    /// 在独占终端输入的情况下执行 `f`（在阻塞线程池中运行，可以安全地读取标准输入）。
    ///
    /// 执行期间后台日志被缓存；多个任务同时需要输入时按顺序排队。
    pub async fn interact<T, F>(&'static self, f: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let _guard = self.interactive.lock().await;
        self.pause();
        let result = tokio::task::spawn_blocking(f).await;
        self.resume();
        match result {
            Ok(value) => value,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("交互任务被取消: {e}"),
        }
    }

    /// 显示提示并读取一行输入（异步版本）。标准输入关闭时返回 `None`。
    pub async fn prompt(&'static self, message: impl Into<String>) -> Option<String> {
        let message = message.into();
        self.interact(move || read_line(&message)).await
    }

    fn pause(&self) {
        let mut state = self.state();
        state.paused = true;
        let _ = self.multi.clear();
        self.multi.set_draw_target(ProgressDrawTarget::hidden());
    }

    fn resume(&self) {
        self.multi.set_draw_target(ProgressDrawTarget::stderr());
        let mut state = self.state();
        state.paused = false;
        let pending = std::mem::take(&mut state.buffer);
        if !pending.is_empty() {
            self.multi.suspend(|| {
                let mut err = io::stderr().lock();
                for buffered in pending {
                    let _ = err.write_all(&buffered);
                }
                let _ = err.flush();
            });
        }
    }
}

/// 标准输入是否连接到终端（决定能否进行交互式输入）。
pub fn stdin_is_interactive() -> bool {
    io::stdin().is_terminal()
}

/// 阻塞地显示提示并读取一行，去掉行尾换行符。只应在 [`Console::interact`] 中调用。
pub fn read_line(message: &str) -> Option<String> {
    let mut out = io::stdout().lock();
    let _ = out.write_all(message.as_bytes());
    let _ = out.flush();
    drop(out);
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn progress_events_keep_task_identity_and_finish_on_error() {
        if io::stderr().is_terminal() {
            return;
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let capture = Capture(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut first = TaskProgress::new("同名视频".into(), 100);
            let second = TaskProgress::new("同名视频".into(), 100);
            first.update(20, 100);
            second.update(50, 100);
            first.finish(true);
            // Drop represents an error or cancellation during an async task.
            drop(second);
        });
        let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line.split_once("[progress] ").unwrap().1).unwrap())
            .collect();
        assert_eq!(events.len(), 6);
        assert_ne!(events[0]["id"], events[1]["id"]);
        assert_eq!(events[0]["id"], events[2]["id"]);
        assert_eq!(events[1]["id"], events[3]["id"]);
        assert_eq!(events[2]["position"], 20);
        assert_eq!(events[3]["position"], 50);
        assert_eq!(events[4]["state"], "done");
        assert_eq!(events[4]["position"], 100);
        assert_eq!(events[5]["state"], "failed");
        assert_eq!(events[5]["position"], 50);
    }
}
