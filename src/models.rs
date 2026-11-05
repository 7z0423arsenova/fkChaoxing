//! 课程、章节、任务点与题目的数据结构。

use serde_json::Value;

/// 课程列表中的一门课程（同一 courseId 可能对应多个班级）。
#[derive(Debug, Clone, Default)]
pub struct Course {
    pub id: String,
    pub info: String,
    pub roleid: String,
    pub clazz_id: String,
    pub course_id: String,
    pub cpi: String,
    pub title: String,
    pub desc: String,
    pub teacher: String,
}

/// 课程中的一个章节（知识点）。
#[derive(Debug, Clone, Default)]
pub struct Point {
    pub id: String,
    pub title: String,
    pub job_count: String,
    pub has_finished: bool,
    pub need_unlock: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CoursePoints {
    /// 课程中是否存在需要解锁的章节
    pub has_locked: bool,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    Video,
    Document,
    Work,
    Read,
    Live,
}

/// 章节中的一个任务点。
#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobType,
    /// 卡片明确标记为音频任务；进度上报优先使用 `dtype=Audio`。
    pub is_audio: bool,
    pub jobid: String,
    pub name: String,
    pub otherinfo: String,
    pub mid: String,
    pub objectid: String,
    pub aid: String,
    pub jtoken: String,
    pub enc: String,
    /// 已播放时长（毫秒）
    pub play_time_ms: i64,
    pub rt: String,
    pub att_duration: String,
    pub att_duration_enc: String,
    pub video_face_capture_enc: String,
    /// 原始 property 字段（直播任务需要）
    pub property: Value,
}

impl Job {
    pub fn new(kind: JobType) -> Self {
        Self {
            kind,
            is_audio: false,
            jobid: String::new(),
            name: String::new(),
            otherinfo: String::new(),
            mid: String::new(),
            objectid: String::new(),
            aid: String::new(),
            jtoken: String::new(),
            enc: String::new(),
            play_time_ms: 0,
            rt: String::new(),
            att_duration: String::new(),
            att_duration_enc: String::new(),
            video_face_capture_enc: String::new(),
            property: Value::Null,
        }
    }

    /// 用于日志展示的任务名称
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.jobid
        } else {
            &self.name
        }
    }
}

/// 任务点卡片页中的公共参数（mArg.defaults）。
#[derive(Debug, Clone, Default)]
pub struct JobInfo {
    pub not_open: bool,
    /// 是否包含 defaults 数据
    pub present: bool,
    pub ktoken: String,
    /// 卡片页原始参数，保留供后续协议扩展使用。
    #[allow(dead_code)]
    pub mt_enc: String,
    #[allow(dead_code)]
    pub report_time_interval: i64,
    #[allow(dead_code)]
    pub defenc: String,
    #[allow(dead_code)]
    pub cardid: String,
    pub cpi: String,
    #[allow(dead_code)]
    pub qnenc: String,
    pub knowledgeid: String,
}

impl JobInfo {
    pub fn not_open() -> Self {
        Self {
            not_open: true,
            ..Default::default()
        }
    }

    /// 合并多张卡片页的参数，后读到的有效数据覆盖之前的。
    pub fn merge(&mut self, other: JobInfo) {
        if other.present {
            let not_open = self.not_open || other.not_open;
            *self = other;
            self.not_open = not_open;
        } else if other.not_open {
            self.not_open = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuestionType {
    Single,
    Multiple,
    Completion,
    Judgement,
    ShortAnswer,
    Unknown,
}

impl QuestionType {
    pub fn from_code(code: &str) -> Self {
        match code.trim() {
            "0" => Self::Single,
            "1" => Self::Multiple,
            "2" => Self::Completion,
            "3" => Self::Judgement,
            "4" => Self::ShortAnswer,
            _ => Self::Unknown,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Single => "0",
            Self::Multiple => "1",
            Self::Completion => "2",
            Self::Judgement => "3",
            Self::ShortAnswer => "4",
            Self::Unknown => "",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::Single => "单选题",
            Self::Multiple => "多选题",
            Self::Completion => "填空题",
            Self::Judgement => "判断题",
            Self::ShortAnswer => "简答题",
            Self::Unknown => "其他类型",
        }
    }

    pub fn is_choice(self) -> bool {
        matches!(self, Self::Single | Self::Multiple)
    }
}

/// 章节检测中的一道题目。
#[derive(Debug, Clone)]
pub struct Question {
    pub id: String,
    pub title: String,
    /// 选项按字典序排序后以换行连接，例如 "A. xxx\nB. yyy"
    pub options: String,
    pub q_type: QuestionType,
    /// 提交表单中的 answertype 值（原始题型代码）
    pub type_code: String,
}

impl Question {
    pub fn new(title: impl Into<String>, q_type: QuestionType, options: &[String]) -> Self {
        Self {
            id: String::new(),
            title: title.into(),
            options: options.join("\n"),
            q_type,
            type_code: q_type.code().to_owned(),
        }
    }

    /// 非空的选项行
    pub fn option_lines(&self) -> Vec<&str> {
        self.options
            .split('\n')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// 将 JSON 值转为字符串参数（字符串原样返回，数字按原样格式化，null 为空串）。
pub fn json_str(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// 把接口中的非空 JSON 值转为字符串。
pub fn json_truthy_str(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let truthy = match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    };
    truthy.then(|| json_str(value))
}

/// 接口值是否表示已设置。
pub fn json_truthy(value: Option<&Value>) -> bool {
    json_truthy_str(value).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_helpers_handle_empty_values() {
        assert_eq!(json_str(&json!(123)), "123");
        assert_eq!(json_str(&json!(0.9)), "0.9");
        assert_eq!(json_str(&json!("x")), "x");
        assert_eq!(json_str(&json!(true)), "true");
        assert_eq!(json_str(&Value::Null), "");
        assert_eq!(json_truthy_str(Some(&json!(0))), None);
        assert_eq!(json_truthy_str(Some(&json!(""))), None);
        assert_eq!(json_truthy_str(Some(&json!("0"))), Some("0".into()));
        assert_eq!(json_truthy_str(Some(&json!(30))), Some("30".into()));
        assert!(!json_truthy(None));
    }

    #[test]
    fn job_info_merge_keeps_latest_defaults() {
        let mut info = JobInfo::default();
        info.merge(JobInfo {
            present: true,
            knowledgeid: "1".into(),
            ..Default::default()
        });
        info.merge(JobInfo::default());
        assert_eq!(info.knowledgeid, "1");
        info.merge(JobInfo {
            present: true,
            knowledgeid: "2".into(),
            ..Default::default()
        });
        assert_eq!(info.knowledgeid, "2");
        assert!(!info.not_open);
    }
}
