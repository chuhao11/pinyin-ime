//! 编译好的词库文件。
//!
//! 文件结构是一棵按音节编号组织的字典树（trie）：从根节点出发，每走一条边消耗一个音节，
//! 节点上挂着"拼音正好是这串音节"的字词，已按分数从高到低排好。
//! 运行时用内存映射只读加载，多个进程可以共享同一份物理内存；加载时做完整的越界检查，
//! 之后的访问不会越界。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::ops::Range;
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"PYIMEDC1";
pub const VERSION: u32 = 1;

const NSEC: usize = 9;
const S_SYL_OFF: usize = 0; // u32 × (音节数+1)
const S_SYL_BLOB: usize = 1; // 音节字符串
const S_NODE_CHILD: usize = 2; // u32 × (节点数+1)：每个节点的边在边数组里的起点
const S_NODE_WORD: usize = 3; // u32 × (节点数+1)：每个节点的字词在字词数组里的起点
const S_EDGE_SYL: usize = 4; // u16 × 边数：边上的音节编号（同一节点内升序）
const S_EDGE_NODE: usize = 5; // u32 × 边数：边指向的节点
const S_WORD_OFF: usize = 6; // u32 × (字词数+1)：字词文字在文字区的起点
const S_WORD_SCORE: usize = 7; // f32 × 字词数：分数（对数概率，越大越常用）
const S_TEXT: usize = 8; // 字词文字（UTF-8）
const HEADER_LEN: usize = 16 + NSEC * 16;

#[derive(Debug)]
pub struct DictError(pub String);

impl std::fmt::Display for DictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "词库文件错误：{}", self.0)
    }
}

impl std::error::Error for DictError {}

fn err<T>(msg: impl Into<String>) -> Result<T, DictError> {
    Err(DictError(msg.into()))
}

enum Storage {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mapped(m) => &m[..],
            Storage::Owned(v) => &v[..],
        }
    }
}

#[inline]
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

#[inline]
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[inline]
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// 只读词库。
pub struct Dict {
    storage: Storage,
    sec: [(usize, usize); NSEC],
    syllables: Vec<String>,
    syl_index: HashMap<String, u16>,
    n_nodes: usize,
    n_edges: usize,
    n_words: usize,
}

impl Dict {
    /// 用内存映射打开词库文件。
    pub fn open(path: &Path) -> Result<Dict, DictError> {
        let f = File::open(path).map_err(|e| DictError(format!("打不开 {}：{e}", path.display())))?;
        // 安全性说明：文件以只读方式映射。词库装在普通程序不能改写的目录里，运行期间不会被修改；
        // 即便内容异常，加载时的完整校验也保证之后的访问不会越界。
        let m =
            unsafe { memmap2::Mmap::map(&f) }.map_err(|e| DictError(format!("映射 {} 失败：{e}", path.display())))?;
        Self::from_storage(Storage::Mapped(m))
    }

    /// 从内存里的字节加载（测试用）。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Dict, DictError> {
        Self::from_storage(Storage::Owned(bytes))
    }

    fn from_storage(storage: Storage) -> Result<Dict, DictError> {
        let b = storage.bytes();
        if b.len() < HEADER_LEN || &b[0..8] != MAGIC {
            return err("不是本输入法的词库文件");
        }
        if rd_u32(b, 8) != VERSION {
            return err("词库文件版本不对，请重新编译词库");
        }
        if rd_u32(b, 12) as usize != NSEC {
            return err("词库文件分段数不对");
        }
        let mut sec = [(0usize, 0usize); NSEC];
        for (i, s) in sec.iter_mut().enumerate() {
            let off = rd_u64(b, 16 + i * 16) as usize;
            let len = rd_u64(b, 16 + i * 16 + 8) as usize;
            match off.checked_add(len) {
                Some(end) if end <= b.len() => *s = (off, len),
                _ => return err(format!("第 {i} 段超出文件范围")),
            }
        }
        let count = |s: usize, unit: usize| -> Result<usize, DictError> {
            if !sec[s].1.is_multiple_of(unit) {
                return err(format!("第 {s} 段长度不对"));
            }
            Ok(sec[s].1 / unit)
        };
        let n_syl = count(S_SYL_OFF, 4)?.checked_sub(1).ok_or(DictError("音节表为空".into()))?;
        let n_nodes = count(S_NODE_CHILD, 4)?.checked_sub(1).ok_or(DictError("没有节点".into()))?;
        if n_nodes == 0 || count(S_NODE_WORD, 4)? != n_nodes + 1 {
            return err("节点表长度不一致");
        }
        let n_edges = count(S_EDGE_NODE, 4)?;
        if count(S_EDGE_SYL, 2)? != n_edges {
            return err("边表长度不一致");
        }
        let n_words = count(S_WORD_OFF, 4)?.checked_sub(1).ok_or(DictError("字词表为空".into()))?;
        if count(S_WORD_SCORE, 4)? != n_words {
            return err("分数表长度不一致");
        }
        if n_syl > u16::MAX as usize {
            return err("音节太多");
        }

        let mut d = Dict { storage, sec, syllables: Vec::new(), syl_index: HashMap::new(), n_nodes, n_edges, n_words };
        d.validate(n_syl)?;
        Ok(d)
    }

    fn validate(&mut self, n_syl: usize) -> Result<(), DictError> {
        let b = self.storage.bytes();
        let sec = self.sec;
        let u32_at = |s: usize, i: usize| rd_u32(b, sec[s].0 + i * 4) as usize;

        // 音节表：偏移单调、UTF-8 合法、严格升序
        let blob = &b[sec[S_SYL_BLOB].0..sec[S_SYL_BLOB].0 + sec[S_SYL_BLOB].1];
        let mut syllables = Vec::with_capacity(n_syl);
        for i in 0..n_syl {
            let (s, e) = (u32_at(S_SYL_OFF, i), u32_at(S_SYL_OFF, i + 1));
            if s > e || e > blob.len() {
                return err("音节偏移不对");
            }
            let t = std::str::from_utf8(&blob[s..e]).map_err(|_| DictError("音节不是合法文字".into()))?;
            if t.is_empty() || !t.bytes().all(|c| c.is_ascii_lowercase()) {
                return err("音节只能由小写字母组成");
            }
            if let Some(prev) = syllables.last() {
                if prev >= &t.to_string() {
                    return err("音节表没有排序");
                }
            }
            syllables.push(t.to_string());
        }

        // 节点：边和字词的起点单调，最后一个等于总数
        for (s, total) in [(S_NODE_CHILD, self.n_edges), (S_NODE_WORD, self.n_words)] {
            let mut prev = 0usize;
            for i in 0..=self.n_nodes {
                let v = u32_at(s, i);
                if v < prev || v > total {
                    return err("节点表不单调");
                }
                prev = v;
            }
            if prev != total {
                return err("节点表总数不对");
            }
        }
        // 边：音节和目标节点都在范围内，同一节点内音节严格升序
        for node in 0..self.n_nodes {
            let (s, e) = (u32_at(S_NODE_CHILD, node), u32_at(S_NODE_CHILD, node + 1));
            let mut prev: Option<u16> = None;
            for edge in s..e {
                let syl = rd_u16(b, sec[S_EDGE_SYL].0 + edge * 2);
                let to = u32_at(S_EDGE_NODE, edge);
                if syl as usize >= n_syl || to >= self.n_nodes || to == 0 {
                    return err("边指向不存在的音节或节点");
                }
                if prev.is_some_and(|p| p >= syl) {
                    return err("同一节点的边没有按音节排序");
                }
                prev = Some(syl);
            }
        }
        // 字词文字：整块是合法 UTF-8，每个偏移都在字符边界上
        let text_len = sec[S_TEXT].1;
        let text = std::str::from_utf8(&b[sec[S_TEXT].0..sec[S_TEXT].0 + text_len])
            .map_err(|_| DictError("字词文字不是合法 UTF-8".into()))?;
        let mut prev = 0usize;
        for i in 0..=self.n_words {
            let v = u32_at(S_WORD_OFF, i);
            if v < prev || v > text_len || !text.is_char_boundary(v) {
                return err("字词偏移不对");
            }
            prev = v;
        }
        for i in 0..self.n_words {
            if !f32::from_bits(rd_u32(b, sec[S_WORD_SCORE].0 + i * 4)).is_finite() {
                return err("分数不是有限数");
            }
        }

        self.syl_index = syllables.iter().enumerate().map(|(i, s)| (s.clone(), i as u16)).collect();
        self.syllables = syllables;
        Ok(())
    }

    #[inline]
    fn b(&self) -> &[u8] {
        self.storage.bytes()
    }

    #[inline]
    fn u32_at(&self, s: usize, i: usize) -> u32 {
        rd_u32(self.b(), self.sec[s].0 + i * 4)
    }

    pub fn n_nodes(&self) -> usize {
        self.n_nodes
    }

    pub fn n_words(&self) -> usize {
        self.n_words
    }

    /// 文件总大小（字节）。
    pub fn byte_len(&self) -> usize {
        self.b().len()
    }

    pub fn syllables(&self) -> &[String] {
        &self.syllables
    }

    pub fn syllable(&self, id: u16) -> &str {
        self.syllables.get(id as usize).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn syllable_id(&self, s: &str) -> Option<u16> {
        self.syl_index.get(s).copied()
    }

    /// 以 `prefix` 开头的音节编号区间 [lo, hi)。音节按字母排序，所以它们是连续的一段。
    pub fn prefix_range(&self, prefix: &str) -> (u16, u16) {
        let lo = self.syllables.partition_point(|s| s.as_str() < prefix);
        let n = self.syllables[lo..].partition_point(|s| s.starts_with(prefix));
        (lo as u16, (lo + n) as u16)
    }

    pub fn root(&self) -> u32 {
        0
    }

    #[inline]
    fn child_edges(&self, node: u32) -> Range<usize> {
        let n = node as usize;
        self.u32_at(S_NODE_CHILD, n) as usize..self.u32_at(S_NODE_CHILD, n + 1) as usize
    }

    #[inline]
    pub fn edge_syl(&self, e: usize) -> u16 {
        rd_u16(self.b(), self.sec[S_EDGE_SYL].0 + e * 2)
    }

    #[inline]
    pub fn edge_node(&self, e: usize) -> u32 {
        self.u32_at(S_EDGE_NODE, e)
    }

    /// 节点下音节编号落在 [lo, hi) 的那些边。
    pub fn child_edges_in(&self, node: u32, lo: u16, hi: u16) -> Range<usize> {
        let r = self.child_edges(node);
        let first = partition(r.clone(), |e| self.edge_syl(e) < lo);
        let last = partition(first..r.end, |e| self.edge_syl(e) < hi);
        first..last
    }

    /// 节点上挂的字词编号（已按分数从高到低排序）。
    pub fn node_words(&self, node: u32) -> Range<u32> {
        let n = node as usize;
        self.u32_at(S_NODE_WORD, n)..self.u32_at(S_NODE_WORD, n + 1)
    }

    pub fn word_text(&self, w: u32) -> &str {
        let w = w as usize;
        let (s, e) = (self.u32_at(S_WORD_OFF, w) as usize, self.u32_at(S_WORD_OFF, w + 1) as usize);
        let base = self.sec[S_TEXT].0;
        std::str::from_utf8(&self.b()[base + s..base + e]).unwrap_or("")
    }

    pub fn word_score(&self, w: u32) -> f32 {
        f32::from_bits(self.u32_at(S_WORD_SCORE, w as usize))
    }

    /// 按完整拼音查字词（调试和测试用），例如 `lookup("xi an")`。
    pub fn lookup(&self, pinyin: &str) -> Vec<(String, f32)> {
        let mut node = self.root();
        for p in pinyin.split_whitespace() {
            let Some(id) = self.syllable_id(p) else { return vec![] };
            let r = self.child_edges_in(node, id, id + 1);
            if r.is_empty() {
                return vec![];
            }
            node = self.edge_node(r.start);
        }
        self.node_words(node).map(|w| (self.word_text(w).to_string(), self.word_score(w))).collect()
    }
}

/// 在区间里找第一个不满足 `pred` 的位置（`pred` 必须是先真后假）。
fn partition(r: Range<usize>, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (r.start, r.end);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[derive(Default)]
struct BNode {
    children: Vec<(u16, u32)>,
    words: Vec<(f32, String)>,
}

/// 生成词库文件。
pub struct DictBuilder {
    syllables: Vec<String>,
    index: HashMap<String, u16>,
    nodes: Vec<BNode>,
}

impl DictBuilder {
    pub fn new<I: IntoIterator<Item = String>>(syllables: I) -> Self {
        let mut v: Vec<String> = syllables.into_iter().collect();
        v.sort();
        v.dedup();
        assert!(v.len() < u16::MAX as usize, "音节太多");
        let index = v.iter().enumerate().map(|(i, s)| (s.clone(), i as u16)).collect();
        DictBuilder { syllables: v, index, nodes: vec![BNode::default()] }
    }

    pub fn syllable_id(&self, s: &str) -> Option<u16> {
        self.index.get(s).copied()
    }

    /// 把拼音（空格分隔）转成音节编号；有不认识的音节就返回 None。
    pub fn code(&self, pinyin: &str) -> Option<Vec<u16>> {
        let v: Option<Vec<u16>> = pinyin.split_whitespace().map(|p| self.syllable_id(p)).collect();
        v.filter(|v| !v.is_empty())
    }

    pub fn add(&mut self, code: &[u16], text: &str, score: f32) {
        assert!(!code.is_empty() && score.is_finite());
        let mut n = 0usize;
        for &s in code {
            n = match self.nodes[n].children.binary_search_by_key(&s, |c| c.0) {
                Ok(i) => self.nodes[n].children[i].1 as usize,
                Err(i) => {
                    let id = self.nodes.len() as u32;
                    self.nodes.push(BNode::default());
                    self.nodes[n].children.insert(i, (s, id));
                    id as usize
                }
            };
        }
        self.nodes[n].words.push((score, text.to_string()));
    }

    pub fn build(mut self) -> Vec<u8> {
        let mut syl_off = vec![0u32];
        let mut syl_blob = Vec::new();
        for s in &self.syllables {
            syl_blob.extend_from_slice(s.as_bytes());
            syl_off.push(syl_blob.len() as u32);
        }
        let n = self.nodes.len();
        let mut node_child = Vec::with_capacity(n + 1);
        let mut node_word = Vec::with_capacity(n + 1);
        let mut edge_syl: Vec<u16> = Vec::new();
        let mut edge_node: Vec<u32> = Vec::new();
        let mut word_off = vec![0u32];
        let mut word_score: Vec<f32> = Vec::new();
        let mut text: Vec<u8> = Vec::new();
        for node in self.nodes.iter_mut() {
            node_child.push(edge_syl.len() as u32);
            node_word.push(word_score.len() as u32);
            for &(s, c) in &node.children {
                edge_syl.push(s);
                edge_node.push(c);
            }
            node.words.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let mut seen = HashSet::new();
            for (sc, t) in node.words.drain(..) {
                if seen.insert(t.clone()) {
                    text.extend_from_slice(t.as_bytes());
                    word_off.push(text.len() as u32);
                    word_score.push(sc);
                }
            }
            node.children = Vec::new();
        }
        node_child.push(edge_syl.len() as u32);
        node_word.push(word_score.len() as u32);

        let u32s = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let sections: [Vec<u8>; NSEC] = [
            u32s(&syl_off),
            syl_blob,
            u32s(&node_child),
            u32s(&node_word),
            edge_syl.iter().flat_map(|x| x.to_le_bytes()).collect(),
            u32s(&edge_node),
            u32s(&word_off),
            word_score.iter().flat_map(|x| x.to_bits().to_le_bytes()).collect(),
            text,
        ];
        let mut out = vec![0u8; HEADER_LEN];
        out[0..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&(NSEC as u32).to_le_bytes());
        for (i, s) in sections.iter().enumerate() {
            while !out.len().is_multiple_of(8) {
                out.push(0);
            }
            let off = out.len() as u64;
            out[16 + i * 16..24 + i * 16].copy_from_slice(&off.to_le_bytes());
            out[24 + i * 16..32 + i * 16].copy_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dict {
        let mut b = DictBuilder::new(["xi", "an", "xian", "zhong", "guo"].map(String::from));
        let c = b.code("xi an").unwrap();
        b.add(&c, "西安", -9.0);
        let c = b.code("xian").unwrap();
        b.add(&c, "先", -6.0);
        b.add(&c, "现", -6.5);
        b.add(&c, "先", -7.0); // 重复的字只保留分数高的
        let c = b.code("zhong guo").unwrap();
        b.add(&c, "中国", -5.0);
        Dict::from_bytes(b.build()).unwrap()
    }

    #[test]
    fn builds_and_reads_back() {
        let d = sample();
        assert_eq!(d.lookup("xian"), vec![("先".to_string(), -6.0), ("现".to_string(), -6.5)]);
        assert_eq!(d.lookup("xi an")[0].0, "西安");
        assert_eq!(d.lookup("zhong guo")[0].0, "中国");
        assert!(d.lookup("zhong").is_empty());
        assert!(d.lookup("bu").is_empty());
    }

    #[test]
    fn prefix_ranges_are_contiguous() {
        let d = sample();
        let (lo, hi) = d.prefix_range("x");
        let got: Vec<&str> = (lo..hi).map(|i| d.syllable(i)).collect();
        assert_eq!(got, vec!["xi", "xian"]);
        let (lo, hi) = d.prefix_range("q");
        assert_eq!(lo, hi);
    }

    #[test]
    fn rejects_corrupted_files() {
        let mut b = DictBuilder::new(["a".to_string()]);
        b.add(&[0], "啊", -1.0);
        let good = b.build();
        assert!(Dict::from_bytes(good.clone()).is_ok());
        assert!(Dict::from_bytes(good[..good.len() - 1].to_vec()).is_err());
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(Dict::from_bytes(bad).is_err());
        // 把每个字节都改一遍：要么被拒绝，要么能安全加载，绝不能崩溃
        for i in 0..good.len() {
            let mut v = good.clone();
            v[i] ^= 0xFF;
            if let Ok(d) = Dict::from_bytes(v) {
                let _ = d.lookup("a");
            }
        }
    }
}
