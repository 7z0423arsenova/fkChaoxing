//! 解析学习通页面：课程列表、课程文件夹、章节列表、任务点卡片与章节检测题目。

use std::sync::LazyLock;

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::{Map, Value};
use tracing::{debug, trace, warn};

use crate::font::FontDecoder;
use crate::models::{
    Course, CoursePoints, Job, JobInfo, JobType, Point, Question, QuestionType, json_str,
    json_truthy,
};

macro_rules! selector {
    ($name:ident, $css:expr) => {
        static $name: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse($css).expect("valid selector"));
    };
}

selector!(COURSE, "div.course");
selector!(COURSE_NOT_OPEN, "a.not-open-tip, div.not-open-tip");
selector!(INPUT_CLAZZ_ID, "input.clazzId");
selector!(INPUT_COURSE_ID, "input.courseId");
selector!(ANCHOR, "a");
selector!(COURSE_NAME, "span.course-name");
selector!(COURSE_DESC, "p.margint10");
selector!(COURSE_TEACHER, "p.color3");
selector!(FOLDER_ITEM, "ul.file-list > li");
selector!(CHAPTER_UNIT, "div.chapter_unit");
selector!(LI, "li");
selector!(DIV, "div");
selector!(CLICK_TITLE, "a.clicktitle");
selector!(JOB_COUNT, "input.knowledgeJobCount");
selector!(HOVER_TIPS, "span.bntHoverTips");
selector!(FORM, "form");
selector!(INPUT, "input");
selector!(QUESTION, "div.singleQuesId");
selector!(TIMU, "div.TiMu");
selector!(TITLE, "div.Zy_TItle");
selector!(UL, "ul");

static CPI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"cpi=([^&#"']+)"#).unwrap());
static POINT_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^cur(\d{1,20})$").unwrap());
static MARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)mArg\s*=\s*\{(.*?)\}\s*;").unwrap());

fn attr<'a>(el: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    el.value().attr(name)
}

fn first<'a>(el: &ElementRef<'a>, sel: &Selector) -> Option<ElementRef<'a>> {
    el.select(sel).next()
}

fn text_of(el: &ElementRef<'_>) -> String {
    el.text().collect()
}

/// 解析课程列表（`courselistdata` 接口返回的 HTML），跳过未开放的课程。
pub fn parse_course_list(html: &str) -> Vec<Course> {
    trace!("开始解码课程列表...");
    let doc = Html::parse_document(html);
    let mut courses = Vec::new();
    for el in doc.select(&COURSE) {
        if first(&el, &COURSE_NOT_OPEN).is_some() {
            continue;
        }
        let course = (|| {
            Some(Course {
                id: attr(&el, "id")?.to_owned(),
                info: attr(&el, "info").unwrap_or_default().to_owned(),
                roleid: attr(&el, "roleid").unwrap_or_default().to_owned(),
                clazz_id: attr(&first(&el, &INPUT_CLAZZ_ID)?, "value")?.to_owned(),
                course_id: attr(&first(&el, &INPUT_COURSE_ID)?, "value")?.to_owned(),
                cpi: CPI_RE.captures(attr(&first(&el, &ANCHOR)?, "href")?)?[1].to_owned(),
                title: attr(&first(&el, &COURSE_NAME)?, "title")?.to_owned(),
                desc: first(&el, &COURSE_DESC)
                    .and_then(|d| attr(&d, "title"))
                    .unwrap_or_default()
                    .to_owned(),
                teacher: first(&el, &COURSE_TEACHER)
                    .and_then(|t| attr(&t, "title"))
                    .unwrap_or_default()
                    .to_owned(),
            })
        })();
        match course {
            Some(course) => courses.push(course),
            None => warn!("跳过一条无法解析的课程信息"),
        }
    }
    courses
}

/// 课程文件夹（二级课程列表）
#[derive(Debug, Clone)]
pub struct CourseFolder {
    pub id: String,
}

/// 解析二级课程文件夹列表。
pub fn parse_course_folders(html: &str) -> Vec<CourseFolder> {
    trace!("开始解码二级课程列表...");
    let doc = Html::parse_document(html);
    doc.select(&FOLDER_ITEM)
        .filter_map(|li| {
            let id = attr(&li, "fileid").filter(|s| !s.is_empty())?;
            Some(CourseFolder { id: id.to_owned() })
        })
        .collect()
}

/// 解析课程章节列表（`studentcourse` 页面）。
pub fn parse_course_points(html: &str) -> CoursePoints {
    trace!("开始解码章节列表...");
    let doc = Html::parse_document(html);
    let mut result = CoursePoints::default();
    for unit in doc.select(&CHAPTER_UNIT) {
        for li in unit.select(&LI) {
            let Some(div) = first(&li, &DIV) else {
                continue;
            };
            let Some(id) = attr(&div, "id")
                .and_then(|id| POINT_ID_RE.captures(id))
                .map(|c| c[1].to_owned())
            else {
                continue;
            };
            let title = first(&div, &CLICK_TITLE)
                .map(|a| text_of(&a).replace('\n', "").trim().to_owned())
                .unwrap_or_default();
            let tips = first(&div, &HOVER_TIPS)
                .map(|s| text_of(&s))
                .unwrap_or_default();
            let job_count_input = first(&div, &JOB_COUNT).and_then(|i| attr(&i, "value"));
            let need_unlock = job_count_input.is_none() && tips.contains("解锁");
            let point = Point {
                id,
                title,
                job_count: job_count_input.unwrap_or("1").to_owned(),
                has_finished: tips.contains("已完成"),
                need_unlock,
            };
            result.has_locked |= point.need_unlock;
            result.points.push(point);
        }
    }
    result
}

/// 解析任务点卡片页（`knowledge/cards`），返回未完成的任务点和公共参数。
pub fn parse_chapter_jobs(html: &str) -> (Vec<Job>, JobInfo) {
    trace!("开始解码任务点列表...");
    if html.contains("章节未开放") {
        return (Vec::new(), JobInfo::not_open());
    }
    let Some(data) = extract_marg_json(html) else {
        debug!("任务点页面缺少 mArg，可能没有更多任务点或页面结构已变化");
        return (Vec::new(), JobInfo::default());
    };
    if data.is_empty() {
        return (Vec::new(), JobInfo::default());
    }
    let job_info = data
        .get("defaults")
        .and_then(Value::as_object)
        .filter(|d| !d.is_empty())
        .map(parse_job_info)
        .unwrap_or_default();
    let cards = data
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    (parse_attachment_jobs(cards), job_info)
}

/// 提取页面脚本中的 `mArg = {...};`，保留 JSON 字符串中的空格。
fn extract_marg_json(html: &str) -> Option<Map<String, Value>> {
    let parse =
        |inner: &str| serde_json::from_str::<Map<String, Value>>(&format!("{{{inner}}}")).ok();
    let caps = MARG_RE.captures(html)?;
    match parse(&caps[1]) {
        Some(map) => Some(map),
        None => {
            warn!("任务点数据 mArg 解析失败");
            None
        }
    }
}

/// 把任务点卡片页的 `defaults` 转成后续请求需要的公共参数。
fn parse_job_info(defaults: &Map<String, Value>) -> JobInfo {
    let s = |key: &str| defaults.get(key).map(json_str).unwrap_or_default();
    JobInfo {
        not_open: false,
        present: true,
        ktoken: s("ktoken"),
        mt_enc: s("mtEnc"),
        report_time_interval: defaults
            .get("reportTimeInterval")
            .and_then(Value::as_i64)
            .unwrap_or(60),
        defenc: s("defenc"),
        cardid: s("cardid"),
        cpi: s("cpi"),
        qnenc: s("qnenc"),
        knowledgeid: s("knowledgeid"),
    }
}

/// 把 `attachments` 中尚未完成的任务点转成 `Job`。
fn parse_attachment_jobs(cards: Vec<Value>) -> Vec<Job> {
    let mut jobs = Vec::new();
    for mut card in cards {
        let Some(obj) = card.as_object_mut() else {
            continue;
        };
        if json_truthy(obj.get("isPassed")) {
            continue;
        }
        let card_type = obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        // 阅读任务的 job 字段有时为 true，不能只在 job 为空时识别。
        if card_type == "read" {
            if let Some(job) = process_read_task(obj) {
                jobs.push(job);
            }
            continue;
        }
        if obj.get("job").is_none_or(Value::is_null) {
            continue;
        }
        // otherInfo 中 & 之后的参数会干扰后续请求拼接，统一截断
        if let Some(Value::String(other)) = obj.get_mut("otherInfo")
            && let Some(pos) = other.find('&')
        {
            other.truncate(pos);
        }
        let property = obj.get("property").cloned().unwrap_or(Value::Null);
        let prop_str = |key: &str| {
            property
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
        };
        let has_prop = |key: &str| property.get(key).is_some_and(|v| !v.is_null());
        let is_live = card_type.contains("live")
            || prop_str("type").contains("live")
            || prop_str("resourceType").contains("live")
            || has_prop("liveId")
            || has_prop("streamName")
            || has_prop("vdoid");

        let job = if is_live {
            Some(process_live_task(obj))
        } else {
            match card_type.as_str() {
                // 音频任务与视频共用进度接口，重放时用 dtype=Audio 区分。
                "video" | "audio" => process_video_task(obj).map(|mut job| {
                    job.is_audio = card_type == "audio";
                    job
                }),
                "document" => Some(process_document_task(obj)),
                "workid" => Some(process_work_task(obj)),
                _ => {
                    warn!("未知的任务点类型: {card_type}");
                    trace!(?obj, "未知任务点数据");
                    None
                }
            }
        };
        jobs.extend(job);
    }
    jobs
}

fn get_str(obj: &Map<String, Value>, key: &str) -> String {
    obj.get(key).map(json_str).unwrap_or_default()
}

fn prop_get_str(obj: &Map<String, Value>, key: &str) -> String {
    obj.get("property")
        .and_then(|p| p.get(key))
        .map(json_str)
        .unwrap_or_default()
}

fn process_live_task(obj: &Map<String, Value>) -> Job {
    let mut job = Job::new(JobType::Live);
    job.jobid = obj
        .get("jobid")
        .map(json_str)
        .unwrap_or_else(|| get_str(obj, "id"));
    let property = obj.get("property").cloned().unwrap_or(Value::Null);
    job.name = property
        .get("title")
        .or_else(|| property.get("name"))
        .map(json_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未知直播".to_owned());
    job.otherinfo = get_str(obj, "otherInfo");
    job.mid = get_str(obj, "mid");
    job.objectid = get_str(obj, "objectId");
    job.aid = get_str(obj, "aid");
    job.property = property;
    job
}

fn process_read_task(obj: &Map<String, Value>) -> Option<Job> {
    let is_read = obj.get("type").and_then(Value::as_str) == Some("read");
    let already_read = obj
        .get("property")
        .and_then(|p| p.get("read"))
        .is_some_and(|v| json_truthy(Some(v)));
    if !is_read || already_read {
        return None;
    }
    let mut job = Job::new(JobType::Read);
    job.name = prop_get_str(obj, "title");
    job.jobid = get_str(obj, "jobid");
    job.jtoken = get_str(obj, "jtoken");
    job.mid = get_str(obj, "mid");
    job.otherinfo = get_str(obj, "otherInfo");
    job.enc = get_str(obj, "enc");
    job.aid = get_str(obj, "aid");
    Some(job)
}

fn process_video_task(obj: &Map<String, Value>) -> Option<Job> {
    let Some(mid) = obj.get("mid") else {
        warn!("出现转码失败视频，已跳过...");
        return None;
    };
    let mut job = Job::new(JobType::Video);
    job.jobid = get_str(obj, "jobid");
    job.name = prop_get_str(obj, "name");
    job.otherinfo = get_str(obj, "otherInfo");
    job.mid = json_str(mid);
    job.objectid = get_str(obj, "objectId");
    job.aid = get_str(obj, "aid");
    job.play_time_ms = obj
        .get("playTime")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_f64().map(|f| f as i64))
                .or_else(|| v.as_str()?.parse().ok())
        })
        .unwrap_or(0);
    job.rt = prop_get_str(obj, "rt");
    job.att_duration = get_str(obj, "attDuration");
    job.att_duration_enc = get_str(obj, "attDurationEnc");
    job.video_face_capture_enc = get_str(obj, "videoFaceCaptureEnc");
    job.property = obj.get("property").cloned().unwrap_or(Value::Null);
    Some(job)
}

fn process_document_task(obj: &Map<String, Value>) -> Job {
    let mut job = Job::new(JobType::Document);
    job.jobid = get_str(obj, "jobid");
    job.name = prop_get_str(obj, "name");
    job.otherinfo = get_str(obj, "otherInfo");
    job.jtoken = get_str(obj, "jtoken");
    job.mid = get_str(obj, "mid");
    job.enc = get_str(obj, "enc");
    job.aid = get_str(obj, "aid");
    job.objectid = prop_get_str(obj, "objectid");
    job
}

fn process_work_task(obj: &Map<String, Value>) -> Job {
    let mut job = Job::new(JobType::Work);
    job.jobid = get_str(obj, "jobid");
    job.name = prop_get_str(obj, "title");
    job.otherinfo = get_str(obj, "otherInfo");
    job.mid = get_str(obj, "mid");
    job.enc = get_str(obj, "enc");
    job.aid = get_str(obj, "aid");
    job
}

/// 章节检测页面中的提交表单与题目列表。
#[derive(Debug, Clone, Default)]
pub struct WorkForm {
    /// 表单中的隐藏字段（不含答案字段），保持页面中的顺序
    pub fields: Vec<(String, String)>,
    pub questions: Vec<Question>,
}

impl WorkForm {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// 设置字段，已存在时原位替换
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.fields.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.fields.push((key, value)),
        }
    }
}

/// 解析章节检测题目页面（`api/work`）。
pub fn parse_work_form(html: &str) -> WorkForm {
    let doc = Html::parse_document(html);
    let Some(form) = doc.select(&FORM).next() else {
        return WorkForm::default();
    };

    let mut work = WorkForm::default();
    for input in form.select(&INPUT) {
        let Some(name) = attr(&input, "name").filter(|n| !n.is_empty() && !n.contains("answer"))
        else {
            continue;
        };
        work.set(name, attr(&input, "value").unwrap_or_default());
    }

    let decoder = match FontDecoder::from_html(&doc) {
        Ok(Some(decoder)) => Some(decoder),
        Ok(None) => {
            warn!("未找到字体文件，可能是未加密的题目不进行解密");
            None
        }
        Err(e) => {
            warn!("初始化字体映射失败，题目将不进行解密: {e:#}");
            None
        }
    };

    for div in form.select(&QUESTION) {
        let id = attr(&div, "data").unwrap_or_default().to_owned();
        let type_code = first(&div, &TIMU)
            .and_then(|t| attr(&t, "data"))
            .unwrap_or_default()
            .to_owned();
        let q_type = QuestionType::from_code(&type_code);
        if q_type == QuestionType::Unknown {
            tracing::info!("未知题型代码 -> {type_code}");
        }
        let title = first(&div, &TITLE)
            .map(|t| parse_question_title(&t, decoder.as_ref()))
            .unwrap_or_default();
        let mut options: Vec<String> = first(&div, &UL)
            .map(|ul| {
                ul.select(&LI)
                    .map(|li| parse_choice_option(&li, decoder.as_ref()))
                    .collect()
            })
            .unwrap_or_default();
        options.sort();
        work.questions.push(Question {
            id,
            title,
            options: options.join("\n"),
            q_type,
            type_code,
        });
    }

    let ids: Vec<&str> = work.questions.iter().map(|q| q.id.as_str()).collect();
    work.set("answerwqbid", format!("{},", ids.join(",")));
    work
}

/// 提取题干文本：保留图片为 `<img src="...">`，去除换行与制表符，并解密字体。
fn parse_question_title(el: &ElementRef<'_>, decoder: Option<&FontDecoder>) -> String {
    let mut content = String::new();
    for node in el.descendants() {
        match node.value() {
            Node::Text(text) => content.push_str(text),
            Node::Element(e) if e.name() == "img" => {
                content.push_str(&format!(
                    "<img src=\"{}\">",
                    e.attr("src").unwrap_or_default()
                ));
            }
            _ => {}
        }
    }
    let cleaned: String = content
        .chars()
        .filter(|c| !matches!(c, '\r' | '\t' | '\n'))
        .collect();
    match decoder {
        Some(d) => d.decode(&cleaned),
        None => cleaned,
    }
}

/// 提取选项文本：优先使用 aria-label，去掉末尾的"选择"。
fn parse_choice_option(li: &ElementRef<'_>, decoder: Option<&FontDecoder>) -> String {
    let raw = attr(li, "aria-label")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| text_of(li));
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '\r' | '\t' | '\n'))
        .collect();
    let decoded = match decoder {
        Some(d) => d.decode(&cleaned),
        None => cleaned,
    };
    let trimmed = decoded.trim();
    trimmed
        .strip_suffix("选择")
        .map(str::trim_end)
        .unwrap_or(trimmed)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn course_list_skips_not_open_courses() {
        let html = r#"
        <div class="course" id="c1" info="i1" roleid="3">
          <input class="clazzId" value="111"><input class="courseId" value="222">
          <a href="https://mooc1.chaoxing.com/visit/stucoursemiddle?courseid=222&clazzid=111&cpi=333&ismooc2=1">x</a>
          <span class="course-name" title="大学英语"></span>
          <p class="margint10" title="描述"></p><p class="color3" title="张老师"></p>
        </div>
        <div class="course" id="c2" info="" roleid="3">
          <a class="not-open-tip">未开放</a>
        </div>"#;
        let courses = parse_course_list(html);
        assert_eq!(courses.len(), 1);
        let c = &courses[0];
        assert_eq!(
            (c.clazz_id.as_str(), c.course_id.as_str(), c.cpi.as_str()),
            ("111", "222", "333")
        );
        assert_eq!(c.title, "大学英语");
        assert_eq!(c.teacher, "张老师");
    }

    #[test]
    fn course_list_accepts_cpi_as_last_query_param() {
        let html = r#"
        <div class="course" id="c1" info="i1" roleid="3">
          <input class="clazzId" value="111"><input class="courseId" value="222">
          <a href="https://mooc1.chaoxing.com/visit/stucoursemiddle?courseid=222&clazzid=111&cpi=333">x</a>
          <span class="course-name" title="大学英语"></span>
        </div>"#;
        let courses = parse_course_list(html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].cpi, "333");
    }

    #[test]
    fn course_points_detect_status() {
        let html = r#"
        <div class="chapter_unit"><ul>
          <li><div id="cur1001"><a class="clicktitle">第一章
              绪论</a><span class="bntHoverTips">已完成</span><input class="knowledgeJobCount" value="2"></div></li>
          <li><div id="cur1002"><a class="clicktitle">第二章</a><span class="bntHoverTips">待解锁</span></div></li>
          <li><div id="other"><a class="clicktitle">无效</a></div></li>
        </ul></div>"#;
        let points = parse_course_points(html);
        assert_eq!(points.points.len(), 2);
        assert!(points.has_locked);
        assert_eq!(points.points[0].id, "1001");
        assert_eq!(
            points.points[0].title,
            "第一章              绪论".replace("              ", "              ")
        );
        assert!(points.points[0].has_finished);
        assert_eq!(points.points[0].job_count, "2");
        assert!(points.points[1].need_unlock);
    }

    #[test]
    fn course_card_extracts_jobs() {
        let html = r#"<script>
        try{ mArg = {"attachments":[
          {"type":"video","job":true,"jobid":"v1","mid":123,"objectId":"obj","otherInfo":"nodeId_9-cpi_1-rt_d&courseId=1","playTime":30000,
           "property":{"name":"第 1 讲 视频","rt":0.9}},
          {"type":"audio","job":true,"jobid":"a1","mid":456,"objectId":"audio-obj","otherInfo":"nodeId_10-cpi_1-rt_d",
           "property":{"name":"第 2 讲 音频","rt":1}},
          {"type":"document","job":true,"jobid":"d1","jtoken":"tk","property":{"objectid":"o2"}},
          {"type":"workid","job":true,"jobid":"work-55","enc":"e1"},
          {"type":"video","jobid":"done","isPassed":true,"mid":1},
          {"type":"video","jobid":"nomid","property":{}},
          {"type":"read","jobid":"r1","job":null,"property":{"read":false,"title":"阅读"}},
          {"type":"live","job":true,"jobid":"l1","property":{"liveId":"77","streamName":"s","vdoid":"v","title":"直播课"}}
        ],"defaults":{"knowledgeid":9,"ktoken":"kt","cpi":"1","reportTimeInterval":60}};}catch(e){}
        </script>"#;
        let (jobs, info) = parse_chapter_jobs(html);
        assert!(info.present);
        assert_eq!(info.knowledgeid, "9");
        let kinds: Vec<_> = jobs.iter().map(|j| (j.kind, j.jobid.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (JobType::Video, "v1"),
                (JobType::Video, "a1"),
                (JobType::Document, "d1"),
                (JobType::Work, "work-55"),
                (JobType::Read, "r1"),
                (JobType::Live, "l1"),
            ]
        );
        let video = &jobs[0];
        assert_eq!(video.otherinfo, "nodeId_9-cpi_1-rt_d");
        assert_eq!(video.name, "第 1 讲 视频");
        assert_eq!(video.play_time_ms, 30000);
        assert_eq!(video.rt, "0.9");
        assert_eq!(video.mid, "123");
        assert_eq!(jobs[1].name, "第 2 讲 音频");
        assert_eq!(jobs[1].objectid, "audio-obj");
        assert_eq!(jobs[1].mid, "456");
        assert!(jobs[1].is_audio);
        assert!(!jobs[0].is_audio);
        assert_eq!(jobs[5].name, "直播课");
    }

    #[test]
    fn course_card_accepts_numeric_live_id_and_read_with_job() {
        let html = r#"<script>
        try{ mArg = {"attachments":[
          {"type":"live","job":true,"jobid":"live-1","property":{"liveId":8000292482101802,"streamName":"s","vdoid":"v","title":"直播"}},
          {"type":"read","job":true,"jobid":"read-1","jtoken":"t","property":{"read":false,"title":"阅读"}},
          {"type":"read","job":true,"jobid":"read-2","property":{"read":true,"title":"已读"}}
        ],"defaults":{}};}catch(e){}
        </script>"#;
        let (jobs, _) = parse_chapter_jobs(html);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].kind, JobType::Live);
        assert_eq!(jobs[0].jobid, "live-1");
        assert_eq!(jobs[1].kind, JobType::Read);
        assert_eq!(jobs[1].jobid, "read-1");
    }

    #[test]
    fn course_card_not_open() {
        let (jobs, info) = parse_chapter_jobs("<div>章节未开放</div>");
        assert!(jobs.is_empty());
        assert!(info.not_open);
    }

    #[test]
    fn questions_form_is_extracted() {
        let html = r#"<form id="form1">
          <input name="courseId" value="1"><input name="workAnswerId" value="9">
          <input name="answer100" value="x"><input name="pyFlag" value="">
          <div class="singleQuesId" data="100"><div class="TiMu" data="0">
            <div class="Zy_TItle"><div>1<span>中国的首都是</span><img src="https://p.ananas.chaoxing.com/a.png">（2.0分）</div></div>
            <ul><li aria-label="B. 北京选择"></li><li aria-label="A. 上海选择"></li></ul>
          </div></div>
          <div class="singleQuesId" data="101"><div class="TiMu" data="3">
            <div class="Zy_TItle">地球是圆的</div><ul><li>对</li><li>错</li></ul>
          </div></div>
        </form>"#;
        let form = parse_work_form(html);
        assert_eq!(form.get("courseId"), Some("1"));
        assert_eq!(form.get("workAnswerId"), Some("9"));
        assert_eq!(form.get("answer100"), None);
        assert_eq!(form.get("answerwqbid"), Some("100,101,"));
        assert_eq!(form.questions.len(), 2);
        let q = &form.questions[0];
        assert_eq!(q.q_type, QuestionType::Single);
        assert_eq!(
            q.title,
            "1中国的首都是<img src=\"https://p.ananas.chaoxing.com/a.png\">（2.0分）"
        );
        assert_eq!(q.options, "A. 上海\nB. 北京");
        assert_eq!(form.questions[1].q_type, QuestionType::Judgement);
        assert_eq!(form.questions[1].type_code, "3");
    }
}
