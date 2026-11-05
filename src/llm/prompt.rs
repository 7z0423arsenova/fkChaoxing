//! 答题提示词。
//!
//! 选择题要求模型返回选项字母（比返回选项全文更不容易出错），解析后再换回选项内容，
//! 供选项内容匹配与缓存使用。

use std::sync::LazyLock;

use regex::Regex;

use crate::models::{Question, QuestionType};
use crate::tiku::matching::strip_option_prefix;

static IMG_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<img\s+[^>]*?src\s*=\s*"([^"]*)"[^>]*>"#).unwrap());

/// 选择题的一个选项
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub letter: char,
    pub text: String,
}

/// 将题目选项解析为（字母，内容）。选项行首字符不是字母时按顺序补齐字母。
pub fn parse_options(question: &Question) -> Vec<ChoiceOption> {
    if !question.q_type.is_choice() {
        return Vec::new();
    }
    question
        .option_lines()
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let first = line.chars().next().filter(char::is_ascii_alphabetic);
            match first {
                Some(letter) => ChoiceOption {
                    letter: letter.to_ascii_uppercase(),
                    text: strip_option_prefix(line),
                },
                None => ChoiceOption {
                    letter: (b'A' + (i as u8).min(25)) as char,
                    text: line.trim().to_owned(),
                },
            }
        })
        .collect()
}

/// 题干中的图片地址（按出现顺序）
pub fn image_urls(title: &str) -> Vec<String> {
    IMG_TAG_RE
        .captures_iter(title)
        .map(|c| c[1].to_owned())
        .filter(|u| !u.is_empty())
        .collect()
}

/// 把题干中的 `<img>` 标签替换为文字占位：`attached` 张图片已作为附件发送时显示为 [图片1]…，其余为 [图片]。
pub fn replace_images(title: &str, attached: usize) -> String {
    let mut index = 0;
    IMG_TAG_RE
        .replace_all(title, |_: &regex::Captures<'_>| {
            index += 1;
            if index <= attached {
                format!("[图片{index}]")
            } else {
                "[图片]".to_owned()
            }
        })
        .into_owned()
}

const BASE_SYSTEM_PROMPT: &str =
    "你是一名严谨、细致的答题助手。请根据题目和选项，运用相关学科知识作答；即使不能完全确定，也要给出最可能的答案。

输出要求：
1. 只输出一个 JSON 对象，不要输出推理过程、解释、Markdown 代码块或任何其他文字。
2. JSON 必须严格符合 {\"answer\": [...]} 的结构。
3. answer 必须是非空数组；数组元素的类型和数量由下面的题型规则决定。
4. 不要留空，不要拒绝作答，不要添加 JSON 之外的字符。";

fn type_rule(q_type: QuestionType) -> &'static str {
    match q_type {
        QuestionType::Single => {
            "本题是单选题，只有一个正确选项。answer 数组中只放一个选项字母，例如 {\"answer\": [\"B\"]}。字母必须与题目给出的选项字母一致。"
        }
        QuestionType::Multiple => {
            "本题是多选题，正确选项有两个或以上。answer 数组中按字母顺序逐个放入所有正确选项，每个字母单独作为一个元素，例如 {\"answer\": [\"A\", \"C\", \"D\"]}；不要把多个字母拼成一个字符串。"
        }
        QuestionType::Judgement => {
            "本题是判断题。answer 数组中只放一个字符串：\"正确\" 或 \"错误\"，例如 {\"answer\": [\"正确\"]}。不要使用“对/错”“True/False”等其他写法。"
        }
        QuestionType::Completion => {
            "本题是填空题。按空格在题目中出现的顺序，把每个空应填的内容依次放进 answer 数组，每个空一个元素，不要带序号或“第一空”等说明，例如 {\"answer\": [\"答案一\", \"答案二\"]}；只有一个空时数组中只放一个元素。"
        }
        QuestionType::ShortAnswer | QuestionType::Unknown => {
            "本题是简答题。请用简洁、完整、条理清晰的中文作答，把全部内容放在一个字符串中，例如 {\"answer\": [\"……\"]}。"
        }
    }
}

/// 系统提示词：通用要求 + 题型规则 + 用户自定义要求 + 上一轮错题反馈。
pub fn system_prompt(q_type: QuestionType, extra: Option<&str>, feedback: &[String]) -> String {
    let mut prompt = format!("{BASE_SYSTEM_PROMPT}\n\n{}", type_rule(q_type));
    if let Some(extra) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        prompt.push_str("\n\n补充要求：");
        prompt.push_str(extra);
    }
    let feedback = feedback_text(feedback);
    if !feedback.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&feedback);
    }
    prompt
}

/// 章节检测重做时，附带上一轮答错的题目与正确答案。
pub fn feedback_text(feedback: &[String]) -> String {
    if feedback.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "上一轮作答中，以下题目回答错误（已附正确答案）。请先理解错因，再回答当前题目；如果当前题目与其中某题相同，直接使用给出的正确答案：".to_owned(),
    ];
    lines.extend(feedback.iter().cloned());
    lines.join("\n")
}

/// 用户消息正文
pub fn user_prompt(
    question: &Question,
    title: &str,
    options: &[ChoiceOption],
    course: Option<&str>,
) -> String {
    let mut text = String::new();
    if let Some(course) = course.map(str::trim).filter(|s| !s.is_empty()) {
        text.push_str(&format!("课程：《{course}》\n"));
    }
    text.push_str(&format!(
        "题型：{}\n题目：{}",
        question.q_type.display(),
        title.trim()
    ));
    if !options.is_empty() {
        text.push_str("\n选项：");
        for option in options {
            text.push_str(&format!("\n{}. {}", option.letter, option.text));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(q_type: QuestionType, options: &[&str]) -> Question {
        let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        Question::new("题目", q_type, &options)
    }

    #[test]
    fn options_are_parsed_with_letters() {
        let q = question(
            QuestionType::Single,
            &["A. 北京", "B、上海", "c：广州", "深圳"],
        );
        let opts = parse_options(&q);
        assert_eq!(
            opts,
            vec![
                ChoiceOption {
                    letter: 'A',
                    text: "北京".into()
                },
                ChoiceOption {
                    letter: 'B',
                    text: "上海".into()
                },
                ChoiceOption {
                    letter: 'C',
                    text: "广州".into()
                },
                ChoiceOption {
                    letter: 'D',
                    text: "深圳".into()
                },
            ]
        );
        assert!(parse_options(&question(QuestionType::Judgement, &["对", "错"])).is_empty());
    }

    #[test]
    fn images_are_extracted_and_replaced() {
        let title = r#"看图<img src="https://a/1.png">回答<img src="https://a/2.jpg" width="10">"#;
        assert_eq!(
            image_urls(title),
            vec!["https://a/1.png", "https://a/2.jpg"]
        );
        assert_eq!(replace_images(title, 1), "看图[图片1]回答[图片]");
    }

    #[test]
    fn prompts_contain_type_rules_and_feedback() {
        let q = question(QuestionType::Multiple, &["A. 甲", "B. 乙"]);
        let opts = parse_options(&q);
        let user = user_prompt(&q, "以下正确的是", &opts, Some("大学物理"));
        assert_eq!(
            user,
            "课程：《大学物理》\n题型：多选题\n题目：以下正确的是\n选项：\nA. 甲\nB. 乙"
        );
        let system = system_prompt(q.q_type, Some("按教材作答"), &["- 题目：x".to_owned()]);
        assert!(system.contains("多选题"));
        assert!(system.contains("每个字母单独作为一个元素"));
        assert!(system.contains("补充要求：按教材作答"));
        assert!(system.contains("- 题目：x"));
        assert!(!system_prompt(QuestionType::Single, None, &[]).contains("上一轮作答"));
    }
}
