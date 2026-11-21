//! 设置页"添加词语"：把一段文字拆成词语，给每个词定拼音。
//!
//! - 词语之间用任意标点、空格、换行隔开："张子涵，螺蛳粉 卷王；看盘" 是 4 个词
//! - 多音字可以自己写拼音：`曾经(zeng jing)`、`银行（yinhang）`
//! - 没写拼音的：每个字取最常用的读音；多音字参考词库里现成的词（"银行"的"行"读 hang）

use std::collections::HashMap;

use crate::dict::Dict;

/// 一个词最多几个字
pub const MAX_WORD_CHARS: usize = 8;
/// 一次最多加多少个词
pub const MAX_WORDS: usize = 500;

/// 拆出来的一个词：(文字, 你写的拼音)
pub type Item = (String, Option<String>);

fn is_han(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '〇')
}

/// 分隔符：空白、各种标点（括号除外，括号用来写拼音）
fn is_sep(c: char) -> bool {
    if matches!(c, '(' | ')' | '（' | '）') {
        return false;
    }
    c.is_whitespace()
        || c.is_ascii_punctuation()
        || matches!(c as u32,
            0x3000..=0x303F      // 中文标点：、。〈〉《》「」『』【】〔〕
            | 0xFF01..=0xFF0F    // 全角 ！＂＃＄％＆＇＊＋，－．／
            | 0xFF1A..=0xFF20    // ：；＜＝＞？＠
            | 0xFF3B..=0xFF40
            | 0xFF5B..=0xFF65
            | 0x2010..=0x2027    // ‐ – — ‘ ’ “ ” … ·
            | 0x2030..=0x205E
            | 0xFE30..=0xFE4F
            | 0x00B7)
}

/// 拆成词语。括号里的是前面那个词的拼音。
pub fn split(text: &str) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' || c == '（' {
            let mut py = String::new();
            for d in chars.by_ref() {
                if d == ')' || d == '）' {
                    break;
                }
                py.push(d);
            }
            if !cur.is_empty() {
                out.push((std::mem::take(&mut cur), Some(py.trim().to_string())));
            }
            continue;
        }
        if c == ')' || c == '）' || is_sep(c) {
            if !cur.is_empty() {
                out.push((std::mem::take(&mut cur), None));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push((cur, None));
    }
    out
}

/// 每个字的读音：字 → [(音节, 分数)]，常用的读音在前。
pub fn char_readings(dict: &Dict) -> HashMap<char, Vec<(u16, f32)>> {
    let mut m: HashMap<char, Vec<(u16, f32)>> = HashMap::new();
    dict.for_each_word(|code, w| {
        if code.len() == 1 {
            let mut cs = dict.word_text(w).chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                m.entry(c).or_default().push((code[0], dict.word_score(w)));
            }
        }
    });
    for v in m.values_mut() {
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.dedup_by_key(|x| x.0);
        v.truncate(4);
    }
    m
}

/// 你写的拼音 → 音节编号。可以用空格、' 隔开，也可以连着写（按字数切）。
fn parse_pinyin(dict: &Dict, py: &str, n: usize) -> Option<Vec<u16>> {
    let lower = py.to_ascii_lowercase().replace('ü', "v");
    let parts: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphabetic()).filter(|p| !p.is_empty()).collect();
    if parts.len() == n {
        return parts.iter().map(|p| dict.syllable_id(p)).collect();
    }
    // 连着写的：切成正好 n 个音节
    let s: String = parts.concat();
    let b = s.as_bytes();
    // ways[i] = 从 i 开始切成若干音节的一种切法（音节数, 下一个位置）
    fn go(dict: &Dict, s: &str, i: usize, left: usize, out: &mut Vec<u16>) -> bool {
        if i == s.len() {
            return left == 0;
        }
        if left == 0 {
            return false;
        }
        for l in (1..=6.min(s.len() - i)).rev() {
            if let Some(id) = dict.syllable_id(&s[i..i + l]) {
                out.push(id);
                if go(dict, s, i + l, left - 1, out) {
                    return true;
                }
                out.pop();
            }
        }
        false
    }
    let _ = b;
    let mut out = Vec::new();
    go(dict, &s, 0, n, &mut out).then_some(out)
}

/// 没写拼音时自动定拼音。
fn guess_pinyin(dict: &Dict, readings: &HashMap<char, Vec<(u16, f32)>>, text: &str) -> Option<Vec<u16>> {
    let chars: Vec<char> = text.chars().collect();
    let opts: Vec<&Vec<(u16, f32)>> = chars.iter().map(|c| readings.get(c)).collect::<Option<_>>()?;
    let n = chars.len();
    // 在这几个读音组合里找词库里有的（最多试 256 种）
    let find = |lo: usize, hi: usize| -> Option<Vec<u16>> {
        let sub: String = chars[lo..hi].iter().collect();
        let mut idx = vec![0usize; hi - lo];
        for _ in 0..256 {
            let code: Vec<u16> = (lo..hi).map(|i| opts[i][idx[i - lo]].0).collect();
            if dict.has_word(&code, &sub) {
                return Some(code);
            }
            // 下一个组合
            let mut k = idx.len();
            loop {
                if k == 0 {
                    return None;
                }
                k -= 1;
                idx[k] += 1;
                if idx[k] < opts[lo + k].len() {
                    break;
                }
                idx[k] = 0;
            }
        }
        None
    };
    if let Some(c) = find(0, n) {
        return Some(c);
    }
    // 整个词不在词库里：先用每个字最常用的读音，再用词库里的三字、两字词修正多音字
    let mut code: Vec<u16> = opts.iter().map(|o| o[0].0).collect();
    let mut fixed = vec![false; n];
    for w in [3usize, 2] {
        if n < w {
            continue;
        }
        for i in 0..=n - w {
            if fixed[i..i + w].iter().any(|&f| f) {
                continue;
            }
            if let Some(c) = find(i, i + w) {
                code[i..i + w].copy_from_slice(&c);
                fixed[i..i + w].iter_mut().for_each(|f| *f = true);
            }
        }
    }
    Some(code)
}

/// 一个词的拼音；定不了返回跳过的原因。
pub fn pinyin_of(
    dict: &Dict,
    readings: &HashMap<char, Vec<(u16, f32)>>,
    text: &str,
    py: Option<&str>,
) -> Result<Vec<u16>, &'static str> {
    let n = text.chars().count();
    if !text.chars().all(is_han) {
        return Err("只能加汉字");
    }
    if n < 2 {
        return Err("单个字不用加");
    }
    if n > MAX_WORD_CHARS {
        return Err("太长了（最多 8 个字）");
    }
    match py {
        Some(p) if !p.is_empty() => parse_pinyin(dict, p, n).ok_or("拼音和字数对不上"),
        _ => guess_pinyin(dict, readings, text).ok_or("有不认识的字"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    #[test]
    fn splits_on_any_punctuation() {
        let v = split("张子涵，螺蛳粉 卷王；看盘\n曾经(zeng jing)、银行（yinhang）!abc 1号");
        let words: Vec<&str> = v.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(words, ["张子涵", "螺蛳粉", "卷王", "看盘", "曾经", "银行", "abc", "1号"]);
        assert_eq!(v[4].1.as_deref(), Some("zeng jing"));
        assert_eq!(v[5].1.as_deref(), Some("yinhang"));
        assert_eq!(v[0].1, None);
    }

    #[test]
    fn picks_readings_using_known_words() {
        let mut b = DictBuilder::new(["yin", "hang", "xing", "zeng", "ceng", "jing"].map(String::from));
        let id = |b: &DictBuilder, s: &str| b.syllable_id(s).unwrap();
        let (yin, hang, xing) = (id(&b, "yin"), id(&b, "hang"), id(&b, "xing"));
        b.add(&[yin], "银", -6.0);
        b.add(&[xing], "行", -5.0); // xing 比 hang 常用
        b.add(&[hang], "行", -7.0);
        b.add(&[yin, hang], "银行", -8.0);
        let d = Dict::from_bytes(b.build()).unwrap();
        let r = char_readings(&d);
        // "银行"在词库里：读 yin hang
        assert_eq!(pinyin_of(&d, &r, "银行", None).unwrap(), vec![yin, hang]);
        // "银行卡"不在词库里：靠"银行"修正
        // （"卡"不认识，跳过）
        assert!(pinyin_of(&d, &r, "银行卡", None).is_err());
        // 自己写拼音
        assert_eq!(pinyin_of(&d, &r, "银行", Some("yinhang")).unwrap(), vec![yin, hang]);
        assert_eq!(pinyin_of(&d, &r, "银行", Some("yin xing")).unwrap(), vec![yin, xing]);
        assert_eq!(pinyin_of(&d, &r, "银行", Some("yin")), Err("拼音和字数对不上"));
        assert_eq!(pinyin_of(&d, &r, "银", None), Err("单个字不用加"));
        assert_eq!(pinyin_of(&d, &r, "abc", None), Err("只能加汉字"));
    }
}
