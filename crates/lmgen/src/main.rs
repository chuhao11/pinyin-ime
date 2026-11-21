//! 语言模型生成工具：从中文语料里统计"哪个词后面常跟哪个词"，生成 data/build/pinyin.lm。
//!
//! 用法：
//!   cargo run --release -p lmgen -- build \
//!       --lccc data/raw/corpus/lccc_large.jsonl.gz \
//!       --wiki data/raw/corpus/zhwiki-20261101-pages-articles.xml.bz2
//!
//! 步骤：
//! 1. 解压读入语料（调用系统自带的 gzcat / bzcat）。维基百科去掉标记，只留正文。
//! 2. 繁体转简体（Unihan 的简体对应字），按非汉字切成小段。
//! 3. 用输入法自己的词库把每段切成词（选总概率最高的切法）。
//! 4. 数每个词出现的次数、每两个相邻的词一起出现的次数（多线程）。
//! 5. 算概率：搭配概率用"绝对折扣插值"（没见过的搭配退回到单词概率）；
//!    单词概率 = 0.8 × 语料里的频率 + 0.2 × 词库原来的权重（语料里没出现的词也有分）。
//! 6. 只留常见的搭配，量化后写文件。
//! 7. 每 200 条语料留一条不参加统计，抽一部分句子写进测试文件，用来测准确率。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use pinyin_engine::lm::{LmBuilder, NUM_TOKEN, START_TOKEN};
use pinyin_engine::Dict;

const MAX_WORD_CHARS: usize = 8;
const HOLDOUT_EVERY: usize = 200;
const EVAL_LCCC: usize = 3000;
const EVAL_WIKI: usize = 2000;
const EVAL_MIN_CHARS: usize = 4;
const EVAL_MAX_CHARS: usize = 12;
const UNI_CORPUS_WEIGHT: f64 = 0.8;
/// 语料里至少出现这么多次的词才进语言模型的词表（其余的用词库分数）。
const MIN_UNI_COUNT: u64 = 2;
/// 默认：对话语料的次数乘 3，维基乘 1（输入法只用这一个模型；排词时还会和词库频率各占一半）。
const CHAT_WEIGHT: u32 = 3;
const WIKI_WEIGHT: u32 = 1;

struct Args {
    dict: PathBuf,
    unihan: PathBuf,
    lccc: Option<PathBuf>,
    wiki: Option<PathBuf>,
    out: PathBuf,
    eval_out: PathBuf,
    max_bigrams: usize,
    limit: Option<usize>,
    chat_weight: u32,
    wiki_weight: u32,
}

fn parse_args(v: &[String]) -> Result<Args, String> {
    let mut a = Args {
        dict: "data/build/pinyin.dict".into(),
        unihan: "data/raw/unihan".into(),
        lccc: None,
        wiki: None,
        out: "data/build/pinyin.lm".into(),
        eval_out: "crates/engine/tests/eval_sentences.tsv".into(),
        max_bigrams: 22_000_000,
        limit: None,
        chat_weight: CHAT_WEIGHT,
        wiki_weight: WIKI_WEIGHT,
    };
    let mut it = v.iter();
    while let Some(k) = it.next() {
        let val = it.next().ok_or(format!("{k} 后面缺少参数"))?;
        match k.as_str() {
            "--dict" => a.dict = val.into(),
            "--unihan" => a.unihan = val.into(),
            "--lccc" => a.lccc = Some(val.into()),
            "--wiki" => a.wiki = Some(val.into()),
            "--out" => a.out = val.into(),
            "--eval-out" => a.eval_out = val.into(),
            "--max-bigrams" => a.max_bigrams = val.parse().map_err(|_| "数字不对")?,
            "--limit" => a.limit = Some(val.parse().map_err(|_| "数字不对")?),
            "--chat-weight" => a.chat_weight = val.parse().map_err(|_| "数字不对")?,
            "--wiki-weight" => a.wiki_weight = val.parse().map_err(|_| "数字不对")?,
            _ => return Err(format!("不认识的参数 {k}")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(|s| s.as_str()) {
        Some("build") => parse_args(&args[1..]).and_then(|a| build(&a)),
        // 调试用：打印前几篇维基清理后的正文
        Some("wikisample") => wiki_sample(args.get(1).map(PathBuf::from).unwrap_or_default()),
        _ => Err("用法：lmgen build [--lccc 文件] [--wiki 文件] [--out data/build/pinyin.lm]".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("出错：{e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------- 词表 ----------------

struct Lex {
    /// 词 → (编号, 词库里的分数)
    map: HashMap<String, (u32, f32)>,
    vocab: Vec<String>,
    /// 每个词最好的拼音
    code: Vec<Vec<u16>>,
    prior: Vec<f32>,
    chars: HashSet<char>,
    /// "前面是数字"这个特殊词的编号
    num: u32,
    /// "一段的开头"这个特殊词的编号
    start: u32,
}

fn build_lex(dict: &Dict) -> Lex {
    let mut best: HashMap<String, (f32, Vec<u16>)> = HashMap::new();
    dict.for_each_word(|code, w| {
        let t = dict.word_text(w);
        if t.chars().count() > MAX_WORD_CHARS || !t.chars().all(is_han) {
            return;
        }
        let s = dict.word_score(w);
        match best.get_mut(t) {
            Some(e) if e.0 >= s => {}
            Some(e) => *e = (s, code.to_vec()),
            None => {
                best.insert(t.to_string(), (s, code.to_vec()));
            }
        }
    });
    best.insert(NUM_TOKEN.to_string(), (-10.0, Vec::new()));
    best.insert(START_TOKEN.to_string(), (-10.0, Vec::new()));
    let mut vocab: Vec<String> = best.keys().cloned().collect();
    vocab.sort();
    let mut map = HashMap::with_capacity(vocab.len());
    let mut code = Vec::with_capacity(vocab.len());
    let mut prior = Vec::with_capacity(vocab.len());
    let mut chars = HashSet::new();
    for (i, w) in vocab.iter().enumerate() {
        let (s, c) = &best[w];
        map.insert(w.clone(), (i as u32, *s));
        code.push(c.clone());
        prior.push(*s);
        let mut it = w.chars();
        if let (Some(ch), None) = (it.next(), it.next()) {
            chars.insert(ch);
        }
    }
    let num = map[NUM_TOKEN].0;
    let start = map[START_TOKEN].0;
    Lex { map, vocab, code, prior, chars, num, start }
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F) || c == '〇'
}

/// 繁体 → 简体。只转成《通用规范汉字表》一、二级字（避免 綫 → 缐 这种转成生僻字的情况）。
fn load_trad(dir: &Path) -> Result<HashMap<char, char>, String> {
    let parse = |x: &str| x.strip_prefix("U+").and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
    let p = dir.join("Unihan_OtherMappings.txt");
    let mut tier: HashMap<char, u32> = HashMap::new();
    for line in fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))?.lines() {
        let mut f = line.split('\t');
        if let (Some(cp), Some("kTGH"), Some(v)) = (f.next(), f.next(), f.next()) {
            if let (Some(c), Some(i)) = (parse(cp), v.split(':').nth(1).and_then(|x| x.parse::<u32>().ok())) {
                tier.insert(
                    c,
                    if i <= 3500 {
                        1
                    } else if i <= 6500 {
                        2
                    } else {
                        3
                    },
                );
            }
        }
    }
    let p = dir.join("Unihan_Variants.txt");
    let text = fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))?;
    let mut m = HashMap::new();
    for line in text.lines() {
        let mut f = line.split('\t');
        if let (Some(cp), Some("kSimplifiedVariant"), Some(v)) = (f.next(), f.next(), f.next()) {
            let Some(c) = parse(cp) else { continue };
            let best = v
                .split_whitespace()
                .filter_map(parse)
                .filter(|t| *t != c && tier.get(t).is_some_and(|&x| x <= 2))
                .min_by_key(|t| tier[t]);
            if let Some(t) = best {
                m.insert(c, t);
            }
        }
    }
    Ok(m)
}

// ---------------- 切词 ----------------

/// 把一段文字切成连续的汉字小段（词库里没有的字、标点、字母都当分隔）。
fn runs<'a>(s: &'a str, lex: &Lex) -> Vec<&'a str> {
    runs_ctx(s, lex).into_iter().map(|x| x.1).collect()
}

/// 同上，另外告诉你这一段前面紧挨着的是不是数字（"2010年"的"年"）。
fn runs_ctx<'a>(s: &'a str, lex: &Lex) -> Vec<(bool, &'a str)> {
    let mut out = Vec::new();
    let mut start: Option<(usize, bool)> = None;
    let mut prev: Option<char> = None;
    for (i, c) in s.char_indices() {
        let ok = lex.chars.contains(&c);
        match (ok, start) {
            (true, None) => start = Some((i, prev.is_some_and(|p| p.is_ascii_digit() || ('０'..='９').contains(&p)))),
            (false, Some((st, d))) => {
                out.push((d, &s[st..i]));
                start = None;
            }
            _ => {}
        }
        prev = Some(c);
    }
    if let Some((st, d)) = start {
        out.push((d, &s[st..]));
    }
    out
}

/// 选总分最高的切法，返回词编号。
fn segment(run: &str, lex: &Lex, out: &mut Vec<u32>) {
    let offs: Vec<usize> = run.char_indices().map(|(i, _)| i).chain(std::iter::once(run.len())).collect();
    let n = offs.len() - 1;
    let mut best = vec![f32::NEG_INFINITY; n + 1];
    let mut back = vec![(0usize, u32::MAX); n + 1];
    best[0] = 0.0;
    for i in 1..=n {
        for l in 1..=MAX_WORD_CHARS.min(i) {
            if best[i - l] == f32::NEG_INFINITY {
                continue;
            }
            if let Some(&(id, sc)) = lex.map.get(&run[offs[i - l]..offs[i]]) {
                let v = best[i - l] + sc;
                if v > best[i] {
                    best[i] = v;
                    back[i] = (i - l, id);
                }
            }
        }
    }
    if best[n] == f32::NEG_INFINITY {
        return;
    }
    let start = out.len();
    let mut i = n;
    while i > 0 {
        let (p, id) = back[i];
        out.push(id);
        i = p;
    }
    out[start..].reverse();
}

fn normalize(s: &str, trad: &HashMap<char, char>) -> String {
    s.chars().filter(|c| !c.is_whitespace()).map(|c| *trad.get(&c).unwrap_or(&c)).collect()
}

// ---------------- 读语料 ----------------

/// 解析一行 JSON 字符串数组（对话语料的格式）。
fn parse_json_strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') | Some('t') | Some('r') => s.push(' '),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Ok(v) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(v) {
                                s.push(ch);
                            }
                        }
                    }
                    Some(x) => s.push(x),
                    None => break,
                },
                _ => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// 去掉成对的 open..close（可以嵌套）。
fn strip_nested(s: &str, open: &str, close: &str) -> String {
    let b = s.as_bytes();
    let (ob, cb) = (open.as_bytes(), close.as_bytes());
    let mut out = Vec::with_capacity(b.len());
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(ob) {
            depth += 1;
            i += ob.len();
        } else if depth > 0 && b[i..].starts_with(cb) {
            depth -= 1;
            i += cb.len();
        } else {
            if depth == 0 {
                out.push(b[i]);
            }
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// [[链接|显示文字]] → 显示文字；文件、分类链接整个去掉。
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("[[") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 2..];
        // 找配对的 ]]（允许嵌套）
        let ab = after.as_bytes();
        let (mut depth, mut i, mut end) = (1usize, 0usize, None);
        while i + 1 < ab.len() {
            if ab[i] == b'[' && ab[i + 1] == b'[' {
                depth += 1;
                i += 2;
            } else if ab[i] == b']' && ab[i + 1] == b']' {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        let Some(e) = end else {
            rest = "";
            break;
        };
        let inner = &after[..e];
        let lower = inner.to_lowercase();
        let skip = ["file:", "image:", "文件:", "图像:", "档案:", "category:", "分类:", "media:"]
            .iter()
            .any(|p| lower.trim_start().starts_with(p));
        if !skip {
            out.push_str(inner.rsplit('|').next().unwrap_or(""));
        }
        rest = &after[e + 2..];
    }
    out.push_str(rest);
    out
}

/// -{zh-hans:简;zh-hant:繁}- 取简体那一项。
fn strip_variants(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("-{") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 2..];
        let Some(e) = after.find("}-") else {
            rest = after;
            continue;
        };
        let inner = &after[..e];
        let pick = inner
            .split(';')
            .find_map(|x| {
                let x = x.trim();
                ["zh-hans:", "zh-cn:", "zh-sg:", "zh:"].iter().find_map(|p| x.strip_prefix(p))
            })
            .unwrap_or_else(|| inner.rsplit(':').next().unwrap_or(inner));
        out.push_str(pick);
        rest = &after[e + 2..];
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    // 先去掉 <ref>...</ref> 和注释，再去掉其余标签
    let mut t = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let a = rest.find("<ref");
        let c = rest.find("<!--");
        let p = match (a, c) {
            (Some(a), Some(c)) => a.min(c),
            (Some(a), None) => a,
            (None, Some(c)) => c,
            (None, None) => break,
        };
        t.push_str(&rest[..p]);
        let after = &rest[p..];
        if after.starts_with("<!--") {
            rest = after.find("-->").map(|e| &after[e + 3..]).unwrap_or("");
        } else {
            let gt = after.find('>').unwrap_or(after.len().saturating_sub(1));
            if after[..gt].ends_with('/') {
                rest = &after[(gt + 1).min(after.len())..];
            } else {
                rest = after.find("</ref>").map(|e| &after[e + 6..]).unwrap_or("");
            }
        }
    }
    t.push_str(rest);
    let mut out = String::with_capacity(t.len());
    let mut in_tag = false;
    for c in t.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn clean_wiki(raw: &str) -> String {
    let s = unescape_xml(raw);
    let s = strip_tags(&s);
    let s = strip_nested(&s, "{{", "}}");
    let s = strip_nested(&s, "{|", "|}");
    let s = strip_links(&s);
    let s = strip_variants(&s);
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let l = line.trim();
        if l.starts_with('=') || l.starts_with('|') || l.starts_with('!') || l.starts_with("[http") {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

enum Doc {
    /// 对话：几句话
    Chat(Vec<String>),
    /// 维基正文（还没清理）
    Wiki(String),
}

fn spawn_reader(path: &Path, tool: &str) -> Result<std::process::Child, String> {
    Command::new(tool)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 {tool} 失败：{e}"))
}

fn wiki_sample(path: PathBuf) -> Result<(), String> {
    let mut child = spawn_reader(&path, "bzcat")?;
    let rd = BufReader::new(child.stdout.take().unwrap());
    let (mut in_text, mut buf, mut n) = (false, String::new(), 0);
    for line in rd.lines() {
        let Ok(line) = line else { break };
        if !in_text {
            if let Some(p) = line.find("<text") {
                let after = &line[p..];
                if let Some(gt) = after.find('>') {
                    buf = after[gt + 1..].to_string() + "\n";
                    in_text = !after.contains("</text>");
                }
            }
            continue;
        }
        if let Some(e) = line.find("</text>") {
            buf.push_str(&line[..e]);
            in_text = false;
            if !buf.starts_with("#") {
                let c = clean_wiki(&buf);
                println!("==== 第 {} 篇 ====\n{}", n + 1, c.chars().take(400).collect::<String>());
                n += 1;
                if n >= 3 {
                    break;
                }
            }
        } else {
            buf.push_str(&line);
            buf.push('\n');
        }
    }
    let _ = child.kill();
    Ok(())
}

// ---------------- 主流程 ----------------

fn build(a: &Args) -> Result<(), String> {
    let t0 = Instant::now();
    let dict = Dict::open(&a.dict).map_err(|e| e.to_string())?;
    let lex = Arc::new(build_lex(&dict));
    let trad = Arc::new(load_trad(&a.unihan)?);
    println!("词表 {} 个词，{} 个单字（{:.1} 秒）", lex.vocab.len(), lex.chars.len(), t0.elapsed().as_secs_f64());

    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).max(2);
    let (doc_tx, doc_rx) = mpsc::sync_channel::<Vec<Doc>>(workers * 4);
    let doc_rx = Arc::new(Mutex::new(doc_rx));
    let (seq_tx, seq_rx) = mpsc::sync_channel::<Vec<(u32, Vec<u32>)>>(workers * 4);

    // 计数线程
    let v = lex.vocab.len();
    let lex_ids = (lex.num, lex.start);
    let counter = thread::spawn(move || {
        let mut uni = vec![0u64; v];
        let mut bi: HashMap<u64, u32> = HashMap::with_capacity(50_000_000);
        let mut tokens = 0u64;
        let (num, start) = lex_ids;
        for batch in seq_rx {
            for (wt, sent) in batch {
                let after_num = sent.first() == Some(&num);
                let _ = start;
                for (k, &id) in sent.iter().enumerate() {
                    // 紧跟在数字后面的词（2010年的"年"）不算进"单独出现"的次数，只算进"数字后面"的搭配
                    if !(after_num && k == 1) {
                        uni[id as usize] += wt as u64;
                        tokens += wt as u64;
                    }
                    if k > 0 {
                        let key = ((sent[k - 1] as u64) << 32) | id as u64;
                        *bi.entry(key).or_insert(0) += wt;
                    }
                }
            }
        }
        (uni, bi, tokens)
    });

    // 切词线程
    let mut handles = Vec::new();
    let (cw, ww) = (a.chat_weight, a.wiki_weight);
    for _ in 0..workers {
        let (rx, tx, lex, trad) = (doc_rx.clone(), seq_tx.clone(), lex.clone(), trad.clone());
        handles.push(thread::spawn(move || loop {
            let batch = { rx.lock().unwrap().recv() };
            let Ok(batch) = batch else { break };
            let mut out: Vec<(u32, Vec<u32>)> = Vec::new();
            for d in batch {
                let (wt, texts): (u32, Vec<String>) = match d {
                    Doc::Chat(v) => (cw, v),
                    Doc::Wiki(raw) => (ww, vec![clean_wiki(&raw)]),
                };
                for t in texts {
                    let t = normalize(&t, &trad);
                    for (after_digit, r) in runs_ctx(&t, &lex) {
                        // 每一段前面加一个特殊词：数字后面的是 <数字>，其余是 <句首>
                        let mut ids = vec![if after_digit { lex.num } else { lex.start }];
                        segment(r, &lex, &mut ids);
                        if ids.len() > 1 {
                            out.push((wt, ids));
                        }
                    }
                }
            }
            if tx.send(out).is_err() {
                break;
            }
        }));
    }
    drop(seq_tx);

    // 读语料（主线程），每 200 条留一条做测试
    let mut eval_chat: Vec<String> = Vec::new();
    let mut eval_wiki: Vec<String> = Vec::new();
    let mut n_chat = 0usize;
    let mut n_wiki = 0usize;
    if let Some(p) = &a.lccc {
        let mut child = spawn_reader(p, "gzcat")?;
        let rd = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
        let mut batch = Vec::with_capacity(2000);
        for line in rd.lines() {
            let Ok(line) = line else { break };
            n_chat += 1;
            let utts = parse_json_strings(&line);
            if n_chat.is_multiple_of(HOLDOUT_EVERY) {
                if eval_chat.len() < EVAL_LCCC * 4 {
                    eval_chat.extend(utts);
                }
                continue;
            }
            batch.push(Doc::Chat(utts));
            if batch.len() >= 2000 {
                doc_tx.send(std::mem::take(&mut batch)).map_err(|_| "切词线程出错")?;
            }
            if n_chat.is_multiple_of(2_000_000) {
                println!("  对话 {} 万条（{:.0} 秒）", n_chat / 10000, t0.elapsed().as_secs_f64());
            }
            if a.limit.is_some_and(|l| n_chat >= l) {
                break;
            }
        }
        if !batch.is_empty() {
            doc_tx.send(batch).map_err(|_| "切词线程出错")?;
        }
        let _ = child.kill();
        println!("对话语料读完：{} 万条（{:.0} 秒）", n_chat / 10000, t0.elapsed().as_secs_f64());
    }
    if let Some(p) = &a.wiki {
        let mut child = spawn_reader(p, "bzcat")?;
        let rd = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
        let mut batch = Vec::with_capacity(200);
        let (mut in_text, mut ns_ok) = (false, false);
        let mut buf = String::new();
        for line in rd.lines() {
            let Ok(line) = line else { break };
            if !in_text {
                let tl = line.trim_start();
                if tl.starts_with("<ns>") {
                    ns_ok = tl.starts_with("<ns>0</ns>");
                    continue;
                }
                let Some(p) = line.find("<text") else { continue };
                let after = &line[p..];
                let Some(gt) = after.find('>') else { continue };
                if after[..gt].ends_with('/') {
                    continue;
                }
                let content = &after[gt + 1..];
                buf.clear();
                if let Some(e) = content.find("</text>") {
                    buf.push_str(&content[..e]);
                } else {
                    buf.push_str(content);
                    buf.push('\n');
                    in_text = true;
                    continue;
                }
            } else if let Some(e) = line.find("</text>") {
                buf.push_str(&line[..e]);
                in_text = false;
            } else {
                buf.push_str(&line);
                buf.push('\n');
                continue;
            }
            // 一篇文章读完
            let redirect =
                buf.trim_start().to_lowercase().starts_with("#redirect") || buf.trim_start().starts_with("#重定向");
            if !ns_ok || redirect {
                continue;
            }
            n_wiki += 1;
            if n_wiki.is_multiple_of(HOLDOUT_EVERY) {
                if eval_wiki.len() < EVAL_WIKI * 20 {
                    eval_wiki.push(clean_wiki(&buf));
                }
                continue;
            }
            batch.push(Doc::Wiki(std::mem::take(&mut buf)));
            if batch.len() >= 200 {
                doc_tx.send(std::mem::take(&mut batch)).map_err(|_| "切词线程出错")?;
            }
            if n_wiki.is_multiple_of(100_000) {
                println!("  维基 {} 万篇（{:.0} 秒）", n_wiki / 10000, t0.elapsed().as_secs_f64());
            }
            if a.limit.is_some_and(|l| n_wiki >= l) {
                break;
            }
        }
        if !batch.is_empty() {
            doc_tx.send(batch).map_err(|_| "切词线程出错")?;
        }
        let _ = child.kill();
        println!("维基读完：{} 万篇（{:.0} 秒）", n_wiki / 10000, t0.elapsed().as_secs_f64());
    }
    drop(doc_tx);
    for h in handles {
        let _ = h.join();
    }
    let (uni, bi, tokens) = counter.join().map_err(|_| "计数线程出错")?;
    println!(
        "统计完：{} 亿个词，{} 万种搭配（{:.0} 秒）",
        tokens / 100_000_000,
        bi.len() / 10000,
        t0.elapsed().as_secs_f64()
    );

    // ---- 算概率 ----
    let n_tok = tokens.max(1) as f64;
    let zr: f64 = lex.prior.iter().map(|&s| (s as f64).exp()).sum();
    // 只留语料里出现过几次的词；新编号按文字顺序
    let mut new_id = vec![u32::MAX; v];
    let mut vocab: Vec<String> = Vec::new();
    for i in 0..v {
        if uni[i] >= MIN_UNI_COUNT {
            new_id[i] = vocab.len() as u32;
            vocab.push(lex.vocab[i].clone());
        }
    }
    let nv = vocab.len();
    let mut uni_lp = vec![0f32; nv];
    for i in 0..v {
        if new_id[i] != u32::MAX {
            let pc = uni[i] as f64 / n_tok;
            let pr = (lex.prior[i] as f64).exp() / zr;
            uni_lp[new_id[i] as usize] = (UNI_CORPUS_WEIGHT * pc + (1.0 - UNI_CORPUS_WEIGHT) * pr).ln() as f32;
        }
    }
    let fallback_offset = ((1.0 - UNI_CORPUS_WEIGHT).ln() - zr.ln()) as f32;
    let mut hist = vec![0u64; nv];
    let mut types = vec![0u32; nv];
    let (mut n1, mut n2) = (0u64, 0u64);
    let mut by_count = vec![0u64; 65];
    for (&k, &c) in &bi {
        let p = new_id[(k >> 32) as usize];
        if p == u32::MAX {
            continue;
        }
        hist[p as usize] += c as u64;
        types[p as usize] += 1;
        if c == 1 {
            n1 += 1;
        } else if c == 2 {
            n2 += 1;
        }
        by_count[(c as usize).min(64)] += 1;
    }
    let d = if n1 + n2 > 0 { n1 as f64 / (n1 as f64 + 2.0 * n2 as f64) } else { 0.75 };
    // 选一个最低次数，让留下的搭配不超过上限
    let mut cmin = 2usize;
    loop {
        let kept: u64 = by_count[cmin..].iter().sum();
        if kept as usize <= a.max_bigrams || cmin >= 64 {
            break;
        }
        cmin += 1;
    }
    let lambda: Vec<f64> =
        (0..nv).map(|i| if hist[i] > 0 { (d * types[i] as f64 / hist[i] as f64).min(1.0) } else { 1.0 }).collect();
    let backoff: Vec<f32> = lambda.iter().map(|l| l.ln() as f32).collect();
    let mut succ: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nv];
    let mut kept = 0usize;
    for (&k, &c) in &bi {
        if (c as usize) < cmin {
            continue;
        }
        let (p, w) = (new_id[(k >> 32) as usize], new_id[(k & 0xFFFF_FFFF) as usize]);
        if p == u32::MAX || w == u32::MAX {
            continue;
        }
        let (p, w) = (p as usize, w as usize);
        let pb = (c as f64 - d).max(0.0) / hist[p] as f64 + lambda[p] * (uni_lp[w] as f64).exp();
        let lp = pb.ln() as f32;
        // 和"退回估计"差不多的搭配不用存
        if lp - (backoff[p] + uni_lp[w]) < 0.1 {
            continue;
        }
        succ[p].push((w as u32, lp));
        kept += 1;
    }
    for s in succ.iter_mut() {
        s.sort_by_key(|x| x.0);
    }
    drop(bi);
    let builder = LmBuilder { vocab, uni: uni_lp, backoff, succ, fallback_offset };
    let bytes = builder.build();
    if let Some(dir) = a.out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&a.out, &bytes).map_err(|e| format!("写不了 {}：{e}", a.out.display()))?;
    println!(
        "语言模型已生成：{}（{:.1} MB，{} 个词，留下 {} 万种搭配，至少出现 {} 次，折扣 {:.2}，共 {:.0} 秒）",
        a.out.display(),
        bytes.len() as f64 / 1048576.0,
        nv,
        kept / 10000,
        cmin,
        d,
        t0.elapsed().as_secs_f64()
    );

    // ---- 测试句子 ----
    let mut out = String::from("# 没参加统计的句子：拼音<Tab>原句<Tab>来源（lmgen 自动生成）\n");
    let add = |texts: &[String], limit: usize, src: &str, out: &mut String| {
        let mut n = 0;
        let mut seen = HashSet::new();
        for t in texts {
            let t = normalize(t, &trad);
            for r in runs(&t, &lex) {
                let len = r.chars().count();
                if !(EVAL_MIN_CHARS..=EVAL_MAX_CHARS).contains(&len) || !seen.insert(r.to_string()) {
                    continue;
                }
                let mut ids = Vec::new();
                segment(r, &lex, &mut ids);
                let py: Vec<&str> =
                    ids.iter().flat_map(|&id| lex.code[id as usize].iter().map(|&s| dict.syllable(s))).collect();
                out.push_str(&format!("{}\t{}\t{}\n", py.concat(), r, src));
                n += 1;
                if n >= limit {
                    return;
                }
            }
        }
    };
    add(&eval_chat, EVAL_LCCC, "对话", &mut out);
    add(&eval_wiki, EVAL_WIKI, "维基", &mut out);
    let mut f = fs::File::create(&a.eval_out).map_err(|e| format!("写不了 {}：{e}", a.eval_out.display()))?;
    f.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
    println!("测试句子已写到 {}（{} 条）", a.eval_out.display(), out.lines().count() - 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_line() {
        let v = parse_json_strings(r#"["火锅 我 在", "哈哈 \"好\" 你"]"#);
        assert_eq!(v, vec!["火锅 我 在", "哈哈 \"好\" 你"]);
    }

    #[test]
    fn wiki_markup() {
        let raw = "'''北京'''是[[中华人民共和国]]的[[首都|首都城市]]&lt;ref&gt;来源&lt;/ref&gt;{{Infobox|a=b}}。\n[[File:x.jpg|图片说明]]\n== 历史 ==\n-{zh-hans:软件;zh-hant:軟體}-很好";
        let c = clean_wiki(raw);
        assert!(c.contains("北京"), "{c}");
        assert!(c.contains("是中华人民共和国的首都城市"), "{c}");
        assert!(!c.contains("来源"), "{c}");
        assert!(!c.contains("Infobox"), "{c}");
        assert!(!c.contains("图片说明"), "{c}");
        assert!(!c.contains("历史"), "{c}");
        assert!(c.contains("软件很好"), "{c}");
    }
}
