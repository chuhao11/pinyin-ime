//! 把输入的字母串切成可能的音节片段。
//!
//! 每个位置可能有几种片段：
//! - 完整音节（`xian`、`xi`、`an`）
//! - 声母简拼（`z`、`zh`、`g`），可以代表所有以它开头的音节，用于 `zg`→中国
//! - 输入末尾没打完的音节（`zho` 代表 zhong/zhou），用于边打边出字
//!
//! 一串字母的所有切法不在这里展开，而是交给查词时一起走（见 lattice.rs），
//! 所以 `xian` 同时能查到"先"（xian）和"西安"（xi + an），不用打分隔符。

use crate::dict::Dict;

pub const INITIALS: [&str; 23] = [
    "b", "c", "ch", "d", "f", "g", "h", "j", "k", "l", "m", "n", "p", "q", "r", "s", "sh", "t", "w", "x", "y", "z",
    "zh",
];

/// 常见的另一种写法：lue/nue 写成 lve/nve，j/q/x/y 后面的 v 当 u。
const ALIASES: [(&str, &str); 6] =
    [("lue", "lve"), ("nue", "nve"), ("jv", "ju"), ("qv", "qu"), ("xv", "xu"), ("yv", "yu")];

pub const MAX_SYL_LEN: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Full,
    Initial,
    Partial,
}

/// 从某个位置开始的一个音节片段：到 `end` 为止，代表音节编号 [lo, hi)。
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub end: usize,
    pub lo: u16,
    pub hi: u16,
    pub kind: TokKind,
}

fn full_id(dict: &Dict, piece: &str) -> Option<u16> {
    dict.syllable_id(piece).or_else(|| ALIASES.iter().find(|(a, _)| *a == piece).and_then(|(_, t)| dict.syllable_id(t)))
}

/// `s` 必须已经转成小写；`'` 是分隔符。返回每个位置开始的片段。
pub fn tokenize(dict: &Dict, s: &[u8]) -> Vec<Vec<Token>> {
    let n = s.len();
    let mut out = vec![Vec::new(); n + 1];
    for i in 0..n {
        if !s[i].is_ascii_lowercase() {
            continue;
        }
        for len in 1..=MAX_SYL_LEN {
            let j = i + len;
            if j > n || !s[j - 1].is_ascii_lowercase() {
                break;
            }
            let piece = std::str::from_utf8(&s[i..j]).unwrap_or("");
            if let Some(id) = full_id(dict, piece) {
                out[i].push(Token { end: j, lo: id, hi: id + 1, kind: TokKind::Full });
                // 末尾的完整音节也可能还没打完（zhonggu → 中国），但只在前面已经有拼音时才这样算，
                // 否则单独打 xia 会冒出"想、小"
                if j == n && i > 0 {
                    let (lo, hi) = dict.prefix_range(piece);
                    if hi - lo > 1 {
                        out[i].push(Token { end: j, lo, hi, kind: TokKind::Partial });
                    }
                }
                continue;
            }
            if INITIALS.contains(&piece) {
                // 声母后面紧跟的字母能和它拼成音节（比如 g 后面是 a），说明你在打全拼，
                // 这里不当简拼，否则会出现 fangan → "犯规啊" 这种怪候选。
                if j < n && s[j].is_ascii_lowercase() {
                    let longer = std::str::from_utf8(&s[i..j + 1]).unwrap_or("");
                    let (lo, hi) = dict.prefix_range(longer);
                    if lo < hi {
                        continue;
                    }
                }
                let (lo, hi) = dict.prefix_range(piece);
                if lo < hi {
                    out[i].push(Token { end: j, lo, hi, kind: TokKind::Initial });
                }
                continue;
            }
            if j == n {
                let (lo, hi) = dict.prefix_range(piece);
                if lo < hi {
                    out[i].push(Token { end: j, lo, hi, kind: TokKind::Partial });
                }
            }
        }
    }
    out
}

/// 跳过分隔符。
pub fn skip_apos(s: &[u8], mut p: usize) -> usize {
    while p < s.len() && s[p] == b'\'' {
        p += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    fn dict() -> Dict {
        let syl = ["xi", "xian", "an", "a", "zhong", "zhou", "guo", "ju", "lve"];
        let mut b = DictBuilder::new(syl.map(String::from));
        b.add(&[0], "x", -1.0);
        Dict::from_bytes(b.build()).unwrap()
    }

    fn kinds(d: &Dict, s: &str, at: usize) -> Vec<(usize, TokKind)> {
        tokenize(d, s.as_bytes())[at].iter().map(|t| (t.end, t.kind)).collect()
    }

    #[test]
    fn finds_full_syllables_at_every_length() {
        let d = dict();
        assert_eq!(kinds(&d, "xian", 0), vec![(2, TokKind::Full), (4, TokKind::Full)]);
        assert_eq!(kinds(&d, "xian", 2), vec![(3, TokKind::Full), (4, TokKind::Full)]);
    }

    #[test]
    fn partial_only_at_end() {
        let d = dict();
        assert_eq!(kinds(&d, "zho", 0), vec![(3, TokKind::Partial)]);
        assert!(kinds(&d, "zhoxi", 0).iter().all(|t| t.1 != TokKind::Partial));
    }

    #[test]
    fn no_initial_when_next_letter_continues_the_syllable() {
        let d = dict();
        // x 后面是 i：xi 能拼成音节，所以 x 不当简拼
        assert!(!kinds(&d, "xian", 0).contains(&(1, TokKind::Initial)));
        // z 后面是 g：拼不成音节，z 是简拼
        assert!(kinds(&d, "zg", 0).contains(&(1, TokKind::Initial)));
    }

    #[test]
    fn aliases() {
        let d = dict();
        assert!(kinds(&d, "jv", 0).contains(&(2, TokKind::Full)));
        assert!(kinds(&d, "lue", 0).contains(&(3, TokKind::Full)));
    }

    #[test]
    fn apostrophe_splits() {
        let d = dict();
        let t = tokenize(&d, b"xi'an");
        assert!(t[0].iter().all(|t| t.end <= 2));
        assert_eq!(skip_apos(b"xi'an", 2), 3);
    }
}
