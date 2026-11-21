//! 由一串拼音生成候选。
//!
//! 1. 把输入切成所有可能的音节片段（pinyin.rs），包括打错纠正后的片段。
//! 2. 从每个位置出发，沿着词库的字典树查出所有能拼上的字词，得到一张"词网"：
//!    每条边是"从位置 i 到位置 j 可以是这个词"。
//! 3. 在词网上找分数最高的若干条整句（现在用词频，第三步换成语言模型）。
//!    最好的一条当首选，其余的作为"组合"候选（好+胀 这种词库里没有的搭配）。
//! 4. 英文：输入能对上英文词（完全一样、补全、拼写纠错）时，按分数混进候选。
//! 5. 排序：整句 → 全部对上的字词 → 组合 → 只对上开头的字词 → 质量差的。

use std::collections::HashSet;

use crate::dict::Dict;
use crate::english::{edit_cost, match_case, max_cost, EnglishDict};
use crate::lm::{Lm, START_TOKEN};
use crate::pinyin::{skip_apos, tokenize, Fix, TokKind, Token};
use crate::userdb::{self, UserDb};

/// 一个词最多多少个音节。
pub const MAX_WORD_SYL: usize = 12;
/// 实在拼不成拼音的字母，原样保留时的扣分。
const PEN_RAW: f32 = -25.0;
/// 同样覆盖全部输入时，多音节词每多一个音节的加分（让"西安"能排进 `xian` 的前几位）。
/// 有语言模型时词的概率已经比较准，只加一点。
const MULTI_SYL_BONUS: f32 = 2.5;
const MULTI_SYL_BONUS_LM: f32 = 1.0;
/// 比同组最好的候选差这么多就算"质量太差"，候选条不硬凑。
const POOR_GAP: f32 = 10.0;
/// 全部对上的多字词：比最好的词差 4 以上、又比开头单字差 5 以上（比如 nihao 的"拟好"），挪到单字后面。
const WEAK_WORD_GAP: f32 = 4.0;
const WEAK_VS_PARTIAL_GAP: f32 = 5.0;
/// 用户自己造的、词库里没有的词的基础分。
const USER_BASE: f32 = -14.0;
/// 拼整句时，记忆加分最多算这么多（否则一个记住的词会把整句拉歪）。
const DP_BONUS_CAP: f32 = 4.0;
/// 候选总数上限。
pub const MAX_CANDS: usize = 200;
/// 整句不超过这么多字才记进用户词库。
const LEARN_SENTENCE_MAX: usize = 6;
/// 拼整句时，每个起点到每个终点留几个词（用来拼出多种组合）；输入短时多留一些。
const PER_END: usize = 6;
const PER_END_SHORT: usize = 16;
const SHORT_INPUT: usize = 12;
/// 整句保留多少种。
const K_PATHS: usize = 32;
/// 组合候选：比最好的整句差不超过这么多、且输入不超过 3 个音节时，放在单字前面，最多这么多个。
const COMBO_GAP: f32 = 6.0;
const COMBO_TOP: usize = 3;
const COMBO_TOP_MAX_SYL: usize = 3;
/// 再往后一档的组合：差不超过 9、输入不超过 4 个音节时，放在开头几个单字之后（矩阵第一页里能看到）。
const COMBO_MORE_GAP: f32 = 9.0;
const COMBO_MORE: usize = 20;
const COMBO_MORE_MAX_SYL: usize = 4;
/// 只对上开头的候选，先放这么多个，再放第二档组合。
const PARTIAL_HEAD: usize = 5;
/// 英文：你主要打中文，英文先扣分；输入本身就是完整的拼音时再多扣。
const EN_PRIOR: f32 = -3.0;
const EN_PRIOR_VALID_PINYIN: f32 = -3.0;
/// 你自己打过、词表里没有的英文词的基础分。
const EN_USER_BASE: f32 = -11.0;
const MAX_EN: usize = 8;
/// 上次选过、但现在排在第 4 个以后的字词，挪到第 4 个（候选条至少显示 4 个，一眼能看到）。
const RECENT_SLOT: usize = 3;
/// 多久以内选过的才算"上次选过"。
const RECENT_SECS: i64 = 60 * 86400;
/// 打的正好是一个完整音节时，前几个位置留给这个音的单字（`gun` → 滚 棍 衮，然后才是"姑娘"这种词）。
const SINGLE_HEAD: usize = 3;
/// 你经常打的词（有效次数达到这么多）不受上面的限制，可以排在单字前面。
const OFTEN_USED: f64 = 2.0;
/// 单字"几十次就改"：一个字你选了这么多次，而且比现在排第一的字多选一半以上，它就排第一。
const CHAR_TAKEOVER: f64 = 20.0;
const CHAR_TAKEOVER_RATIO: f64 = 1.5;
/// 上文（刚上屏的词）对下一个字词分数的影响上限：上文只能微调，不能让一个不相干的字突然排第一。
/// 单字更严：单字的顺序要你自己选几十次才会变。
const CTX_UP_CHAR: f32 = 1.0;
const CTX_UP_WORD: f32 = 2.5;
const CTX_DOWN: f32 = 3.0;
/// 真正常见的搭配（前一个词后面接它的概率至少 5%，比如 走→吧、数字→年）上限放宽到这么多
const CTX_UP_STRONG: f32 = 3.0;
const STRONG_PAIR: f32 = -3.0;
/// 上一个字词和这个字词连起来本身就是一个词（谢+谢、吃+饭、走+吧、滚+滚）：最可靠的前后关联，加这么多。
/// （语言模型按词统计，"谢谢"是一个词，所以"谢"后面接"谢"在统计里反而少见，不能只看统计。）
const CTX_JOINED: f32 = 2.0;
/// 连起来的词至少要这么常见才算（太生僻的组合不算）
const JOINED_MIN_UNI: f32 = -15.0;
/// 一个词里最多纠正几处。
const MAX_FIXES_PER_WORD: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandKind {
    /// 整句或组合（由几个词拼成）
    Sentence,
    /// 单个字或词
    Word,
    /// 原样的字母
    Raw,
    /// 英文单词
    English,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub text: String,
    /// 覆盖了输入的前多少个字节
    pub consumed: usize,
    /// 拼音（音节编号），用于记忆
    pub code: Vec<u16>,
    /// 排序分数（含记忆加分）
    pub score: f32,
    /// 不含记忆加分的分数（判断"本来该排第一的是谁"）
    pub base: f32,
    pub kind: CandKind,
    /// 质量差，候选条到它为止
    pub poor: bool,
    /// 选了以后要不要记住
    pub learnable: bool,
    /// 这个候选用到的打错纠正
    pub fixes: Vec<Fix>,
    /// 打错时你实际打的拼音（都是完整音节时才有），用来记住"这样打错就是想要这个"
    pub typed_code: Option<Vec<u16>>,
    /// 最后一个词在语言模型里的编号（上屏后当下一次输入的上文）
    pub last_lm: Option<u32>,
}

impl Candidate {
    pub fn corrected(&self) -> bool {
        !self.fixes.is_empty()
    }

    fn simple(text: String, consumed: usize, score: f32, kind: CandKind) -> Self {
        Candidate {
            text,
            consumed,
            code: Vec::new(),
            score,
            base: score,
            kind,
            poor: false,
            learnable: false,
            fixes: Vec::new(),
            typed_code: None,
            last_lm: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Src {
    Dict(u32),
    User(u32),
}

/// 一次纠正：在词的第几个音节、改了什么、你打的那个音节是什么。
type FixAt = (usize, Fix, Option<u16>);

#[derive(Clone, Debug)]
struct Hit {
    end: usize,
    src: Src,
    /// 词本身的分数（单词概率 + 简拼、纠错扣分）
    score: f32,
    /// 简拼、纠错扣分
    pen: f32,
    /// 在语言模型里的编号
    lm: Option<u32>,
    /// 这个读音占这个字（词）所有读音的比例（对数）。语言模型按文字统计，分不出读音，要靠它
    rd: f32,
    /// 记忆加分
    bonus: f32,
    code: Vec<u16>,
    fixes: Vec<FixAt>,
    /// 全是完整音节、没有纠正
    exact: bool,
}

impl Hit {
    fn rank_score(&self) -> f32 {
        self.score + self.bonus
    }
    fn dp_score(&self) -> f32 {
        self.score + self.bonus.min(DP_BONUS_CAP)
    }
    /// 整句里：前一个词是 `prev` 时的分数（完整的语言模型）。
    fn after(&self, lm: Option<&Lm>, prev: Option<u32>) -> f32 {
        match (lm, prev, self.lm) {
            (Some(lm), Some(p), Some(w)) => lm.logp(p, w) + self.rd + self.pen,
            _ => self.score,
        }
    }

    /// 排候选时：刚上屏的词是 `prev`，在单独的分数上按前后关联加减一点（有上限）。
    /// - 连起来本身是一个词（谢谢、吃饭）：加 CTX_JOINED
    /// - 统计里真的一起出现过：按搭配好坏加减，有上限
    /// - 统计里没见过：不扣分（没见过多半是因为它们平时连成一个词，不是搭配差）
    fn after_ctx(&self, lm: Option<&Lm>, prev: Option<u32>, text: &str) -> f32 {
        let (Some(lm), Some(p)) = (lm, prev) else { return self.score };
        let up = if self.code.len() == 1 { CTX_UP_CHAR } else { CTX_UP_WORD };
        let pair = match self.lm.and_then(|w| lm.bigram(p, w).map(|b| (b, lm.uni(w)))) {
            Some((b, u)) => (b - u).clamp(-CTX_DOWN, if b >= STRONG_PAIR { CTX_UP_STRONG } else { up }),
            None => 0.0,
        };
        let prev_text = lm.word(p);
        let joined = !prev_text.starts_with('<')
            && lm.id(&format!("{prev_text}{text}")).is_some_and(|j| lm.uni(j) >= JOINED_MIN_UNI);
        self.score + if joined { pair.max(CTX_JOINED) } else { pair }
    }
    fn typed_code(&self) -> Option<Vec<u16>> {
        if self.fixes.is_empty() {
            return None;
        }
        let mut c = self.code.clone();
        for &(i, _, typed) in &self.fixes {
            c[i] = typed?;
        }
        Some(c)
    }
}

struct Ctx<'a> {
    dict: &'a Dict,
    lm: Option<&'a Lm>,
    user: &'a UserDb,
    toks: &'a [Vec<Token>],
    s: &'a [u8],
    now: i64,
}

#[derive(Default)]
struct Walk {
    code: Vec<u16>,
    fixes: Vec<FixAt>,
    soft: usize,
    inexact: usize,
}

impl Walk {
    /// 纠正了"本来就能用的拼音"的，只在多字词里算数
    fn can_emit(&self) -> bool {
        !self.code.is_empty() && !(self.soft > 0 && self.code.len() < 2)
    }
    fn enter(&mut self, syl: u16, t: &Token) {
        self.code.push(syl);
        if t.kind != TokKind::Full {
            self.inexact += 1;
        }
        if let Some(f) = t.fix {
            self.fixes.push((self.code.len() - 1, f, t.typed));
            if t.soft {
                self.soft += 1;
            }
        }
    }
    fn leave(&mut self, t: &Token) {
        self.code.pop();
        if t.kind != TokKind::Full {
            self.inexact -= 1;
        }
        if t.fix.is_some() {
            self.fixes.pop();
            if t.soft {
                self.soft -= 1;
            }
        }
    }
    fn usable(&self, t: &Token) -> bool {
        t.fix.is_none() || self.fixes.len() < MAX_FIXES_PER_WORD
    }
}

/// 单字的分数里，"单独打这个字的频率"（语言模型）占多少；其余是词库里的总频率（含在词里出现的）。
const SINGLE_LM_SHARE: f32 = 0.7;

/// ln(w·e^a + (1−w)·e^b)
fn log_mix(a: f32, b: f32, w: f32) -> f32 {
    let m = a.max(b);
    m + (w * (a - m).exp() + (1.0 - w) * (b - m).exp()).ln()
}

/// 词的单词分数和语言模型编号。
/// - 没有语言模型：用词库分数。
/// - 单字：70% 看"单独打这个字有多常见"（了 比 来 常见、你 比 那 常见），30% 看词库里的总频率。
/// - 多字词：语言模型的概率 × 这个读音的比例；不在模型词表里的用词库分数换算。
fn word_uni(ctx: &Ctx, text: &str, dict_score: f32, reading: f32) -> (Option<u32>, f32) {
    let Some(lm) = ctx.lm else { return (None, dict_score) };
    let id = lm.id(text);
    let single = text.chars().nth(1).is_none();
    match id {
        Some(id) if single => (Some(id), log_mix(lm.uni(id) + reading, dict_score, SINGLE_LM_SHARE)),
        None if single => (None, dict_score),
        Some(id) => (Some(id), lm.uni(id) + reading),
        None => (None, lm.fallback(dict_score)),
    }
}

/// 沿词库字典树走，每个节点最多收 `limit` 个字词。
fn dfs_dict(ctx: &Ctx, node: u32, pos: usize, pen: f32, w: &mut Walk, limit: usize, out: &mut Vec<Hit>) {
    if w.can_emit() {
        for wi in ctx.dict.node_words(node).take(limit) {
            let rd = ctx.dict.word_reading(wi);
            let (lm, uni) = word_uni(ctx, ctx.dict.word_text(wi), ctx.dict.word_score(wi), rd);
            out.push(Hit {
                end: pos,
                src: Src::Dict(wi),
                score: uni + pen,
                pen,
                lm,
                rd,
                bonus: 0.0,
                code: w.code.clone(),
                fixes: w.fixes.clone(),
                exact: w.inexact == 0,
            });
        }
    }
    if w.code.len() >= MAX_WORD_SYL {
        return;
    }
    let p = skip_apos(ctx.s, pos);
    if p >= ctx.s.len() {
        return;
    }
    for t in &ctx.toks[p] {
        if !w.usable(t) {
            continue;
        }
        for e in ctx.dict.child_edges_in(node, t.lo, t.hi) {
            w.enter(ctx.dict.edge_syl(e), t);
            match t.second {
                None => dfs_dict(ctx, ctx.dict.edge_node(e), t.end, pen + t.pen, w, limit, out),
                Some(sec) => {
                    for e2 in ctx.dict.child_edges_in(ctx.dict.edge_node(e), sec, sec + 1) {
                        w.code.push(sec);
                        dfs_dict(ctx, ctx.dict.edge_node(e2), t.end, pen + t.pen, w, limit, out);
                        w.code.pop();
                    }
                }
            }
            w.leave(t);
        }
    }
}

/// 沿用户词库字典树走（用户词库里都是两个字以上的词）。
fn dfs_user(ctx: &Ctx, node: u32, pos: usize, pen: f32, w: &mut Walk, out: &mut Vec<Hit>) {
    if w.can_emit() {
        for &i in ctx.user.node_entries(node) {
            let e = ctx.user.entry(i);
            let (lm, uni) = match ctx.lm.and_then(|l| l.id(&e.text).map(|id| (id, l.uni(id)))) {
                Some((id, u)) => (Some(id), u.max(USER_BASE)),
                None => (None, USER_BASE),
            };
            out.push(Hit {
                end: pos,
                src: Src::User(i),
                score: uni + pen,
                pen,
                lm,
                rd: 0.0,
                bonus: userdb::bonus(e.effective(ctx.now)),
                code: w.code.clone(),
                fixes: w.fixes.clone(),
                exact: w.inexact == 0,
            });
        }
    }
    if w.code.len() >= MAX_WORD_SYL {
        return;
    }
    let p = skip_apos(ctx.s, pos);
    if p >= ctx.s.len() {
        return;
    }
    for t in &ctx.toks[p] {
        if !w.usable(t) {
            continue;
        }
        for &(syl, child) in ctx.user.children_in(node, t.lo, t.hi) {
            w.enter(syl, t);
            match t.second {
                None => dfs_user(ctx, child, t.end, pen + t.pen, w, out),
                Some(sec) => {
                    for &(s2, child2) in ctx.user.children_in(child, sec, sec + 1) {
                        w.code.push(s2);
                        dfs_user(ctx, child2, t.end, pen + t.pen, w, out);
                        w.code.pop();
                    }
                }
            }
            w.leave(t);
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
enum Step {
    Start,
    Edge { at: usize, k: usize },
    Raw { at: usize },
    Skip,
}

#[derive(Clone, Copy)]
struct PathEnt {
    score: f32,
    prev: usize,
    prev_k: usize,
    step: Step,
    /// 这条路径最后一个词（语言模型编号）
    last: Option<u32>,
}

fn push_k(list: &mut Vec<PathEnt>, e: PathEnt) {
    if list.len() >= K_PATHS && list.last().is_some_and(|l| e.score <= l.score) {
        return;
    }
    let i = list.partition_point(|x| x.score >= e.score);
    list.insert(i, e);
    list.truncate(K_PATHS);
}

/// 一条整句。
struct Path {
    text: String,
    code: Vec<u16>,
    pieces: usize,
    raw: bool,
    exact: bool,
    score: f32,
    fixes: Vec<Fix>,
    typed_code: Option<Vec<u16>>,
    syllables: usize,
    last_lm: Option<u32>,
}

/// 生成候选要用到的东西。
pub struct Query<'a> {
    pub dict: &'a Dict,
    pub en: Option<&'a EnglishDict>,
    pub lm: Option<&'a Lm>,
    pub user: &'a UserDb,
    /// 上文：刚上屏的最后一个词（语言模型编号）
    pub prev: Option<u32>,
    pub now: i64,
}

/// 生成候选（不用语言模型、没有上文）。
pub fn analyze(dict: &Dict, en: Option<&EnglishDict>, user: &UserDb, input: &str, now: i64) -> Vec<Candidate> {
    analyze_with(&Query { dict, en, lm: None, user, prev: None, now }, input)
}

/// 生成候选。`input` 是还没上屏的那部分输入（原样，可以有大写和 `'`）。
pub fn analyze_with(q: &Query, input: &str) -> Vec<Candidate> {
    let (dict, en, lm, user, now) = (q.dict, q.en, q.lm, q.user, q.now);
    if input.is_empty() || !input.is_ascii() {
        return Vec::new();
    }
    let lower = input.to_ascii_lowercase();
    let s = lower.as_bytes();
    let n = s.len();
    let fix_bonus = |f: &Fix| {
        let (a, b) = f.key();
        (1.2 * (1.0 + user.fix_count(&a, &b, now)).ln()) as f32
    };
    let toks = tokenize(dict, s, &fix_bonus);
    let ctx = Ctx { dict, lm, user, toks: &toks, s, now };
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
        let mut w = Walk::default();
        let per_end = if n <= SHORT_INPUT { PER_END_SHORT } else { PER_END };
        dfs_dict(&ctx, dict.root(), p, 0.0, &mut w, if all { usize::MAX } else { per_end }, &mut hits);
        dfs_user(&ctx, 0, p, 0.0, &mut w, &mut hits);
        // 删掉的词不出现
        hits.retain(|h| !user.is_blocked(&h.code, hit_text(&ctx, h)));
        // 词库里的词如果你用过，加上记忆分；单字的记忆分很小，要选几十次才会换位置
        for h in hits.iter_mut() {
            if let Src::Dict(wi) = h.src {
                let t = dict.word_text(wi);
                if t.chars().nth(1).is_some() {
                    h.bonus = user.bonus(&h.code, t, now);
                } else if h.code.len() == 1 {
                    h.bonus = user.char_bonus(h.code[0], t, now);
                }
            }
        }
        if all {
            // 排候选时看上文（刚上屏的词）
            first_hits = hits.clone();
            let prev_text = lm.zip(q.prev).map(|(l, p)| l.word(p)).filter(|t| !t.starts_with('<'));
            for h in first_hits.iter_mut() {
                let t = hit_text(&ctx, h);
                h.score = h.after_ctx(lm, q.prev, t);
                // 你在同样的上文后面选过它：记忆加分（比上文本身的影响大）
                if let Some(pt) = prev_text {
                    h.bonus += user.pair_bonus(pt, t, now);
                }
            }
        }
        // 每个终点只留最好的几条边
        hits.sort_by(|a, b| b.end.cmp(&a.end).then(b.dp_score().total_cmp(&a.dp_score())));
        let mut kept: Vec<Hit> = Vec::new();
        let (mut cur, mut cnt) = (usize::MAX, 0);
        for h in hits {
            if h.end != cur {
                cur = h.end;
                cnt = 0;
            }
            if cnt < per_end {
                kept.push(h);
                cnt += 1;
            }
        }
        edges[p] = kept;
    }

    // ---- 整句：在词网上找分数最高的若干条路径 ----
    let mut paths: Vec<Vec<PathEnt>> = vec![Vec::new(); n + 1];
    // 整句从上文开始；没有上文就从"一段的开头"开始（候选列表里的单字不受影响，l 还是"了"）
    let start_ctx = q.prev.or_else(|| lm.and_then(|l| l.id(START_TOKEN)));
    paths[start0].push(PathEnt { score: 0.0, prev: 0, prev_k: 0, step: Step::Start, last: start_ctx });
    for p in 0..n {
        if paths[p].is_empty() {
            continue;
        }
        let q = skip_apos(s, p);
        let entries = paths[p].clone();
        for (ki, e) in entries.iter().enumerate() {
            if q >= n {
                push_k(&mut paths[n], PathEnt { score: e.score, prev: p, prev_k: ki, step: Step::Skip, last: e.last });
                continue;
            }
            for (k, h) in edges[q].iter().enumerate() {
                // 有语言模型时，看前一个词和这个词的搭配
                let sc = h.after(lm, e.last) + h.bonus.min(DP_BONUS_CAP);
                let ent =
                    PathEnt { score: e.score + sc, prev: p, prev_k: ki, step: Step::Edge { at: q, k }, last: h.lm };
                push_k(&mut paths[h.end], ent);
            }
            let raw = PathEnt { score: e.score + PEN_RAW, prev: p, prev_k: ki, step: Step::Raw { at: q }, last: None };
            push_k(&mut paths[q + 1], raw);
        }
    }
    let extract = |k: usize| -> Path {
        let mut pieces: Vec<(String, Option<&Hit>)> = Vec::new();
        let (mut pos, mut idx) = (n, k);
        let score = paths[n][k].score;
        loop {
            let e = paths[pos][idx];
            match e.step {
                Step::Start => break,
                Step::Edge { at, k } => {
                    let h = &edges[at][k];
                    pieces.push((hit_text(&ctx, h).to_string(), Some(h)));
                }
                Step::Raw { at } => pieces.push((input[at..at + 1].to_string(), None)),
                Step::Skip => {}
            }
            pos = e.prev;
            idx = e.prev_k;
        }
        pieces.reverse();
        let mut p = Path {
            text: String::new(),
            code: Vec::new(),
            pieces: 0,
            raw: false,
            exact: true,
            score,
            fixes: Vec::new(),
            typed_code: Some(Vec::new()),
            syllables: 0,
            last_lm: None,
        };
        let mut any_fix = false;
        for (t, h) in pieces {
            p.text.push_str(&t);
            match h {
                Some(h) => {
                    p.pieces += 1;
                    p.last_lm = h.lm;
                    p.code.extend_from_slice(&h.code);
                    p.syllables += h.code.len();
                    p.exact &= h.exact;
                    p.fixes.extend(h.fixes.iter().map(|f| f.1));
                    any_fix |= !h.fixes.is_empty();
                    let tc = if h.fixes.is_empty() { Some(h.code.clone()) } else { h.typed_code() };
                    match (&mut p.typed_code, tc) {
                        (Some(all), Some(c)) => all.extend(c),
                        _ => p.typed_code = None,
                    }
                }
                None => {
                    p.raw = true;
                    p.exact = false;
                    p.last_lm = None;
                }
            }
        }
        if !any_fix {
            p.typed_code = None;
        }
        p
    };
    let best = if paths[n].is_empty() { None } else { Some(extract(0)) };
    // 不纠错能不能拼成拼音（不能的话多半是英文或别的，原样字母排第一）
    let plain_needs_raw = {
        let mut ok = vec![false; n + 1];
        ok[start0] = true;
        for p in 0..n {
            if !ok[p] {
                continue;
            }
            let q = skip_apos(s, p);
            if q >= n {
                ok[n] = true;
                continue;
            }
            for h in edges[q].iter().filter(|h| h.fixes.is_empty()) {
                ok[h.end] = true;
            }
        }
        !ok[n]
    };

    // ---- 候选列表 ----
    let mut out: Vec<Candidate> = Vec::new();
    let mut tail: Vec<Candidate> = Vec::new();
    let has_upper = input.bytes().any(|b| b.is_ascii_uppercase());
    let used_raw = best.as_ref().is_none_or(|b| b.raw);
    if has_upper || used_raw || plain_needs_raw {
        out.push(Candidate::simple(input.to_string(), n, 0.0, CandKind::Raw));
    }
    let sentence_cand = |p: &Path| Candidate {
        text: p.text.clone(),
        consumed: n,
        code: p.code.clone(),
        score: p.score,
        base: p.score,
        kind: CandKind::Sentence,
        poor: false,
        learnable: p.text.chars().count() <= LEARN_SENTENCE_MAX,
        fixes: p.fixes.clone(),
        typed_code: p.typed_code.clone(),
        last_lm: p.last_lm,
    };
    if let Some(b) = &best {
        if !b.raw && b.pieces >= 2 {
            if plain_needs_raw && !b.fixes.is_empty() {
                // 不像拼音的输入，纠错硬拼出来的整句放后面
                let mut c = sentence_cand(b);
                c.poor = true;
                tail.push(c);
            } else {
                out.push(sentence_cand(b));
            }
        }
    }

    // ---- 英文 ----
    let letters_only = s.iter().all(|b| b.is_ascii_lowercase());
    let mut ens: Vec<Candidate> = Vec::new();
    if letters_only && n >= 2 {
        let valid_pinyin = best.as_ref().is_some_and(|b| b.exact && !b.raw);
        let prior = EN_PRIOR + if valid_pinyin && !has_upper { EN_PRIOR_VALID_PINYIN } else { 0.0 };
        let mut add = |text: &str, key: &str, score: f32| {
            let b =
                user.english().get(key).map(|e| userdb::bonus(userdb::effective(e.freq, e.last, now))).unwrap_or(0.0);
            let mut c = Candidate::simple(match_case(text, input), n, score + prior + b, CandKind::English);
            c.base = score + prior;
            c.learnable = true;
            ens.push(c);
        };
        if let Some(en) = en {
            for h in en.search(s) {
                let w = en.word(h.word);
                add(&w.text, &w.text.to_ascii_lowercase(), w.score + h.penalty);
            }
        }
        // 你打过的、词表里没有的英文词
        let maxc = max_cost(n);
        for (key, e) in user.english() {
            let kb = key.as_bytes();
            let pen = if kb == s {
                Some(0.0)
            } else if n >= 3 && kb.starts_with(s) {
                Some(-1.5 - 0.15 * (kb.len() - n) as f32)
            } else if maxc > 0.0 && kb.len().abs_diff(n) <= 2 {
                let c = edit_cost(s, kb);
                (c <= maxc).then_some(c * -4.0)
            } else {
                None
            };
            if let Some(p) = pen {
                add(&e.text, key, EN_USER_BASE + p);
            }
        }
        ens.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut seen = HashSet::new();
        ens.retain(|c| seen.insert(c.text.clone()));
        ens.truncate(MAX_EN);
    }
    let en_best = ens.first().map(|c| c.score).unwrap_or(f32::NEG_INFINITY);

    let covers_all = |end: usize| skip_apos(s, end) >= n;
    let msb = if lm.is_some() { MULTI_SYL_BONUS_LM } else { MULTI_SYL_BONUS };
    let multi = |h: &Hit| if covers_all(h.end) { msb * (h.code.len() as f32 - 1.0) } else { 0.0 };
    let word_cand = |h: &Hit, poor: bool| Candidate {
        text: hit_text(&ctx, h).to_string(),
        consumed: if covers_all(h.end) { n } else { h.end },
        code: h.code.clone(),
        score: h.rank_score() + multi(h),
        base: h.score + multi(h),
        kind: CandKind::Word,
        poor,
        learnable: true,
        fixes: h.fixes.iter().map(|f| f.1).collect(),
        typed_code: h.typed_code(),
        last_lm: h.lm,
    };
    let mut full: Vec<&Hit> = first_hits.iter().filter(|h| covers_all(h.end)).collect();
    full.sort_by(|a, b| (b.rank_score() + multi(b)).total_cmp(&(a.rank_score() + multi(a))));
    // 只对上开头一部分的候选不用纠错（纠错只用在能把整串对上的时候）
    let mut part: Vec<&Hit> = first_hits.iter().filter(|h| !covers_all(h.end) && h.fixes.is_empty()).collect();
    part.sort_by(|a, b| b.end.cmp(&a.end).then(b.rank_score().total_cmp(&a.rank_score())));
    let best_full_base = full.iter().map(|h| h.score + multi(h)).fold(f32::NEG_INFINITY, f32::max);
    // 不纠错的候选里最好的分数：纠错候选只有比它还好才放前面（haozhang 不能被改成"保障"）
    // 不纠错的整句里最好的那条（也比较英文）：纠错的词只有比它还好才放前面
    let best_plain = (0..paths[n].len())
        .map(&extract)
        .find(|p| p.fixes.is_empty() && !p.raw)
        .map(|p| p.score)
        .unwrap_or(f32::NEG_INFINITY)
        .max(en_best);
    let best_part_base = part.iter().map(|h| h.score).fold(f32::NEG_INFINITY, f32::max);

    // 全部对上的字词
    for h in &full {
        let base = h.score + multi(h);
        let multi_char = hit_text(&ctx, h).chars().count() >= 2;
        let poor = base < best_full_base - POOR_GAP;
        let unneeded_fix = !h.fixes.is_empty() && h.rank_score() < best_plain;
        let weak = unneeded_fix
            || multi_char
                && h.bonus == 0.0
                && base < best_full_base - WEAK_WORD_GAP
                && h.score < best_part_base - WEAK_VS_PARTIAL_GAP;
        if poor || weak {
            tail.push(word_cand(h, true));
        } else {
            out.push(word_cand(h, false));
        }
    }
    // 组合（好+胀 这种）
    let mut combo_more: Vec<Candidate> = Vec::new();
    if let Some(b) = &best {
        let mut top = 0;
        for k in 1..paths[n].len() {
            let p = extract(k);
            if p.raw || p.pieces < 2 {
                continue;
            }
            let near = p.score >= b.score - COMBO_GAP && b.syllables <= COMBO_TOP_MAX_SYL;
            let more = p.score >= b.score - COMBO_MORE_GAP && b.syllables <= COMBO_MORE_MAX_SYL;
            let mut c = sentence_cand(&p);
            if near && top < COMBO_TOP {
                out.push(c);
                top += 1;
            } else if more && combo_more.len() < COMBO_MORE {
                c.poor = true;
                combo_more.push(c);
            } else {
                c.poor = true;
                tail.push(c);
            }
        }
    }
    // 只对上开头的字词
    let full_end = out.len();
    let mut group: Option<(usize, f32)> = None;
    let mut part_poor = Vec::new();
    let mut part_n = 0;
    for h in &part {
        let gb = match group {
            Some((e, b)) if e == h.end => b,
            _ => {
                group = Some((h.end, h.score));
                h.score
            }
        };
        if h.score < gb - POOR_GAP {
            part_poor.push(word_cand(h, true));
        } else {
            out.push(word_cand(h, false));
            part_n += 1;
            if part_n == PARTIAL_HEAD {
                // 开头几个单字之后，放第二档组合（它们标成"差"，候选条到这里为止）
                out.append(&mut combo_more);
            }
        }
    }
    // 单字不够 5 个的，第二档组合排在单字后面
    out.append(&mut combo_more);

    // 英文插进"全部对上"的那一段：按分数找位置；原样字母永远在最前
    {
        let lo = usize::from(out.first().is_some_and(|c| c.kind == CandKind::Raw));
        for (end, c) in (full_end..).zip(ens) {
            let pos = (lo..end).find(|&i| out[i].score < c.score).unwrap_or(end);
            out.insert(pos, c);
        }
    }

    out.extend(tail);
    out.extend(part_poor);
    // 去重（保留排在前面的）
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.text.clone()));
    // 打的正好是一个完整音节（gun、lin、xian）：前 3 个留给这个音的常用单字，
    // 词（姑娘、理念、西安）排在后面——除非你经常打那个词
    let one_syllable = !has_upper && toks[start0].iter().any(|t| t.kind == TokKind::Full && t.end == n);
    if one_syllable {
        let is_single = |c: &Candidate| {
            c.kind == CandKind::Word
                && c.code.len() == 1
                && c.text.chars().nth(1).is_none()
                && c.fixes.is_empty()
                && c.consumed == n
                && !c.poor
        };
        let often = |c: &Candidate| {
            matches!(c.kind, CandKind::Word | CandKind::Sentence)
                && user.lookup(&c.code, &c.text).is_some_and(|e| e.effective(now) >= OFTEN_USED)
        };
        // 前几个单字所在的范围
        if let Some(last) =
            out.iter().enumerate().filter(|(_, c)| is_single(c)).take(SINGLE_HEAD).map(|(i, _)| i).last()
        {
            let head: Vec<Candidate> = out.drain(..=last).collect();
            let (keep, later): (Vec<Candidate>, Vec<Candidate>) =
                head.into_iter().partition(|c| is_single(c) || often(c) || c.kind == CandKind::Raw);
            let rest = std::mem::take(&mut out);
            out = keep.into_iter().chain(later).chain(rest).collect();
        }
    }
    // 单字"几十次就改"：你一直选的那个字排到单字的第一位
    {
        let single_at = |c: &Candidate| {
            c.kind == CandKind::Word
                && c.code.len() == 1
                && c.text.chars().nth(1).is_none()
                && c.fixes.is_empty()
                && c.consumed == n
        };
        if let Some(first) = out.iter().position(single_at) {
            let count = |c: &Candidate| user.char_count(c.code[0], &c.text, now);
            let top = count(&out[first]);
            let best = out
                .iter()
                .enumerate()
                .skip(first + 1)
                .filter(|(_, c)| single_at(c))
                .map(|(i, c)| (i, count(c)))
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, cnt)) = best {
                if cnt >= CHAR_TAKEOVER && cnt >= top * CHAR_TAKEOVER_RATIO {
                    let mut c = out.remove(i);
                    c.poor = false;
                    out.insert(first, c);
                }
            }
        }
    }
    // 上次选过、但排得靠后的字词：放进前 4 个。第一个仍然是常用的，要选很多次才会换
    let recent = out
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.kind, CandKind::Word | CandKind::Sentence) && !c.code.is_empty())
        .filter_map(|(i, c)| user.last_pick(&c.code, &c.text).filter(|&t| now - t <= RECENT_SECS).map(|t| (i, t)))
        .max_by_key(|&(_, t)| t);
    if let Some((i, _)) = recent {
        if i > RECENT_SLOT {
            let mut c = out.remove(i);
            c.poor = false;
            out.insert(RECENT_SLOT, c);
            for x in out.iter_mut().take(RECENT_SLOT) {
                x.poor = false;
            }
        }
    }
    out.truncate(MAX_CANDS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    fn dict() -> Dict {
        let syl = [
            "xi", "an", "xian", "zai", "wo", "zhong", "guo", "ren", "fang", "fan", "gan", "xu", "huan", "hao", "zhang",
            "ni",
        ];
        let mut b = DictBuilder::new(syl.map(String::from));
        let mut add = |py: &str, t: &str, s: f32| {
            let c = b.code(py).unwrap();
            b.add(&c, t, s);
        };
        add("xian", "先", -6.0);
        add("xian", "现", -6.3);
        add("xian", "线", -7.5);
        add("xi", "西", -7.0);
        add("xi", "喜", -7.2);
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
        add("xi huan", "喜欢", -7.0);
        add("xu", "需", -8.0);
        add("huan", "欢", -10.0);
        add("hao", "好", -5.0);
        add("zhang", "张", -7.0);
        add("zhang", "长", -7.2);
        add("zhang", "胀", -10.0);
        add("hao zhang", "好长", -9.0);
        add("ni", "你", -5.0);
        add("ni hao", "你好", -6.0);
        add("ni hao", "拟好", -11.0);
        Dict::from_bytes(b.build()).unwrap()
    }

    fn texts(d: &Dict, u: &UserDb, s: &str) -> Vec<String> {
        analyze(d, None, u, s, 0).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn mixes_segmentations_without_apostrophe() {
        let d = dict();
        let u = UserDb::in_memory();
        let t = texts(&d, &u, "xian");
        assert_eq!(t[0], "先");
        let pos = t.iter().position(|x| x == "西安").unwrap();
        assert!(pos <= 3, "{t:?}");
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
        let c = analyze(&d, None, &u, "woxianzai", 0);
        assert_eq!(c[0].kind, CandKind::Sentence);
        assert!(c.iter().any(|c| c.text == "我" && c.consumed == 2));
    }

    #[test]
    fn rare_full_word_goes_after_common_single_chars() {
        let d = dict();
        let u = UserDb::in_memory();
        let t = texts(&d, &u, "nihao");
        assert_eq!(t[0], "你好");
        let ni = t.iter().position(|x| x == "你").unwrap();
        let nh = t.iter().position(|x| x == "拟好").unwrap();
        assert!(ni < nh, "{t:?}");
    }

    #[test]
    fn combinations_not_in_dictionary_appear() {
        let d = dict();
        let u = UserDb::in_memory();
        let t = texts(&d, &u, "haozhang");
        assert_eq!(t[0], "好长");
        assert!(t.contains(&"好胀".to_string()), "{t:?}");
        assert!(t.contains(&"好张".to_string()), "{t:?}");
    }

    #[test]
    fn typo_is_corrected_in_words() {
        let d = dict();
        let u = UserDb::in_memory();
        let c = analyze(&d, None, &u, "woxuhuan", 0);
        assert_eq!(c[0].text, "我喜欢");
        assert!(c[0].corrected());
        // 单独打 xu 不会被改成 xi（单字不纠正本来就对的拼音）
        assert_eq!(texts(&d, &u, "xu")[0], "需");
        assert!(!texts(&d, &u, "xu").contains(&"喜".to_string()));
    }

    #[test]
    fn unparseable_letters_give_raw_candidate() {
        let d = dict();
        let u = UserDb::in_memory();
        let c = analyze(&d, None, &u, "iii", 0);
        assert_eq!(c[0].kind, CandKind::Raw);
        assert_eq!(c[0].text, "iii");
        let c = analyze(&d, None, &u, "Xian", 0);
        assert_eq!(c[0].text, "Xian");
        assert_eq!(c[1].text, "先");
    }

    #[test]
    fn english_words_mix_in() {
        let d = dict();
        let u = UserDb::in_memory();
        let en = EnglishDict::from_words(vec![("help".into(), -6.6), ("helpful".into(), -9.8), ("an".into(), -4.0)]);
        let c: Vec<String> = analyze(&d, Some(&en), &u, "help", 0).into_iter().map(|c| c.text).collect();
        assert_eq!(c[0], "help", "{c:?}");
        assert!(c.contains(&"helpful".to_string()));
        // 大写开头：原样第一，英文跟着大写
        let c: Vec<String> = analyze(&d, Some(&en), &u, "Help", 0).into_iter().map(|c| c.text).collect();
        assert_eq!(c[0], "Help");
        assert!(c.contains(&"Helpful".to_string()), "{c:?}");
        // 本来就是完整拼音的，中文在前
        let c: Vec<String> = analyze(&d, Some(&en), &u, "an", 0).into_iter().map(|c| c.text).collect();
        assert_eq!(c[0], "安", "{c:?}");
    }

    #[test]
    fn learned_word_goes_first() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let c = analyze(&d, None, &u, "fangan", 0);
        let fg = c.iter().find(|c| c.text == "反感").unwrap().clone();
        u.learn(&d, &fg.code, "反感", 0);
        assert_eq!(texts(&d, &u, "fangan")[0], "反感");
        let code: Vec<u16> = ["xi", "an", "ren"].iter().map(|p| d.syllable_id(p).unwrap()).collect();
        u.learn(&d, &code, "西安人", 0);
        assert_eq!(texts(&d, &u, "xianren")[0], "西安人");
        assert_eq!(texts(&d, &u, "xianr")[0], "西安人");
    }

    #[test]
    fn learned_word_keeps_other_candidates_visible() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let c = analyze(&d, None, &u, "fangan", 0);
        let fg = c.iter().find(|c| c.text == "反感").unwrap().clone();
        u.learn(&d, &fg.code, "反感", 0);
        let c = analyze(&d, None, &u, "fangan", 0);
        // 记住的词排第一，但其他候选不能因此被判成"质量差"
        assert_eq!(c[0].text, "反感");
        assert!(!c.iter().find(|c| c.text == "方案").unwrap().poor, "{c:?}");
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
