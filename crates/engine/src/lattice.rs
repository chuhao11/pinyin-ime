//! 由一串拼音生成候选。
//!
//! 1. 把输入切成所有可能的音节片段（pinyin.rs）。
//! 2. 从每个位置出发，沿着词库的字典树查出所有能拼上的字词，得到一张"词网"：
//!    每条边是"从位置 i 到位置 j 可以是这个词"。
//! 3. 在词网上找分数最高的一条整句（现在用词频，第三步换成语言模型）。
//! 4. 候选 = 整句 + 正好覆盖全部输入的字词 + 只覆盖开头一部分的字词。

use std::collections::HashSet;

use crate::dict::Dict;
use crate::pinyin::{skip_apos, tokenize, TokKind, Token};
use crate::userdb::{self, UserDb};

/// 一个词最多多少个音节。
pub const MAX_WORD_SYL: usize = 12;
/// 用声母简拼的音节扣分。
const PEN_INITIAL: f32 = -1.5;
/// 末尾没打完的音节扣分（边打边出字，扣得少）。
const PEN_PARTIAL: f32 = -0.5;
/// 实在拼不成拼音的字母，原样保留时的扣分。
const PEN_RAW: f32 = -25.0;
/// 同样覆盖全部输入时，多音节词每多一个音节的加分（让"西安"能排进 `xian` 的前几位）。
const MULTI_SYL_BONUS: f32 = 2.5;
/// 比同组最好的候选差这么多就算"质量太差"，候选条不硬凑。
const POOR_GAP: f32 = 10.0;
/// 用户自己造的、词库里没有的词的基础分。
const USER_BASE: f32 = -12.0;
/// 拼整句时，记忆加分最多算这么多（否则一个记住的词会把整句拉歪）。
const DP_BONUS_CAP: f32 = 4.0;
/// 候选总数上限。
pub const MAX_CANDS: usize = 200;
/// 整句不超过这么多字才记进用户词库。
const LEARN_SENTENCE_MAX: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandKind {
    /// 整句（由几个词拼成）
    Sentence,
    /// 单个字或词
    Word,
    /// 原样的字母
    Raw,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub text: String,
    /// 覆盖了输入的前多少个字节
    pub consumed: usize,
    /// 拼音（音节编号），用于记忆
    pub code: Vec<u16>,
    /// 排序分数
    pub score: f32,
    pub kind: CandKind,
    /// 比同组最好的差太多
    pub poor: bool,
    /// 选了以后要不要记住
    pub learnable: bool,
}

#[derive(Clone, Copy, Debug)]
enum Src {
    Dict(u32),
    User(u32),
}

#[derive(Clone, Debug)]
struct Hit {
    end: usize,
    src: Src,
    /// 词本身的分数（含简拼扣分）
    score: f32,
    /// 记忆加分
    bonus: f32,
    code: Vec<u16>,
}

impl Hit {
    /// 排候选时用：记忆加分全算
    fn rank_score(&self) -> f32 {
        self.score + self.bonus
    }
    /// 拼整句时用：记忆加分有上限
    fn dp_score(&self) -> f32 {
        self.score + self.bonus.min(DP_BONUS_CAP)
    }
}

struct Ctx<'a> {
    dict: &'a Dict,
    user: &'a UserDb,
    toks: &'a [Vec<Token>],
    s: &'a [u8],
    now: i64,
}

fn tok_pen(t: &Token) -> f32 {
    match t.kind {
        TokKind::Full => 0.0,
        TokKind::Initial => PEN_INITIAL,
        TokKind::Partial => PEN_PARTIAL,
    }
}

/// 沿词库字典树走。`all` 为真时收集节点上的所有字词，否则只要每个节点最好的那个。
fn dfs_dict(ctx: &Ctx, node: u32, pos: usize, pen: f32, code: &mut Vec<u16>, all: bool, out: &mut Vec<Hit>) {
    if !code.is_empty() {
        let ws = ctx.dict.node_words(node);
        if all {
            for w in ws {
                out.push(Hit {
                    end: pos,
                    src: Src::Dict(w),
                    score: ctx.dict.word_score(w) + pen,
                    bonus: 0.0,
                    code: code.clone(),
                });
            }
        } else if let Some(w) = ws.clone().next() {
            out.push(Hit {
                end: pos,
                src: Src::Dict(w),
                score: ctx.dict.word_score(w) + pen,
                bonus: 0.0,
                code: code.clone(),
            });
        }
    }
    if code.len() >= MAX_WORD_SYL {
        return;
    }
    let p = skip_apos(ctx.s, pos);
    if p >= ctx.s.len() {
        return;
    }
    for t in &ctx.toks[p] {
        let tp = pen + tok_pen(t);
        for e in ctx.dict.child_edges_in(node, t.lo, t.hi) {
            code.push(ctx.dict.edge_syl(e));
            dfs_dict(ctx, ctx.dict.edge_node(e), t.end, tp, code, all, out);
            code.pop();
        }
    }
}

/// 沿用户词库字典树走（用户词库里都是两个字以上的词）。
fn dfs_user(ctx: &Ctx, node: u32, pos: usize, pen: f32, code: &mut Vec<u16>, out: &mut Vec<Hit>) {
    if !code.is_empty() {
        for &i in ctx.user.node_entries(node) {
            let e = ctx.user.entry(i);
            let bonus = userdb::bonus(e.effective(ctx.now));
            out.push(Hit { end: pos, src: Src::User(i), score: USER_BASE + pen, bonus, code: code.clone() });
        }
    }
    if code.len() >= MAX_WORD_SYL {
        return;
    }
    let p = skip_apos(ctx.s, pos);
    if p >= ctx.s.len() {
        return;
    }
    for t in &ctx.toks[p] {
        let tp = pen + tok_pen(t);
        for &(syl, child) in ctx.user.children_in(node, t.lo, t.hi) {
            code.push(syl);
            dfs_user(ctx, child, t.end, tp, code, out);
            code.pop();
        }
    }
}

fn hit_text<'a>(ctx: &'a Ctx, h: &Hit) -> &'a str {
    match h.src {
        Src::Dict(w) => ctx.dict.word_text(w),
        Src::User(i) => &ctx.user.entry(i).text,
    }
}

#[derive(Clone, Copy)]
enum Back {
    Edge { from: usize, at: usize, k: usize },
    Raw { from: usize, at: usize },
    Skip { from: usize },
}

/// 生成候选。`input` 是还没上屏的那部分输入（原样，可以有大写和 `'`）。
pub fn analyze(dict: &Dict, user: &UserDb, input: &str, now: i64) -> Vec<Candidate> {
    if input.is_empty() || !input.is_ascii() {
        return Vec::new();
    }
    let lower = input.to_ascii_lowercase();
    let s = lower.as_bytes();
    let n = s.len();
    let toks = tokenize(dict, s);
    let ctx = Ctx { dict, user, toks: &toks, s, now };
    let start0 = skip_apos(s, 0);

    // ---- 词网：每个起点出发能查到的词 ----
    let mut first_hits: Vec<Hit> = Vec::new();
    let mut edges: Vec<Vec<Hit>> = vec![Vec::new(); n + 1];
    for p in 0..n {
        if toks[p].is_empty() {
            continue;
        }
        let all = p == start0;
        let mut hits = Vec::new();
        let mut code = Vec::new();
        dfs_dict(&ctx, dict.root(), p, 0.0, &mut code, all, &mut hits);
        dfs_user(&ctx, 0, p, 0.0, &mut code, &mut hits);
        // 删掉的词不出现
        hits.retain(|h| !user.is_blocked(&h.code, hit_text(&ctx, h)));
        if all {
            // 词库里的词如果你用过，加上记忆分（单字不加）
            for h in hits.iter_mut() {
                if let Src::Dict(w) = h.src {
                    let t = dict.word_text(w);
                    if t.chars().count() >= 2 {
                        h.bonus = user.bonus(&h.code, t, now);
                    }
                }
            }
            first_hits = hits.clone();
        }
        // 词网里每个终点只留最好的一条边
        let mut best: Vec<Hit> = Vec::new();
        for h in hits {
            match best.iter_mut().find(|b| b.end == h.end) {
                Some(b) if b.dp_score() >= h.dp_score() => {}
                Some(b) => *b = h,
                None => best.push(h),
            }
        }
        edges[p] = best;
    }

    // ---- 整句：在词网上找分数最高的路径 ----
    let mut dp = vec![f32::NEG_INFINITY; n + 1];
    let mut back: Vec<Option<Back>> = vec![None; n + 1];
    dp[start0] = 0.0;
    for p in 0..=n {
        if dp[p] == f32::NEG_INFINITY || p == n {
            continue;
        }
        let q = skip_apos(s, p);
        if q >= n {
            if dp[p] > dp[n] {
                dp[n] = dp[p];
                back[n] = Some(Back::Skip { from: p });
            }
            continue;
        }
        for (k, h) in edges[q].iter().enumerate() {
            let v = dp[p] + h.dp_score();
            if v > dp[h.end] {
                dp[h.end] = v;
                back[h.end] = Some(Back::Edge { from: p, at: q, k });
            }
        }
        let v = dp[p] + PEN_RAW;
        if v > dp[q + 1] {
            dp[q + 1] = v;
            back[q + 1] = Some(Back::Raw { from: p, at: q });
        }
    }
    let mut pieces: Vec<(String, Vec<u16>)> = Vec::new();
    let mut used_raw = false;
    let mut p = n;
    while p != start0 {
        match back[p] {
            Some(Back::Edge { from, at, k }) => {
                let h = &edges[at][k];
                pieces.push((hit_text(&ctx, h).to_string(), h.code.clone()));
                p = from;
            }
            Some(Back::Raw { from, at }) => {
                pieces.push((input[at..at + 1].to_string(), Vec::new()));
                used_raw = true;
                p = from;
            }
            Some(Back::Skip { from }) => p = from,
            None => break,
        }
    }
    pieces.reverse();

    // ---- 候选列表 ----
    let mut cands: Vec<Candidate> = Vec::new();
    let has_upper = input.bytes().any(|b| b.is_ascii_uppercase());
    if has_upper || used_raw {
        cands.push(Candidate {
            text: input.to_string(),
            consumed: n,
            code: Vec::new(),
            score: 0.0,
            kind: CandKind::Raw,
            poor: false,
            learnable: false,
        });
    }
    let word_pieces = pieces.iter().filter(|(_, c)| !c.is_empty()).count();
    if !used_raw && word_pieces >= 2 {
        let text: String = pieces.iter().map(|(t, _)| t.as_str()).collect();
        let code: Vec<u16> = pieces.iter().flat_map(|(_, c)| c.iter().copied()).collect();
        let chars = text.chars().count();
        cands.push(Candidate {
            text,
            consumed: n,
            code,
            score: dp[n],
            kind: CandKind::Sentence,
            poor: false,
            learnable: chars <= LEARN_SENTENCE_MAX,
        });
    }

    let covers_all = |end: usize| skip_apos(s, end) >= n;
    let rank = |h: &Hit| {
        let syl = h.code.len() as f32;
        h.rank_score() + if covers_all(h.end) { MULTI_SYL_BONUS * (syl - 1.0) } else { 0.0 }
    };
    let mut full: Vec<(f32, &Hit)> = first_hits.iter().filter(|h| covers_all(h.end)).map(|h| (rank(h), h)).collect();
    full.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut part: Vec<(f32, &Hit)> = first_hits.iter().filter(|h| !covers_all(h.end)).map(|h| (rank(h), h)).collect();
    part.sort_by(|a, b| b.1.end.cmp(&a.1.end).then(b.0.total_cmp(&a.0)));

    let mut group_best: Option<(usize, f32)> = None;
    for (r, h) in full.into_iter().chain(part) {
        let consumed = if covers_all(h.end) { n } else { h.end };
        let gb = match group_best {
            Some((c, b)) if c == consumed => b,
            _ => {
                group_best = Some((consumed, r));
                r
            }
        };
        cands.push(Candidate {
            text: hit_text(&ctx, h).to_string(),
            consumed,
            code: h.code.clone(),
            score: r,
            kind: CandKind::Word,
            poor: r < gb - POOR_GAP,
            learnable: true,
        });
    }

    // 记住的词如果比整句还好，排到整句前面（"选过一次，下次第一个就是它"）
    if let Some(si) = cands.iter().position(|c| c.kind == CandKind::Sentence) {
        if let Some(fi) = cands.iter().position(|c| c.kind == CandKind::Word && c.consumed == n) {
            let learned = first_hits.iter().any(|h| h.bonus > 0.0 && hit_text(&ctx, h) == cands[fi].text);
            let word_raw = first_hits
                .iter()
                .filter(|h| hit_text(&ctx, h) == cands[fi].text)
                .map(|h| h.rank_score())
                .fold(f32::NEG_INFINITY, f32::max);
            if learned && word_raw > cands[si].score {
                let w = cands.remove(fi);
                cands.insert(si, w);
            }
        }
    }

    // 去重（保留排在前面的），限制总数
    let mut seen = HashSet::new();
    cands.retain(|c| seen.insert(c.text.clone()));
    cands.truncate(MAX_CANDS);
    cands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    fn dict() -> Dict {
        let syl = ["xi", "an", "xian", "zai", "wo", "zhong", "guo", "ren", "fang", "fan", "gan"];
        let mut b = DictBuilder::new(syl.map(String::from));
        let mut add = |py: &str, t: &str, s: f32| {
            let c = b.code(py).unwrap();
            b.add(&c, t, s);
        };
        add("xian", "先", -6.0);
        add("xian", "现", -6.3);
        add("xian", "线", -7.5);
        add("xi", "西", -7.0);
        add("an", "安", -8.0);
        add("xi an", "西安", -9.5);
        add("xian zai", "现在", -7.0);
        add("zai", "在", -5.0);
        add("wo", "我", -4.5);
        add("zhong guo", "中国", -6.0);
        add("zhong", "中", -6.0);
        add("guo", "国", -6.5);
        add("ren", "人", -6.0);
        add("zhong guo ren", "中国人", -8.0);
        add("fang an", "方案", -7.0);
        add("fan gan", "反感", -9.0);
        let bytes = b.build();
        Dict::from_bytes(bytes).unwrap()
    }

    fn texts(d: &Dict, u: &UserDb, s: &str) -> Vec<String> {
        analyze(d, u, s, 0).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn mixes_segmentations_without_apostrophe() {
        let d = dict();
        let u = UserDb::in_memory();
        let t = texts(&d, &u, "xian");
        assert_eq!(t[0], "先");
        let pos = t.iter().position(|x| x == "西安").unwrap();
        assert!(pos <= 3, "{t:?}");
        // 打了分隔符，西安排第一
        assert_eq!(texts(&d, &u, "xi'an")[0], "西安");
        let t = texts(&d, &u, "fangan");
        assert_eq!(&t[..2], &["方案", "反感"]);
    }

    #[test]
    fn sentence_and_abbreviation() {
        let d = dict();
        let u = UserDb::in_memory();
        assert_eq!(texts(&d, &u, "woxianzai")[0], "我现在");
        assert_eq!(texts(&d, &u, "zg")[0], "中国");
        assert_eq!(texts(&d, &u, "zhongg")[0], "中国");
        assert_eq!(texts(&d, &u, "zhongguoren")[0], "中国人");
    }

    #[test]
    fn partial_candidates_come_after_full_ones() {
        let d = dict();
        let u = UserDb::in_memory();
        let c = analyze(&d, &u, "woxianzai", 0);
        assert_eq!(c[0].kind, CandKind::Sentence);
        assert!(c.iter().any(|c| c.text == "我" && c.consumed == 2));
    }

    #[test]
    fn unparseable_letters_give_raw_candidate() {
        let d = dict();
        let u = UserDb::in_memory();
        let c = analyze(&d, &u, "iii", 0);
        assert_eq!(c[0].kind, CandKind::Raw);
        assert_eq!(c[0].text, "iii");
        let c = analyze(&d, &u, "Xian", 0);
        assert_eq!(c[0].text, "Xian");
        assert_eq!(c[1].text, "先");
    }

    #[test]
    fn learned_word_goes_first() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let c = analyze(&d, &u, "fangan", 0);
        let fg = c.iter().find(|c| c.text == "反感").unwrap().clone();
        u.learn(&d, &fg.code, "反感", 0);
        assert_eq!(texts(&d, &u, "fangan")[0], "反感");
        // 用户自己造的新词（词库里没有）
        let code: Vec<u16> = ["xi", "an", "ren"].iter().map(|p| d.syllable_id(p).unwrap()).collect();
        u.learn(&d, &code, "西安人", 0);
        assert_eq!(texts(&d, &u, "xianren")[0], "西安人");
        assert_eq!(texts(&d, &u, "xianr")[0], "西安人");
    }

    #[test]
    fn forgotten_word_disappears() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let code = vec![d.syllable_id("xian").unwrap()];
        u.forget(&d, &code, "先");
        assert!(!texts(&d, &u, "xian").contains(&"先".to_string()));
    }
}
