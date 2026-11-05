//! 本地答案缓存 `cache.json`（题目 -> 答案）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{error, warn};

pub const DEFAULT_CACHE_FILE: &str = "cache.json";

pub struct AnswerCache {
    path: PathBuf,
    data: Mutex<Map<String, Value>>,
}

impl AnswerCache {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = if path.is_file() {
            read_cache_file(&path)
        } else {
            let empty = Map::new();
            if let Err(e) = write_atomic(&path, &empty) {
                warn!("创建缓存文件 {} 失败: {e}", path.display());
            }
            empty
        };
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    pub fn get(&self, question: &str) -> Option<String> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.get(question)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    /// 写入缓存。写入前重新读取文件以合并其他进程的写入，然后原子替换。
    pub fn add(&self, question: &str, answer: &str) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if self.path.is_file() {
            for (k, v) in read_cache_file(&self.path) {
                data.entry(k).or_insert(v);
            }
        }
        data.insert(question.to_owned(), Value::String(answer.to_owned()));
        if let Err(e) = write_atomic(&self.path, &data) {
            error!("写入缓存文件失败: {e}");
        }
    }
}

fn read_cache_file(path: &Path) -> Map<String, Value> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            error!("读取缓存异常: {e}");
            return Map::new();
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    if let Ok(map) = serde_json::from_str::<Map<String, Value>>(&text) {
        return map;
    }
    // 尝试截取首尾大括号之间的内容恢复
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && start < end
        && let Ok(map) = serde_json::from_str::<Map<String, Value>>(&text[start..=end])
    {
        warn!("缓存文件格式异常，已恢复可用部分");
        return map;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut backup = path.as_os_str().to_owned();
    backup.push(format!(".bak.{ts}"));
    match std::fs::copy(path, &backup) {
        Ok(_) => error!(
            "缓存文件已损坏，已备份为 {}，将使用空缓存继续运行",
            Path::new(&backup).display()
        ),
        Err(e) => error!("缓存文件已损坏且备份失败: {e}"),
    }
    Map::new()
}

/// 以 4 空格缩进、保留中文的格式写入临时文件后替换，避免写入中断导致文件损坏。
fn write_atomic(path: &Path, data: &Map<String, Value>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    data.serialize(&mut ser).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chaoxing-cache-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cache_roundtrip_and_format() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("cache.json");
        let cache = AnswerCache::open(&path);
        assert_eq!(cache.get("题目"), None);
        cache.add("题目", "答案");
        assert_eq!(cache.get("题目").as_deref(), Some("答案"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\n    \"题目\": \"答案\"\n}");
        // 重新打开后仍可读取
        assert_eq!(
            AnswerCache::open(&path).get("题目").as_deref(),
            Some("答案")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupted_cache_is_recovered_or_backed_up() {
        let dir = temp_dir("corrupt");
        let path = dir.join("cache.json");
        std::fs::write(&path, "garbage{\"q\": \"a\"}garbage").unwrap();
        assert_eq!(AnswerCache::open(&path).get("q").as_deref(), Some("a"));

        std::fs::write(&path, "not json at all").unwrap();
        let cache = AnswerCache::open(&path);
        assert_eq!(cache.get("q"), None);
        let backups = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak.")
            })
            .count();
        assert_eq!(backups, 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
