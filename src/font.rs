//! 超星加密字体解码。
//!
//! 章节检测页面会内嵌一个打乱了字形映射的 TTF 字体（`style#cxSecretStyle`）。解码方法：
//! 计算加密字体中每个字形轮廓的哈希，再在预先生成的标准字体哈希表
//! （`resource/font_map_table.json`，构建时 gzip 压缩后嵌入）中找回原始字符。
//! 哈希算法对每个轮廓点拼接 `"{x}{y}{on_curve}"` 后取 MD5。

use std::collections::HashMap;
use std::io::Read;
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use regex::Regex;
use scraper::{Html, Selector};
use tracing::warn;
use ttf_parser::{Face, GlyphId, Tag};

use crate::crypto::md5_hex;

/// 构建脚本生成的 gzip 压缩映射表，避免把 1.6MB 的 JSON 原样放进二进制。
static FONT_MAP_TABLE_GZ: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/font_map_table.json.gz"));

/// 标准字体：字形哈希 -> 字形名（同一哈希出现多次时以最后一个为准）。
static REFERENCE_TABLE: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut json = String::new();
    if let Err(e) = flate2::read::GzDecoder::new(FONT_MAP_TABLE_GZ).read_to_string(&mut json) {
        warn!("解压字体映射表失败: {e}");
        return HashMap::new();
    }
    match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json) {
        Ok(map) => map
            .into_iter()
            .filter_map(|(name, hash)| hash.as_str().map(|h| (h.to_owned(), name)))
            .collect(),
        Err(e) => {
            warn!("加载字体映射表失败: {e}");
            HashMap::new()
        }
    }
});

/// 康熙部首 -> 对应的常用汉字
const KX_RADICALS: &str = "⼀⼁⼂⼃⼄⼅⼆⼇⼈⼉⼊⼋⼌⼍⼎⼏⼐⼑⼒⼓⼔⼕⼖⼗⼘⼙⼚⼛⼜⼝⼞⼟⼠⼡⼢⼣⼤⼥⼦⼧⼨⼩⼪⼫⼬⼭⼮⼯⼰⼱⼲⼳⼴⼵⼶⼷⼸⼹⼺⼻⼼⼽⼾⼿⽀⽁⽂⽃⽄⽅⽆⽇⽈⽉⽊⽋⽌⽍⽎⽏⽐⽑⽒⽓⽔⽕⽖⽗⽘⽙⽚⽛⽜⽝⽞⽟⽠⽡⽢⽣⽤⽥⽦⽧⽨⽩⽪⽫⽬⽭⽮⽯⽰⽱⽲⽳⽴⽵⽶⽷⽸⽹⽺⽻⽼⽽⽾⽿⾀⾁⾂⾃⾄⾅⾆⾇⾈⾉⾊⾋⾌⾍⾎⾏⾐⾑⾒⾓⾔⾕⾖⾗⾘⾙⾚⾛⾜⾝⾞⾟⾠⾡⾢⾣⾤⾥⾦⾧⾨⾩⾪⾫⾬⾭⾮⾯⾰⾱⾲⾳⾴⾵⾶⾷⾸⾹⾺⾻⾼髙⾽⾾⾿⿀⿁⿂⿃⿄⿅⿆⿇⿈⿉⿊⿋⿌⿍⿎⿏⿐⿑⿒⿓⿔⿕⺠⻬⻩⻢⻜⻅⺟⻓⻛";
const KX_CHARS: &str = "一丨丶丿乙亅二亠人儿入八冂冖冫几凵刀力勹匕匚匸十卜卩厂厶又口囗土士夂夊夕大女子宀寸小尢尸屮山巛工己巾干幺广廴廾弋弓彐彡彳心戈戶手支攴文斗斤方无日曰月木欠止歹殳毋比毛氏气水火爪父爻爿片牙牛犬玄玉瓜瓦甘生用田疋疒癶白皮皿目矛矢石示禸禾穴立竹米糸缶网羊羽老而耒耳聿肉臣自至臼舌舛舟艮色艸虍虫血行衣襾見角言谷豆豕豸貝赤走足身車辛辰辵邑酉采里金長門阜隶隹雨青非面革韋韭音頁風飛食首香馬骨高高髟鬥鬯鬲鬼魚鳥鹵鹿麥麻黃黍黑黹黽鼎鼓鼠鼻齊齒龍龜龠民齐黄马飞见母长风";

static KX_TABLE: LazyLock<HashMap<char, char>> =
    LazyLock::new(|| KX_RADICALS.chars().zip(KX_CHARS.chars()).collect());

static FONT_BASE64_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"base64,([A-Za-z0-9+/=\s]+)").unwrap());

/// 忽略 Base64 文本中的非编码字符，不强制填充。
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// 页面加密字体的解码器。
pub struct FontDecoder {
    /// 加密字体：字形名（如 `uni4E00`）-> 字形哈希
    glyph_hashes: HashMap<String, String>,
}

impl FontDecoder {
    /// 从页面 HTML 中提取 `style#cxSecretStyle` 内嵌字体；页面没有加密字体时返回 `Ok(None)`。
    pub fn from_html(document: &Html) -> Result<Option<Self>> {
        static STYLE: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse("style#cxSecretStyle").unwrap());
        let Some(style) = document.select(&STYLE).next() else {
            return Ok(None);
        };
        let css: String = style.text().collect();
        let encoded = FONT_BASE64_RE
            .captures(&css)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow!("无法从样式标签中提取字体数据"))?
            .as_str();
        let cleaned: String = encoded
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();
        let bytes = LENIENT_BASE64
            .decode(cleaned)
            .context("无法解码 Base64 字体数据")?;
        Ok(Some(Self::from_font_bytes(&bytes)?))
    }

    pub fn from_font_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            glyph_hashes: glyph_hashes(bytes)?,
        })
    }

    pub fn decode(&self, text: &str) -> String {
        decrypt(&self.glyph_hashes, &REFERENCE_TABLE, text)
    }
}

/// 使用给定的加密字体哈希表与标准字体反查表解密文本。
pub fn decrypt(
    glyph_hashes: &HashMap<String, String>,
    reference: &HashMap<String, String>,
    text: &str,
) -> String {
    text.chars()
        .map(|ch| {
            let original = glyph_hashes
                .get(&format!("uni{:X}", ch as u32))
                .and_then(|hash| reference.get(hash))
                .and_then(|name| name.get(3..))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .and_then(char::from_u32)
                .unwrap_or(ch);
            KX_TABLE.get(&original).copied().unwrap_or(original)
        })
        .collect()
}

/// 计算字体中所有 `uni*` 字形的轮廓哈希。
pub fn glyph_hashes(font: &[u8]) -> Result<HashMap<String, String>> {
    let face = Face::parse(font, 0).map_err(|e| anyhow!("无法解析字体文件: {e}"))?;
    let raw = face.raw_face();
    let glyf = raw
        .table(Tag::from_bytes(b"glyf"))
        .ok_or_else(|| anyhow!("字体缺少 glyf 表"))?;
    let loca = raw
        .table(Tag::from_bytes(b"loca"))
        .ok_or_else(|| anyhow!("字体缺少 loca 表"))?;
    let head = raw
        .table(Tag::from_bytes(b"head"))
        .ok_or_else(|| anyhow!("字体缺少 head 表"))?;
    let long_offsets = read_i16(head, 50).ok_or_else(|| anyhow!("head 表格式错误"))? != 0;
    let num_glyphs = face.number_of_glyphs();

    let names = glyph_names(&face);
    let mut map = HashMap::new();
    for gid in 0..num_glyphs {
        let Some(name) = names.get(gid as usize).and_then(Option::as_deref) else {
            continue;
        };
        if !name.starts_with("uni") {
            continue;
        }
        let range = glyph_range(loca, gid as usize, long_offsets);
        let Some(data) = range.and_then(|(s, e)| glyf.get(s..e)) else {
            continue;
        };
        if let Some(hash) = hash_glyph(data) {
            map.insert(name.to_owned(), hash);
        }
    }
    Ok(map)
}

/// 字形名：优先使用 post 表，没有字形名时按 fontTools 的规则由 cmap 推导（`uniXXXX`）。
fn glyph_names(face: &Face<'_>) -> Vec<Option<String>> {
    let count = face.number_of_glyphs() as usize;
    let from_post: Vec<Option<String>> = (0..count)
        .map(|gid| face.glyph_name(GlyphId(gid as u16)).map(str::to_owned))
        .collect();
    if from_post.iter().skip(1).any(Option::is_some) {
        return from_post;
    }

    let mut codepoints: Vec<Option<u32>> = vec![None; count];
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables.into_iter().filter(|s| s.is_unicode()) {
            subtable.codepoints(|cp| {
                if let Some(slot) = subtable
                    .glyph_index(cp)
                    .and_then(|gid| codepoints.get_mut(gid.0 as usize))
                {
                    *slot = Some(slot.map_or(cp, |old| old.min(cp)));
                }
            });
        }
    }
    let mut used: HashMap<String, usize> = HashMap::new();
    codepoints
        .into_iter()
        .map(|cp| {
            let cp = cp?;
            let base = if cp <= 0xFFFF {
                format!("uni{cp:04X}")
            } else {
                format!("u{cp:X}")
            };
            let uses = used.entry(base.clone()).or_insert(0);
            *uses += 1;
            Some(if *uses > 1 {
                format!("{base}.alt{}", *uses - 1)
            } else {
                base
            })
        })
        .collect()
}

fn glyph_range(loca: &[u8], gid: usize, long_offsets: bool) -> Option<(usize, usize)> {
    if long_offsets {
        let start = read_u32(loca, gid * 4)? as usize;
        let end = read_u32(loca, gid * 4 + 4)? as usize;
        (end >= start).then_some((start, end))
    } else {
        let start = read_u16(loca, gid * 2)? as usize * 2;
        let end = read_u16(loca, gid * 2 + 2)? as usize * 2;
        (end >= start).then_some((start, end))
    }
}

/// 简单字形的轮廓哈希；空字形与复合字形返回 `None`。
fn hash_glyph(data: &[u8]) -> Option<String> {
    let contours = read_i16(data, 0)?;
    if contours <= 0 {
        return None;
    }
    let contours = contours as usize;
    let mut pos = 10;
    let mut end_points = Vec::with_capacity(contours);
    for _ in 0..contours {
        end_points.push(read_u16(data, pos)? as usize);
        pos += 2;
    }
    let num_points = *end_points.last()? + 1;
    let instructions = read_u16(data, pos)? as usize;
    pos += 2 + instructions;

    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        let flag = *data.get(pos)?;
        pos += 1;
        flags.push(flag);
        if flag & 0x08 != 0 {
            let repeat = *data.get(pos)?;
            pos += 1;
            flags.extend(std::iter::repeat_n(flag, repeat as usize));
        }
    }
    flags.truncate(num_points);

    let xs = read_coordinates(data, &mut pos, &flags, 0x02, 0x10)?;
    let ys = read_coordinates(data, &mut pos, &flags, 0x04, 0x20)?;

    let mut buffer = String::new();
    let mut last = 0;
    for end in end_points {
        for j in last..=end {
            let (x, y, f) = (xs.get(j)?, ys.get(j)?, flags.get(j)?);
            buffer.push_str(&format!("{x}{y}{}", f & 0x01));
        }
        last = end + 1;
    }
    Some(md5_hex(buffer))
}

fn read_coordinates(
    data: &[u8],
    pos: &mut usize,
    flags: &[u8],
    short: u8,
    same_or_positive: u8,
) -> Option<Vec<i32>> {
    let mut value = 0i32;
    let mut out = Vec::with_capacity(flags.len());
    for &flag in flags {
        if flag & short != 0 {
            let delta = *data.get(*pos)? as i32;
            *pos += 1;
            value += if flag & same_or_positive != 0 {
                delta
            } else {
                -delta
            };
        } else if flag & same_or_positive == 0 {
            value += read_i16(data, *pos)? as i32;
            *pos += 2;
        }
        out.push(value);
    }
    Some(out)
}

fn read_u16(data: &[u8], pos: usize) -> Option<u16> {
    data.get(pos..pos + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn read_i16(data: &[u8], pos: usize) -> Option<i16> {
    read_u16(data, pos).map(|v| v as i16)
}

fn read_u32(data: &[u8], pos: usize) -> Option<u32> {
    data.get(pos..pos + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 校验内嵌的字体映射表可以正常加载。
#[cfg(test)]
pub fn reference_table_size() -> usize {
    REFERENCE_TABLE.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_table_is_embedded() {
        assert!(reference_table_size() > 10_000);
    }

    #[test]
    fn kangxi_radicals_are_normalized() {
        assert_eq!(KX_RADICALS.chars().count(), KX_CHARS.chars().count());
        let empty = HashMap::new();
        assert_eq!(decrypt(&empty, &empty, "⼀⼈⻛abc"), "一人风abc");
    }

    #[test]
    fn font_base64_supports_single_double_and_no_quotes() {
        for css in [
            r"@font-face{src:url('data:application/font-ttf;base64,QUJD')}",
            r#"@font-face{src:url("data:application/font-ttf;base64,QUJD")}"#,
            r"@font-face{src:url(data:application/font-ttf;base64,QUJD)}",
        ] {
            let captured = FONT_BASE64_RE
                .captures(css)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
                .unwrap();
            assert_eq!(captured, "QUJD");
        }
    }

    #[test]
    fn decrypt_maps_through_glyph_hashes() {
        let glyphs = HashMap::from([
            ("uni4E00".to_owned(), "h1".to_owned()),
            ("uni41".to_owned(), "h2".to_owned()),
        ]);
        let reference = HashMap::from([
            ("h1".to_owned(), "uni5F00".to_owned()),
            ("h2".to_owned(), "B".to_owned()),
        ]);
        // 一 -> 开；无法解析的字形名（"B"）保留原字符；'A' 的键为 "uni41"
        assert_eq!(decrypt(&glyphs, &reference, "一A二"), "开A二");
    }
}
