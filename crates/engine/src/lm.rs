//! 语言模型：词和词之间的搭配统计（二元模型）。
//!
//! 文件里存三样东西：
//! - 每个词单独出现的概率（对数）
//! - 每个词后面跟另一个词的概率（只存统计里常见的搭配）
//! - 每个词的"退回系数"：没见过的搭配，用 退回系数 × 后一个词单独的概率 来估计
//!
//! 和词库一样用内存映射只读加载，加载时做完整的越界检查。
//! 词按文字排序，查词用二分查找，所以模型和词库的编译顺序无关。

use std::fs::File;
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"PYIMELM1";
pub const VERSION: u32 = 1;

const NSEC: usize = 8;
const S_VOCAB_OFF: usize = 0; // u32 × (词数+1)
const S_VOCAB: usize = 1; // 词的文字（按字节排序）
const S_UNI: usize = 2; // f32 × 词数：单独出现的对数概率
const S_BACKOFF: usize = 3; // f32 × 词数：退回系数（对数）
const S_SUCC_OFF: usize = 4; // u32 × (词数+1)：每个词的搭配在搭配表里的起点
const S_SUCC_ID: usize = 5; // u32 × 搭配数：后一个词（同一个前词内升序）
const S_SUCC_LP: usize = 6; // u8 × 搭配数：对数概率，值 q 表示 -q/10
const S_META: usize = 7; // f32 × 1：词表外的词，用"词库分数 + 这个数"当单词概率
const HEADER_LEN: usize = 16 + NSEC * 16;

/// 搭配概率的量化：0.1 一档，最低 -25.5。
pub fn quantize(lp: f32) -> u8 {
    (-lp * 10.0).round().clamp(0.0, 255.0) as u8
}

pub fn dequantize(q: u8) -> f32 {
    -(q as f32) / 10.0
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
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

pub struct Lm {
    storage: Storage,
    sec: [(usize, usize); NSEC],
    n_vocab: usize,
    n_succ: usize,
}

impl Lm {
    pub fn open(path: &Path) -> Result<Lm, String> {
        let f = File::open(path).map_err(|e| format!("打不开语言模型 {}：{e}", path.display()))?;
        // 安全性说明：只读映射；加载时完整校验，之后的访问不会越界。
        let m = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| format!("映射语言模型失败：{e}"))?;
        Self::from_storage(Storage::Mapped(m))
    }

    pub fn from_bytes(v: Vec<u8>) -> Result<Lm, String> {
        Self::from_storage(Storage::Owned(v))
    }

    fn from_storage(storage: Storage) -> Result<Lm, String> {
        let b = storage.bytes();
        if b.len() < HEADER_LEN || &b[0..8] != MAGIC {
            return Err("不是本输入法的语言模型文件".into());
        }
        if rd_u32(b, 8) != VERSION || rd_u32(b, 12) as usize != NSEC {
            return Err("语言模型版本不对".into());
        }
        let mut sec = [(0usize, 0usize); NSEC];
        for (i, s) in sec.iter_mut().enumerate() {
            let off = rd_u64(b, 16 + i * 16) as usize;
            let len = rd_u64(b, 24 + i * 16) as usize;
            match off.checked_add(len) {
                Some(end) if end <= b.len() => *s = (off, len),
                _ => return Err(format!("语言模型第 {i} 段超出文件范围")),
            }
        }
        let n_vocab = (sec[S_VOCAB_OFF].1 / 4).checked_sub(1).ok_or("词表为空")?;
        let n_succ = sec[S_SUCC_ID].1 / 4;
        let ok = sec[S_VOCAB_OFF].1 % 4 == 0
            && sec[S_UNI].1 == n_vocab * 4
            && sec[S_BACKOFF].1 == n_vocab * 4
            && sec[S_SUCC_OFF].1 == (n_vocab + 1) * 4
            && sec[S_SUCC_ID].1 % 4 == 0
            && sec[S_SUCC_LP].1 == n_succ
            && sec[S_META].1 == 4;
        if !ok || n_vocab >= u32::MAX as usize {
            return Err("语言模型各段长度不一致".into());
        }
        let lm = Lm { storage, sec, n_vocab, n_succ };
        lm.validate()?;
        Ok(lm)
    }

    fn validate(&self) -> Result<(), String> {
        let b = self.b();
        let blob_len = self.sec[S_VOCAB].1;
        let blob = std::str::from_utf8(&b[self.sec[S_VOCAB].0..self.sec[S_VOCAB].0 + blob_len])
            .map_err(|_| "词表不是合法 UTF-8")?;
        let mut prev_off = 0usize;
        let mut prev_word: Option<&str> = None;
        for i in 0..self.n_vocab {
            let (s, e) = (self.u32_at(S_VOCAB_OFF, i) as usize, self.u32_at(S_VOCAB_OFF, i + 1) as usize);
            if s != prev_off || e < s || e > blob_len || !blob.is_char_boundary(s) || !blob.is_char_boundary(e) {
                return Err("词表偏移不对".into());
            }
            let w = &blob[s..e];
            if prev_word.is_some_and(|p| p >= w) {
                return Err("词表没有排序".into());
            }
            prev_word = Some(w);
            prev_off = e;
            if !self.uni(i as u32).is_finite() || !self.backoff(i as u32).is_finite() {
                return Err("概率不是有限数".into());
            }
        }
        let mut prev = 0usize;
        for i in 0..=self.n_vocab {
            let v = self.u32_at(S_SUCC_OFF, i) as usize;
            if v < prev || v > self.n_succ {
                return Err("搭配表不单调".into());
            }
            prev = v;
        }
        if prev != self.n_succ {
            return Err("搭配表总数不对".into());
        }
        if !self.fallback(0.0).is_finite() {
            return Err("词表外分数不是有限数".into());
        }
        for v in 0..self.n_vocab {
            let r = self.succ_range(v as u32);
            let mut last: Option<u32> = None;
            for k in r {
                let id = self.u32_at(S_SUCC_ID, k);
                if id as usize >= self.n_vocab || last.is_some_and(|l| l >= id) {
                    return Err("搭配指向不存在的词或没有排序".into());
                }
                last = Some(id);
            }
        }
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

    pub fn n_vocab(&self) -> usize {
        self.n_vocab
    }

    pub fn n_bigrams(&self) -> usize {
        self.n_succ
    }

    pub fn byte_len(&self) -> usize {
        self.b().len()
    }

    pub fn word(&self, id: u32) -> &str {
        let i = id as usize;
        let (s, e) = (self.u32_at(S_VOCAB_OFF, i) as usize, self.u32_at(S_VOCAB_OFF, i + 1) as usize);
        let base = self.sec[S_VOCAB].0;
        std::str::from_utf8(&self.b()[base + s..base + e]).unwrap_or("")
    }

    /// 查词的编号（二分查找）。
    pub fn id(&self, text: &str) -> Option<u32> {
        let (mut lo, mut hi) = (0usize, self.n_vocab);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.word(mid as u32).as_bytes().cmp(text.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid as u32),
            }
        }
        None
    }

    /// 单独出现的对数概率。
    pub fn uni(&self, id: u32) -> f32 {
        f32::from_bits(self.u32_at(S_UNI, id as usize))
    }

    pub fn backoff(&self, id: u32) -> f32 {
        f32::from_bits(self.u32_at(S_BACKOFF, id as usize))
    }

    fn succ_range(&self, prev: u32) -> std::ops::Range<usize> {
        let p = prev as usize;
        self.u32_at(S_SUCC_OFF, p) as usize..self.u32_at(S_SUCC_OFF, p + 1) as usize
    }

    /// 语料里没怎么出现、不在词表里的词：用词库里的分数换算成单词概率。
    pub fn fallback(&self, dict_score: f32) -> f32 {
        dict_score + f32::from_bits(rd_u32(self.b(), self.sec[S_META].0))
    }

    /// 统计里见过的搭配的对数概率。
    pub fn bigram(&self, prev: u32, w: u32) -> Option<f32> {
        let r = self.succ_range(prev);
        let (mut lo, mut hi) = (r.start, r.end);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let id = self.u32_at(S_SUCC_ID, mid);
            match id.cmp(&w) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(dequantize(self.b()[self.sec[S_SUCC_LP].0 + mid])),
            }
        }
        None
    }

    /// 前一个词是 `prev` 时，下一个词是 `w` 的对数概率。
    pub fn logp(&self, prev: u32, w: u32) -> f32 {
        self.bigram(prev, w).unwrap_or_else(|| self.backoff(prev) + self.uni(w))
    }
}

/// 生成语言模型文件。
pub struct LmBuilder {
    /// 按字节排好序的词
    pub vocab: Vec<String>,
    pub uni: Vec<f32>,
    pub backoff: Vec<f32>,
    /// 每个词后面的搭配：(后一个词, 对数概率)，按后一个词升序
    pub succ: Vec<Vec<(u32, f32)>>,
    /// 词表外的词：词库分数加上这个数
    pub fallback_offset: f32,
}

impl LmBuilder {
    pub fn build(&self) -> Vec<u8> {
        assert_eq!(self.vocab.len(), self.uni.len());
        assert_eq!(self.vocab.len(), self.backoff.len());
        assert_eq!(self.vocab.len(), self.succ.len());
        let mut voff = vec![0u32];
        let mut blob = Vec::new();
        for w in &self.vocab {
            blob.extend_from_slice(w.as_bytes());
            voff.push(blob.len() as u32);
        }
        let mut soff = vec![0u32];
        let mut sid: Vec<u32> = Vec::new();
        let mut slp: Vec<u8> = Vec::new();
        for s in &self.succ {
            for &(id, lp) in s {
                sid.push(id);
                slp.push(quantize(lp));
            }
            soff.push(sid.len() as u32);
        }
        let u32s = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let f32s = |v: &[f32]| v.iter().flat_map(|x| x.to_bits().to_le_bytes()).collect::<Vec<u8>>();
        let sections: [Vec<u8>; NSEC] = [
            u32s(&voff),
            blob,
            f32s(&self.uni),
            f32s(&self.backoff),
            u32s(&soff),
            u32s(&sid),
            slp,
            f32s(&[self.fallback_offset]),
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

    pub fn sample() -> Lm {
        // 词表：把 吧 好 走（按字节排序）
        let vocab: Vec<String> = ["吧", "好", "把", "走"].iter().map(|s| s.to_string()).collect();
        let mut v = vocab.clone();
        v.sort();
        assert_eq!(v, vocab);
        let b = LmBuilder {
            vocab,
            uni: vec![-6.0, -5.0, -4.0, -6.5],
            backoff: vec![-1.0, -1.0, -1.0, -1.0],
            succ: vec![vec![], vec![(0, -1.0)], vec![], vec![(0, -0.5), (2, -6.0)]],
            fallback_offset: -2.0,
        };
        Lm::from_bytes(b.build()).unwrap()
    }

    #[test]
    fn reads_back() {
        let lm = sample();
        assert_eq!(lm.n_vocab(), 4);
        let (ba, hao, ba3, zou) =
            (lm.id("吧").unwrap(), lm.id("好").unwrap(), lm.id("把").unwrap(), lm.id("走").unwrap());
        assert!(lm.id("你").is_none());
        // 走 后面：吧 远比 把 常见
        assert!(lm.logp(zou, ba) > lm.logp(zou, ba3));
        assert_eq!(lm.logp(hao, ba), -1.0);
        // 没见过的搭配：退回系数 + 单独概率
        assert_eq!(lm.logp(ba, hao), -1.0 + -5.0);
        assert_eq!(lm.word(ba3), "把");
        assert_eq!(lm.fallback(-10.0), -12.0);
    }

    #[test]
    fn rejects_corrupted() {
        let lm = sample();
        let good = lm.b().to_vec();
        for i in 0..good.len() {
            let mut v = good.clone();
            v[i] ^= 0xFF;
            if let Ok(l) = Lm::from_bytes(v) {
                for a in 0..l.n_vocab() as u32 {
                    for b in 0..l.n_vocab() as u32 {
                        let _ = l.logp(a, b);
                    }
                    let _ = l.id(l.word(a));
                }
            }
        }
    }

    #[test]
    fn quantization_roundtrip() {
        assert_eq!(dequantize(quantize(-1.23)), -1.2);
        assert_eq!(dequantize(quantize(-99.0)), -25.5);
        assert_eq!(dequantize(quantize(0.5)), 0.0);
    }
}
