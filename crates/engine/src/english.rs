//! 英文单词：完全一样、补全当前这个词、拼写纠错。
//!
//! 词表来自 data/build/english.tsv（dictgen 生成），每个词带一个分数（越常用越大）。
//! 查找用一棵按字母组织的字典树：
//! - 完全一样：沿着输入走到底
//! - 补全：走到输入的末尾，再往下找最常用的几个词（只补全当前这个词，不预测下一个词）
//! - 拼写纠错：边走边算编辑距离，按到旁边的键、两个字母打反算半个错

use std::path::Path;

use crate::pinyin::adjacent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnKind {
    Exact,
    Complete,
    Fix,
}

pub struct EWord {
    pub text: String,
    pub score: f32,
}

#[derive(Default)]
struct ENode {
    children: Vec<(u8, u32)>,
    words: Vec<u32>,
}

pub struct EnglishDict {
    words: Vec<EWord>,
    nodes: Vec<ENode>,
}

#[derive(Clone, Copy, Debug)]
pub struct EnHit {
    pub word: u32,
    pub kind: EnKind,
    /// 补全、纠错的扣分
    pub penalty: f32,
}

/// 纠错时一个错要扣多少分
const FIX_COST_PEN: f32 = -4.0;
/// 补全扣分：基础 + 每多一个字母
const COMPLETE_PEN: f32 = -1.5;
const COMPLETE_PER_LETTER: f32 = -0.15;
const MAX_COMPLETIONS: usize = 4;
const MAX_FIXES: usize = 4;

/// 加权编辑距离：按到旁边的键、两个字母打反算 0.7，其余算 1。
pub fn edit_cost(a: &[u8], b: &[u8]) -> f32 {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0f32; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as f32;
    }
    for (j, v) in d[0].iter_mut().enumerate() {
        *v = j as f32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = sub_cost(a[i - 1], b[j - 1]);
            let mut v = (d[i - 1][j] + 1.0).min(d[i][j - 1] + 1.0).min(d[i - 1][j - 1] + sub);
            if i >= 2 && j >= 2 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] && a[i - 1] != a[i - 2] {
                v = v.min(d[i - 2][j - 2] + 0.7);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

fn sub_cost(a: u8, b: u8) -> f32 {
    if a == b {
        0.0
    } else if adjacent(a, b) {
        0.7
    } else {
        1.0
    }
}

/// 输入多长允许错多少。
pub fn max_cost(len: usize) -> f32 {
    match len {
        0..=3 => 0.0,
        4..=6 => 1.0,
        _ => 2.0,
    }
}

impl EnglishDict {
    pub fn load(path: &Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("读不了英文词表 {}：{e}", path.display()))?;
        let mut v = Vec::new();
        for line in s.lines() {
            let mut f = line.trim_end_matches('\r').split('\t');
            if let (Some(w), Some(sc)) = (f.next(), f.next()) {
                if let Ok(sc) = sc.parse::<f32>() {
                    v.push((w.to_string(), sc));
                }
            }
        }
        Ok(Self::from_words(v))
    }

    pub fn from_words(list: Vec<(String, f32)>) -> Self {
        let mut d = EnglishDict { words: Vec::new(), nodes: vec![ENode::default()] };
        for (text, score) in list {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_alphabetic()) || !score.is_finite() {
                continue;
            }
            let idx = d.words.len() as u32;
            let mut n = 0usize;
            for b in text.bytes().map(|b| b.to_ascii_lowercase()) {
                n = match d.nodes[n].children.binary_search_by_key(&b, |c| c.0) {
                    Ok(i) => d.nodes[n].children[i].1 as usize,
                    Err(i) => {
                        let id = d.nodes.len() as u32;
                        d.nodes.push(ENode::default());
                        d.nodes[n].children.insert(i, (b, id));
                        id as usize
                    }
                };
            }
            d.nodes[n].words.push(idx);
            d.words.push(EWord { text, score });
        }
        d
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn word(&self, i: u32) -> &EWord {
        &self.words[i as usize]
    }

    fn walk(&self, key: &[u8]) -> Option<usize> {
        let mut n = 0usize;
        for b in key {
            let c = &self.nodes[n].children;
            n = c.binary_search_by_key(b, |x| x.0).ok().map(|i| c[i].1 as usize)?;
        }
        Some(n)
    }

    /// 查英文。`key` 是小写字母。
    pub fn search(&self, key: &[u8]) -> Vec<EnHit> {
        let mut out = Vec::new();
        if key.len() < 2 {
            return out;
        }
        let node = self.walk(key);
        if let Some(n) = node {
            for &w in &self.nodes[n].words {
                out.push(EnHit { word: w, kind: EnKind::Exact, penalty: 0.0 });
            }
            // 补全当前这个词
            if key.len() >= 3 {
                let mut found: Vec<(f32, u32, usize)> = Vec::new();
                let mut stack: Vec<(usize, usize)> =
                    self.nodes[n].children.iter().map(|&(_, c)| (c as usize, 1)).collect();
                while let Some((x, depth)) = stack.pop() {
                    for &w in &self.nodes[x].words {
                        found.push((self.words[w as usize].score, w, depth));
                    }
                    for &(_, c) in &self.nodes[x].children {
                        stack.push((c as usize, depth + 1));
                    }
                }
                found.sort_by(|a, b| b.0.total_cmp(&a.0));
                for (_, w, extra) in found.into_iter().take(MAX_COMPLETIONS) {
                    let penalty = COMPLETE_PEN + COMPLETE_PER_LETTER * extra as f32;
                    out.push(EnHit { word: w, kind: EnKind::Complete, penalty });
                }
            }
        }
        // 拼写纠错
        let maxc = max_cost(key.len());
        if maxc > 0.0 {
            let mut fixes: Vec<(u32, f32)> = Vec::new();
            let row0: Vec<f32> = (0..=key.len()).map(|j| j as f32).collect();
            for &(c, child) in &self.nodes[0].children {
                self.fix_rec(child as usize, c, 0, &row0, None, key, maxc, &mut fixes);
            }
            let mut fixes: Vec<(f32, u32, f32)> = fixes
                .into_iter()
                .map(|(w, cost)| (self.words[w as usize].score + cost * FIX_COST_PEN, w, cost))
                .collect();
            fixes.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (_, w, cost) in fixes.into_iter().take(MAX_FIXES) {
                out.push(EnHit { word: w, kind: EnKind::Fix, penalty: cost * FIX_COST_PEN });
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn fix_rec(
        &self,
        node: usize,
        c: u8,
        prev_c: u8,
        prev: &[f32],
        prev2: Option<&[f32]>,
        key: &[u8],
        maxc: f32,
        out: &mut Vec<(u32, f32)>,
    ) {
        let n = key.len();
        let mut row = vec![0f32; n + 1];
        row[0] = prev[0] + 1.0;
        let mut best = row[0];
        for j in 1..=n {
            let mut v = (prev[j] + 1.0).min(row[j - 1] + 1.0).min(prev[j - 1] + sub_cost(key[j - 1], c));
            if let Some(p2) = prev2 {
                if j >= 2 && key[j - 1] == prev_c && key[j - 2] == c && c != prev_c {
                    v = v.min(p2[j - 2] + 0.7);
                }
            }
            row[j] = v;
            best = best.min(v);
        }
        let cost = row[n];
        if cost > 0.0 && cost <= maxc {
            for &w in &self.nodes[node].words {
                out.push((w, cost));
            }
        }
        if best <= maxc {
            for &(c2, ch) in &self.nodes[node].children {
                self.fix_rec(ch as usize, c2, c, &row, Some(prev), key, maxc, out);
            }
        }
    }
}

/// 按你打的大小写调整英文词：首字母大写就首字母大写，全大写就全大写；词本身有大写（iPhone）就不动。
pub fn match_case(word: &str, typed: &str) -> String {
    let word_has_upper = word.bytes().any(|b| b.is_ascii_uppercase());
    let tb = typed.as_bytes();
    if word_has_upper || tb.is_empty() {
        return word.to_string();
    }
    if tb.len() >= 2 && tb.iter().all(|b| b.is_ascii_uppercase()) {
        return word.to_ascii_uppercase();
    }
    if tb[0].is_ascii_uppercase() {
        let mut s = word.to_string();
        s[..1].make_ascii_uppercase();
        return s;
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> EnglishDict {
        let words = [
            ("the", -2.3),
            ("help", -6.6),
            ("helpful", -9.8),
            ("helper", -10.5),
            ("what", -6.3),
            ("immediately", -10.0),
            ("receive", -9.1),
            ("accommodate", -11.3),
            ("definitely", -9.5),
            ("iPhone", -12.4),
        ];
        EnglishDict::from_words(words.iter().map(|(w, s)| (w.to_string(), *s as f32)).collect())
    }

    fn texts(d: &EnglishDict, k: &str, kind: EnKind) -> Vec<String> {
        d.search(k.as_bytes()).into_iter().filter(|h| h.kind == kind).map(|h| d.word(h.word).text.clone()).collect()
    }

    #[test]
    fn exact_and_completion() {
        let d = dict();
        assert_eq!(texts(&d, "help", EnKind::Exact), vec!["help"]);
        assert_eq!(texts(&d, "help", EnKind::Complete), vec!["helpful", "helper"]);
        assert_eq!(texts(&d, "immedi", EnKind::Complete), vec!["immediately"]);
        assert_eq!(texts(&d, "iphone", EnKind::Exact), vec!["iPhone"]);
    }

    #[test]
    fn spelling_fixes() {
        let d = dict();
        assert!(texts(&d, "recieve", EnKind::Fix).contains(&"receive".to_string()));
        assert!(texts(&d, "accomodate", EnKind::Fix).contains(&"accommodate".to_string()));
        assert!(texts(&d, "definately", EnKind::Fix).contains(&"definitely".to_string()));
        assert!(texts(&d, "hwlp", EnKind::Fix).contains(&"help".to_string()));
        // 太短的不纠错
        assert!(texts(&d, "teh", EnKind::Fix).is_empty());
    }

    #[test]
    fn edit_costs() {
        assert_eq!(edit_cost(b"help", b"help"), 0.0);
        assert!((edit_cost(b"hwlp", b"help") - 0.7).abs() < 1e-6);
        assert!((edit_cost(b"hlep", b"help") - 0.7).abs() < 1e-6);
        assert_eq!(edit_cost(b"hep", b"help"), 1.0);
    }

    #[test]
    fn case_follows_typing() {
        assert_eq!(match_case("help", "Hel"), "Help");
        assert_eq!(match_case("help", "HELP"), "HELP");
        assert_eq!(match_case("iPhone", "Iphone"), "iPhone");
        assert_eq!(match_case("help", "help"), "help");
    }
}
