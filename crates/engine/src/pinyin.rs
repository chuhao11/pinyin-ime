//! 把输入的字母串切成可能的音节片段。
//!
//! 每个位置可能有几种片段：
//! - 完整音节（`xian`、`xi`、`an`）
//! - 声母简拼（`z`、`zh`、`g`），可以代表所有以它开头的音节，用于 `zg`→中国
//! - 输入末尾没打完的音节（`zho` 代表 zhong/zhou）。末尾已经是完整音节时不往后猜（`zheba` 不会猜成"这般"）
//! - 打错纠正：按到旁边的键、两个字母打反、多打一个、漏打一个（`xu`→`xi`）。
//!   纠正要扣分，所以只有纠正后明显更通顺时才会被选上；你接受过的纠正，以后扣分会变少。
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

/// 用声母简拼的音节扣分。
pub const PEN_INITIAL: f32 = -1.5;
/// 末尾没打完的音节扣分。
pub const PEN_PARTIAL: f32 = -0.5;
/// 简拼把一个完整音节拆开了，再多扣这么多：`gun` 本身是完整音节，切成 gu + n（姑娘）、
/// `lin` 切成 li + n（理念）、`xiang` 切成 xian + g，都不是你想打的，除非你经常打那个词。
pub const PEN_SPLIT_SYLLABLE: f32 = -3.0;
/// 打错纠正的扣分：按到旁边的键、两个字母打反、多打一个、漏打一个。
pub const PEN_NEIGHBOR: f32 = -5.0;
pub const PEN_SWAP: f32 = -4.5;
pub const PEN_EXTRA: f32 = -5.5;
pub const PEN_MISSING: f32 = -6.0;
/// 你打的本来就是能用的拼音（比如 xu、nihao），要纠正时再多扣这么多：只有纠正后明显通顺得多才会被选上。
pub const PEN_SOFT_EXTRA: f32 = -1.5;
/// 你接受过的同类纠正，最多能少扣这么多。
pub const MAX_FIX_BONUS: f32 = 3.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Full,
    Initial,
    Partial,
    /// 打错纠正后的音节
    Fixed,
}

/// 一次纠正：把 `from` 改成 `to`（0 表示没有字母）。
/// 例如按错旁边的键 u→i 是 from=[u], to=[i]；打反 ab→ba；多打 x 是 from=[x], to=[]；漏打 h 是 from=[], to=[h]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fix {
    pub from: [u8; 2],
    pub to: [u8; 2],
}

impl Fix {
    fn part(b: &[u8; 2]) -> String {
        b.iter().filter(|&&c| c != 0).map(|&c| c as char).collect()
    }
    /// 记忆用的键，例如 ("u", "i")
    pub fn key(&self) -> (String, String) {
        (Self::part(&self.from), Self::part(&self.to))
    }
}

/// 从某个位置开始的一个音节片段：到 `end` 为止，代表音节编号 [lo, hi)。
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub end: usize,
    pub lo: u16,
    pub hi: u16,
    pub kind: TokKind,
    /// 扣分
    pub pen: f32,
    /// 纠正（只有 Fixed 才有）
    pub fix: Option<Fix>,
    /// 你打的这段本身就是能用的拼音（完整音节、声母、末尾没打完的音节）。
    /// 这种纠正只用在多字词里，免得单字候选里冒出一堆纠错字。
    pub soft: bool,
    /// 你打的这段如果本身是完整音节，它的编号（用来记住"这样打错就是想要这个词"）
    pub typed: Option<u16>,
    /// 跨两个音节的打反（zhdiao → zhi dao）：这个片段代表两个音节，第二个在这里
    pub second: Option<u16>,
}

const ROWS: [&[u8]; 3] = [b"qwertyuiop", b"asdfghjkl", b"zxcvbnm"];
const ROW_SHIFT: [f32; 3] = [0.0, 0.25, 0.75];

fn key_pos(c: u8) -> Option<(f32, f32)> {
    for (r, row) in ROWS.iter().enumerate() {
        if let Some(i) = row.iter().position(|&x| x == c) {
            return Some((r as f32, i as f32 + ROW_SHIFT[r]));
        }
    }
    None
}

/// 两个字母在 QWERTY 键盘上是不是挨着。
pub fn adjacent(a: u8, b: u8) -> bool {
    if a == b {
        return false;
    }
    match (key_pos(a), key_pos(b)) {
        (Some((ra, xa)), Some((rb, xb))) => {
            let (dr, dx) = ((ra - rb).abs(), (xa - xb).abs());
            (dr == 0.0 && dx <= 1.0) || (dr == 1.0 && dx <= 0.75)
        }
        _ => false,
    }
}

fn neighbors(c: u8) -> impl Iterator<Item = u8> {
    (b'a'..=b'z').filter(move |&x| adjacent(c, x))
}

fn full_id(dict: &Dict, piece: &str) -> Option<u16> {
    dict.syllable_id(piece).or_else(|| ALIASES.iter().find(|(a, _)| *a == piece).and_then(|(_, t)| dict.syllable_id(t)))
}

fn full_id_bytes(dict: &Dict, piece: &[u8]) -> Option<u16> {
    std::str::from_utf8(piece).ok().and_then(|p| full_id(dict, p))
}

/// 切分。`s` 必须已经转成小写，`'` 是分隔符。
/// `fix_bonus` 给出某种纠正你以前接受过多少（返回少扣的分数）。
pub fn tokenize(dict: &Dict, s: &[u8], fix_bonus: &dyn Fn(&Fix) -> f32) -> Vec<Vec<Token>> {
    let n = s.len();
    let mut out = vec![Vec::new(); n + 1];
    // 每个位置开始的完整音节在哪里结束（用来判断一段字母能不能整个拆成完整音节）
    let mut full_ends: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    for i in 0..n {
        for len in 1..=MAX_SYL_LEN {
            let j = i + len;
            if j > n || !s[j - 1].is_ascii_lowercase() {
                break;
            }
            if full_id_bytes(dict, &s[i..j]).is_some() {
                full_ends[i].push(j);
            }
        }
    }
    let decomposable = |i: usize, j: usize| -> bool {
        let mut reach = vec![false; j - i + 1];
        reach[0] = true;
        for a in i..j {
            if reach[a - i] {
                for &e in &full_ends[a] {
                    if e <= j {
                        reach[e - i] = true;
                    }
                }
            }
        }
        reach[j - i]
    };
    for i in 0..n {
        if !s[i].is_ascii_lowercase() {
            continue;
        }
        for len in 1..=MAX_SYL_LEN + 1 {
            let j = i + len;
            if j > n || !s[j - 1].is_ascii_lowercase() {
                break;
            }
            let piece_b = &s[i..j];
            let piece = std::str::from_utf8(piece_b).unwrap_or("");
            let mut soft = false;
            let mut typed = None;
            let mut partial_here = false;
            if len <= MAX_SYL_LEN {
                if let Some(id) = full_id(dict, piece) {
                    out[i].push(Token {
                        end: j,
                        lo: id,
                        hi: id + 1,
                        kind: TokKind::Full,
                        pen: 0.0,
                        fix: None,
                        soft: false,
                        typed: None,
                        second: None,
                    });
                    soft = true;
                    typed = Some(id);
                } else if INITIALS.contains(&piece) {
                    soft = true;
                    // 声母后面紧跟的字母能和它拼成音节（比如 g 后面是 a），说明你在打全拼，
                    // 这里不当简拼，否则会出现 fangan → "犯规啊" 这种怪候选。
                    let continues = j < n
                        && s[j].is_ascii_lowercase()
                        && std::str::from_utf8(&s[i..j + 1])
                            .map(|l| dict.prefix_range(l))
                            .is_ok_and(|(lo, hi)| lo < hi);
                    if !continues {
                        let (lo, hi) = dict.prefix_range(piece);
                        if lo < hi {
                            out[i].push(Token {
                                end: j,
                                lo,
                                hi,
                                kind: TokKind::Initial,
                                pen: PEN_INITIAL,
                                fix: None,
                                soft: false,
                                typed: None,
                                second: None,
                            });
                        }
                    }
                } else if j == n {
                    let (lo, hi) = dict.prefix_range(piece);
                    if lo < hi {
                        out[i].push(Token {
                            end: j,
                            lo,
                            hi,
                            kind: TokKind::Partial,
                            pen: PEN_PARTIAL,
                            fix: None,
                            soft: false,
                            typed: None,
                            second: None,
                        });
                        soft = true;
                        partial_here = true;
                    }
                }
            }
            if len >= 2 {
                // 能整个拆成完整音节的（nihao = ni + hao），也算"本来就能用"
                let soft = soft || decomposable(i, j);
                add_fixes(dict, piece_b, &s[i..], j, j == n, soft, partial_here, typed, fix_bonus, &mut out[i]);
            }
        }
    }
    // 跨两个音节的两个字母打反：zhdiao → zhi dao，xixeie → xie xie
    for i in 0..n {
        for m in i + 1..n.min(i + MAX_SYL_LEN + 1) {
            if !s[i..=m].iter().all(|b| b.is_ascii_lowercase()) {
                break;
            }
            if s[m - 1] == s[m] {
                continue;
            }
            let mut v1 = s[i..m - 1].to_vec();
            v1.push(s[m]);
            let Some(id1) = full_id_bytes(dict, &v1) else { continue };
            let fix = Fix { from: [s[m - 1], s[m]], to: [s[m], s[m - 1]] };
            for j in m + 1..=n.min(m + MAX_SYL_LEN) {
                if !s[j - 1].is_ascii_lowercase() {
                    break;
                }
                let mut v2 = vec![s[m - 1]];
                v2.extend_from_slice(&s[m + 1..j]);
                if let Some(id2) = full_id_bytes(dict, &v2) {
                    let soft = decomposable(i, j);
                    let pen =
                        PEN_SWAP + if soft { PEN_SOFT_EXTRA } else { 0.0 } + fix_bonus(&fix).clamp(0.0, MAX_FIX_BONUS);
                    out[i].push(Token {
                        end: j,
                        lo: id1,
                        hi: id1 + 1,
                        kind: TokKind::Fixed,
                        pen,
                        fix: Some(fix),
                        soft,
                        typed: None,
                        second: Some(id2),
                    });
                }
            }
        }
    }
    // 简拼（或没打完的音节）和前面的字母拼起来本身就是一个完整音节：说明这里本来是一个音节，被拆开了
    for (k, toks) in out.iter_mut().enumerate().take(n).skip(1) {
        for t in toks.iter_mut() {
            if !matches!(t.kind, TokKind::Initial | TokKind::Partial) {
                continue;
            }
            let splits =
                (k.saturating_sub(MAX_SYL_LEN)..k).any(|i| full_ends[i].contains(&k) && full_ends[i].contains(&t.end));
            if splits {
                t.pen += PEN_SPLIT_SYLLABLE;
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn add_fixes(
    dict: &Dict,
    piece: &[u8],
    rest: &[u8],
    end: usize,
    at_end: bool,
    soft: bool,
    partial_here: bool,
    typed: Option<u16>,
    fix_bonus: &dyn Fn(&Fix) -> f32,
    out: &mut Vec<Token>,
) {
    let len = piece.len();
    let mut found: Vec<(u16, f32, Fix)> = Vec::new();
    let mut consider = |v: &[u8], base: f32, fix: Fix| {
        // 末尾没打完的音节已经代表了所有以它开头的音节，不用再"补字母"
        if partial_here && at_end && v.starts_with(piece) {
            return;
        }
        // 改出来的音节其实原样就在输入里（比如 nih 去掉 h 是 ni，gan 的 n 就在后面），不算打错
        if rest.starts_with(v) {
            return;
        }
        if let Some(id) = full_id_bytes(dict, v) {
            if Some(id) == typed {
                return;
            }
            let pen = base + if soft { PEN_SOFT_EXTRA } else { 0.0 } + fix_bonus(&fix).clamp(0.0, MAX_FIX_BONUS);
            match found.iter_mut().find(|f| f.0 == id) {
                Some(f) if f.1 >= pen => {}
                Some(f) => *f = (id, pen, fix),
                None => found.push((id, pen, fix)),
            }
        }
    };
    let mut v: Vec<u8> = Vec::with_capacity(len + 1);
    if len <= MAX_SYL_LEN {
        // 按到旁边的键
        for k in 0..len {
            for c in neighbors(piece[k]) {
                v.clear();
                v.extend_from_slice(piece);
                v[k] = c;
                consider(&v, PEN_NEIGHBOR, Fix { from: [piece[k], 0], to: [c, 0] });
            }
        }
        // 两个字母打反
        for k in 0..len - 1 {
            if piece[k] != piece[k + 1] {
                v.clear();
                v.extend_from_slice(piece);
                v.swap(k, k + 1);
                consider(&v, PEN_SWAP, Fix { from: [piece[k], piece[k + 1]], to: [piece[k + 1], piece[k]] });
            }
        }
        // 漏打一个字母
        if len < MAX_SYL_LEN {
            for k in 0..=len {
                for c in b'a'..=b'z' {
                    v.clear();
                    v.extend_from_slice(&piece[..k]);
                    v.push(c);
                    v.extend_from_slice(&piece[k..]);
                    consider(&v, PEN_MISSING, Fix { from: [0, 0], to: [c, 0] });
                }
            }
        }
    }
    // 多打一个字母
    if len >= 3 {
        for k in 0..len {
            v.clear();
            v.extend_from_slice(&piece[..k]);
            v.extend_from_slice(&piece[k + 1..]);
            consider(&v, PEN_EXTRA, Fix { from: [piece[k], 0], to: [0, 0] });
        }
    }
    for (id, pen, fix) in found {
        out.push(Token {
            end,
            lo: id,
            hi: id + 1,
            kind: TokKind::Fixed,
            pen,
            fix: Some(fix),
            soft,
            typed,
            second: None,
        });
    }
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
        let syl = ["xi", "xian", "an", "a", "zhong", "zhou", "guo", "ju", "lve", "xu", "huan", "zhe", "ba", "ban"];
        let mut b = DictBuilder::new(syl.map(String::from));
        b.add(&[0], "x", -1.0);
        Dict::from_bytes(b.build()).unwrap()
    }

    fn no_bonus(_: &Fix) -> f32 {
        0.0
    }

    fn kinds(d: &Dict, s: &str, at: usize) -> Vec<(usize, TokKind)> {
        tokenize(d, s.as_bytes(), &no_bonus)[at]
            .iter()
            .filter(|t| t.kind != TokKind::Fixed)
            .map(|t| (t.end, t.kind))
            .collect()
    }

    fn fixed(d: &Dict, s: &str, at: usize) -> Vec<(usize, String)> {
        tokenize(d, s.as_bytes(), &no_bonus)[at]
            .iter()
            .filter(|t| t.kind == TokKind::Fixed)
            .map(|t| (t.end, d.syllable(t.lo).to_string()))
            .collect()
    }

    #[test]
    fn finds_full_syllables_at_every_length() {
        let d = dict();
        assert_eq!(kinds(&d, "xian", 0), vec![(2, TokKind::Full), (4, TokKind::Full)]);
        assert_eq!(kinds(&d, "xian", 2), vec![(3, TokKind::Full), (4, TokKind::Full)]);
    }

    #[test]
    fn partial_only_for_unfinished_syllable_at_end() {
        let d = dict();
        assert_eq!(kinds(&d, "zho", 0), vec![(3, TokKind::Partial)]);
        assert!(kinds(&d, "zhoxi", 0).iter().all(|t| t.1 != TokKind::Partial));
        // 末尾 ba 已经是完整音节：不猜成 ban
        assert!(kinds(&d, "zheba", 3).iter().all(|t| t.1 == TokKind::Full));
    }

    #[test]
    fn no_initial_when_next_letter_continues_the_syllable() {
        let d = dict();
        assert!(!kinds(&d, "xian", 0).contains(&(1, TokKind::Initial)));
        assert!(kinds(&d, "zg", 0).contains(&(1, TokKind::Initial)));
    }

    #[test]
    fn typo_fixes() {
        let d = dict();
        // u 和 i 挨着：xu 可以是 xi
        assert!(fixed(&d, "xuhuan", 0).contains(&(2, "xi".to_string())));
        // 打反：hunan → huan 不是打反；xina → xian 是打反（in→ni 的反面）
        assert!(fixed(&d, "xina", 0).contains(&(4, "xian".to_string())));
        // 漏打：xan → xian
        assert!(fixed(&d, "xan", 0).contains(&(3, "xian".to_string())));
        // 多打：xiian → xian
        assert!(fixed(&d, "xiian", 0).contains(&(5, "xian".to_string())));
        // 不挨着的键不算：xp 不会变成 xi（p 和 i 不挨着）
        assert!(!fixed(&d, "xp", 0).contains(&(2, "xi".to_string())));
    }

    #[test]
    fn swap_across_syllables() {
        let d = dict();
        // xi + an 打成 xain（i 和 a 跨音节打反）
        let t = tokenize(&d, b"xain", &no_bonus);
        assert!(t[0]
            .iter()
            .any(|t| t.second.is_some() && d.syllable(t.lo) == "xi" && d.syllable(t.second.unwrap()) == "an"));
    }

    #[test]
    fn keyboard_neighbors() {
        assert!(adjacent(b'u', b'i'));
        assert!(adjacent(b'a', b's'));
        assert!(adjacent(b'h', b'u'));
        assert!(adjacent(b'm', b'k'));
        assert!(!adjacent(b'a', b'p'));
        assert!(!adjacent(b'g', b'u'));
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
        let t = tokenize(&d, b"xi'an", &no_bonus);
        assert!(t[0].iter().all(|t| t.end <= 2));
        assert_eq!(skip_apos(b"xi'an", 2), 3);
    }
}
