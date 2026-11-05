//! 日志初始化：终端输出（与进度条协调）+ `fkChaoxing.log` 文件（TRACE 级别，超过 10MB 自动轮转）。

use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

use crate::console::CONSOLE;

const LOG_FILE: &str = "fkChaoxing.log";
const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// 初始化全局日志。`verbosity`: 0 = INFO，1 = DEBUG，2+ = TRACE。
pub fn init(verbosity: u8) {
    #[cfg(windows)]
    let _ = nu_ansi_term::enable_ansi_support();

    let console_level = match verbosity {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    // 第三方库（reqwest/hyper/html5ever 等）只输出警告以上
    let console_filter = Targets::new()
        .with_target(env!("CARGO_CRATE_NAME"), console_level)
        .with_default(LevelFilter::WARN);
    let console_layer = fmt::layer()
        .with_writer(ConsoleMakeWriter)
        .with_ansi(io::stderr().is_terminal())
        .with_target(false)
        .with_timer(ChronoLocal::new(TIME_FORMAT.to_owned()))
        .with_filter(console_filter);

    let file_layer = match RotatingFile::open(PathBuf::from(LOG_FILE), LOG_ROTATE_BYTES) {
        Ok(file) => {
            let file_filter = Targets::new()
                .with_target(env!("CARGO_CRATE_NAME"), LevelFilter::TRACE)
                .with_default(LevelFilter::WARN);
            Some(
                fmt::layer()
                    .with_writer(file)
                    .with_ansi(false)
                    .with_timer(ChronoLocal::new(TIME_FORMAT.to_owned()))
                    .with_filter(file_filter),
            )
        }
        Err(e) => {
            eprintln!("无法打开日志文件 {LOG_FILE}: {e}，将只输出到终端");
            None
        }
    };

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();
}

/// 每条日志先写入缓冲，在 drop 时整体交给 [`CONSOLE`]，保证与进度条输出不交错。
struct ConsoleMakeWriter;

struct ConsoleLine(Vec<u8>);

impl Write for ConsoleLine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ConsoleLine {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            CONSOLE.write_log(&self.0);
        }
    }
}

impl<'a> MakeWriter<'a> for ConsoleMakeWriter {
    type Writer = ConsoleLine;

    fn make_writer(&'a self) -> Self::Writer {
        ConsoleLine(Vec::with_capacity(256))
    }
}

/// 按大小轮转的日志文件：超过上限时重命名为 `fkChaoxing.log.1` 并新建文件。
struct RotatingFile {
    inner: Mutex<FileState>,
}

struct FileState {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    max_bytes: u64,
}

impl RotatingFile {
    fn open(path: PathBuf, max_bytes: u64) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        // 日志里可能包含题目、答案和课程信息，尽量收紧到仅当前用户可读。
        let _ = crate::fsutil::restrict_permissions(&path);
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Mutex::new(FileState {
                path,
                file: Some(file),
                size,
                max_bytes,
            }),
        })
    }
}

impl FileState {
    fn write_line(&mut self, line: &[u8]) {
        if self.size + line.len() as u64 > self.max_bytes && self.size > 0 {
            self.rotate();
        }
        if let Some(file) = self.file.as_mut()
            && file.write_all(line).is_ok()
        {
            self.size += line.len() as u64;
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let mut backup = self.path.clone().into_os_string();
        backup.push(".1");
        let _ = std::fs::rename(&self.path, PathBuf::from(backup));
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        if self.file.is_some() {
            let _ = crate::fsutil::restrict_permissions(&self.path);
        }
        self.size = 0;
    }
}

struct FileLine<'a> {
    buf: Vec<u8>,
    target: &'a RotatingFile,
}

impl Write for FileLine<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for FileLine<'_> {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let mut state = self.target.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.write_line(&self.buf);
        }
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = FileLine<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        FileLine {
            buf: Vec::with_capacity(256),
            target: self,
        }
    }
}
