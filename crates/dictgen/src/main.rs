//! 词库编译工具：把雾凇拼音词库 + Unicode 汉字资料编译成输入法用的二进制词库。
//!
//! 用法：cargo run --release -p dictgen [-- --raw data/raw --out data/build/pinyin.dict --report docs/表外字清单.md]
//!
//! 规则（见 docs/方案.md）：
//! - 单字：只收雾凇字表（8105.dict.yaml）里的字，也就是《通用规范汉字表》8105 字加少量常用表外字。
//! - 词：词里每个字都必须"有用"：在通用规范汉字表或雾凇字表里，或者是在词库里有一定用量的表外字。
//!   用量很低的表外字（比如"鯙"）连同它所在的词一起删掉。
//! - 分数 = ln(权重) − ln(总权重) + 字等级扣分（二级、三级、表外字扣分，单字还按笔画轻微扣分）。

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pinyin_engine::DictBuilder;

/// 表外字在词库里的总权重达到这么多，或者出现在这么多个词里，就保留。
const KEEP_WEIGHT: u64 = 200;
const KEEP_WORDS: usize = 3;
/// 单字按等级扣分：[未用, 一级, 二级, 三级, 表外]
const CHAR_PEN: [f64; 5] = [0.0, 0.0, -1.0, -2.5, -4.0];
/// 词里每个字按等级扣分
const WORD_PEN: [f64; 5] = [0.0, 0.0, -0.3, -1.0, -2.0];
/// 单字每一笔扣分（频率接近时笔画少的在前）
const STROKE_PEN: f64 = -0.02;
const MAX_SYL: usize = 12;

struct Args {
    raw: PathBuf,
    out: PathBuf,
    report: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { raw: "data/raw".into(), out: "data/build/pinyin.dict".into(), report: None };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or(format!("{k} 后面缺少参数"))?;
        match k.as_str() {
            "--raw" => a.raw = v.into(),
            "--out" => a.out = v.into(),
            "--report" => a.report = Some(v.into()),
            _ => return Err(format!("不认识的参数 {k}")),
        }
    }
    Ok(a)
}

/// 读 rime 词库：跳过开头的说明（到 `...` 为止），返回每行按 Tab 分开的字段。
fn read_rime(path: &Path) -> Result<Vec<Vec<String>>, String> {
    let s = fs::read_to_string(path).map_err(|e| format!("读不了 {}：{e}", path.display()))?;
    let mut body = false;
    let mut rows = Vec::new();
    for line in s.lines() {
        let line = line.trim_end_matches('\r');
        if !body {
            body = line == "...";
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        rows.push(line.split('\t').map(|x| x.to_string()).collect());
    }
    if !body {
        return Err(format!("{} 格式不对（没有 ... 分隔行）", path.display()));
    }
    Ok(rows)
}

#[derive(Clone, Copy, Default)]
struct CharInfo {
    /// 1/2/3 = 通用规范汉字表一二三级，4 = 表外
    tier: u8,
    strokes: u32,
}

/// 繁体字：Unihan 里有简体对应字、而且简体不是它自己的字。
fn load_traditional(dir: &Path) -> Result<HashSet<char>, String> {
    let p = dir.join("Unihan_Variants.txt");
    let mut simp: HashMap<char, Vec<char>> = HashMap::new();
    for line in fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))?.lines() {
        let mut f = line.split('\t');
        let (Some(cp), Some("kSimplifiedVariant"), Some(v)) = (f.next(), f.next(), f.next()) else { continue };
        let Some(c) = cp.strip_prefix("U+").and_then(|x| u32::from_str_radix(x, 16).ok()).and_then(char::from_u32)
        else {
            continue;
        };
        for t in v.split_whitespace() {
            if let Some(t) = t.strip_prefix("U+").and_then(|x| u32::from_str_radix(x, 16).ok()).and_then(char::from_u32)
            {
                simp.entry(c).or_default().push(t);
            }
        }
    }
    Ok(simp.into_iter().filter(|(c, v)| !v.contains(c)).map(|(c, _)| c).collect())
}

fn load_unihan(dir: &Path) -> Result<HashMap<char, CharInfo>, String> {
    let mut info: HashMap<char, CharInfo> = HashMap::new();
    let parse = |line: &str| -> Option<(char, String, String)> {
        let mut f = line.split('\t');
        let cp = f.next()?.strip_prefix("U+")?;
        let ch = char::from_u32(u32::from_str_radix(cp, 16).ok()?)?;
        Some((ch, f.next()?.to_string(), f.next()?.to_string()))
    };
    let p = dir.join("Unihan_IRGSources.txt");
    for line in fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))?.lines() {
        if let Some((ch, field, v)) = parse(line) {
            if field == "kTotalStrokes" {
                let n = v.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(30);
                info.entry(ch).or_insert(CharInfo { tier: 4, strokes: 0 }).strokes = n;
            }
        }
    }
    let p = dir.join("Unihan_OtherMappings.txt");
    for line in fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))?.lines() {
        if let Some((ch, field, v)) = parse(line) {
            if field == "kTGH" {
                let idx: u32 = v.split(':').nth(1).and_then(|x| x.parse().ok()).unwrap_or(9999);
                let tier = if idx <= 3500 {
                    1
                } else if idx <= 6500 {
                    2
                } else {
                    3
                };
                info.entry(ch).or_insert(CharInfo { tier: 4, strokes: 30 }).tier = tier;
            }
        }
    }
    Ok(info)
}

struct WordRow {
    text: String,
    pinyin: String,
    weight: u64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("出错：{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let rime = args.raw.join("rime-ice");
    let info = load_unihan(&args.raw.join("unihan"))?;
    let tier = |c: char| info.get(&c).map(|i| i.tier).unwrap_or(4);
    let is_han = |c: char| info.contains_key(&c) || c == '〇';
    let trad = load_traditional(&args.raw.join("unihan"))?;
    // 表外的繁体字不要（表内的都是规范简体字）
    let is_trad = |c: char| tier(c) == 4 && trad.contains(&c);
    let mut trad_singles: Vec<char> = Vec::new();

    // ---- 单字 ----
    let mut chars: Vec<(char, String, u64)> = Vec::new();
    for r in read_rime(&rime.join("8105.dict.yaml"))? {
        if r.len() >= 3 {
            let mut it = r[0].chars();
            if let (Some(c), None) = (it.next(), it.next()) {
                if is_trad(c) {
                    if !trad_singles.contains(&c) {
                        trad_singles.push(c);
                    }
                    continue;
                }
                chars.push((c, r[1].clone(), r[2].parse().unwrap_or(0)));
            }
        }
    }
    let char_table: HashSet<char> = chars.iter().map(|x| x.0).collect();
    let in_table = |c: char| char_table.contains(&c) || tier(c) <= 3;

    // ---- 词 ----
    let mut words: Vec<WordRow> = Vec::new();
    for f in ["base", "ext"] {
        for r in read_rime(&rime.join(format!("{f}.dict.yaml")))? {
            if r.len() >= 3 {
                words.push(WordRow { text: r[0].clone(), pinyin: r[1].clone(), weight: r[2].parse().unwrap_or(0) });
            }
        }
    }
    let mut known: HashMap<String, (String, u64)> = HashMap::new();
    for w in &words {
        let e = known.entry(w.text.clone()).or_insert((w.pinyin.clone(), w.weight));
        if w.weight > e.1 {
            *e = (w.pinyin.clone(), w.weight);
        }
    }
    // 容错读音（没有权重的，用同一个词已有的权重）
    for r in read_rime(&rime.join("others.dict.yaml"))? {
        if r.len() >= 2 {
            let weight = r.get(2).and_then(|x| x.parse().ok()).or_else(|| known.get(&r[0]).map(|k| k.1)).unwrap_or(100);
            words.push(WordRow { text: r[0].clone(), pinyin: r[1].clone(), weight });
        }
    }
    // 腾讯词表没有拼音：用已知的词和每个字最常用的读音拼出来
    let mut char_best: HashMap<char, (String, u64)> = HashMap::new();
    for (c, py, w) in &chars {
        let e = char_best.entry(*c).or_insert((py.clone(), *w));
        if *w > e.1 {
            *e = (py.clone(), *w);
        }
    }
    let derive = |text: &str| -> Option<String> {
        let cs: Vec<char> = text.chars().collect();
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < cs.len() {
            let mut matched = false;
            for j in (i + 2..=(i + 6).min(cs.len())).rev() {
                let piece: String = cs[i..j].iter().collect();
                if piece.chars().count() == cs.len() {
                    continue;
                }
                if let Some((py, _)) = known.get(&piece) {
                    out.push(py.clone());
                    i = j;
                    matched = true;
                    break;
                }
            }
            if !matched {
                out.push(char_best.get(&cs[i])?.0.clone());
                i += 1;
            }
        }
        Some(out.join(" "))
    };
    let mut tencent_ok = 0usize;
    let mut tencent_skip = 0usize;
    for r in read_rime(&rime.join("tencent.dict.yaml"))? {
        if r.len() >= 2 {
            if known.contains_key(&r[0]) {
                continue;
            }
            match derive(&r[0]) {
                Some(py) => {
                    words.push(WordRow { text: r[0].clone(), pinyin: py, weight: r[1].parse().unwrap_or(100) });
                    tencent_ok += 1;
                }
                None => tencent_skip += 1,
            }
        }
    }

    // ---- 表外字：按在词里的用量决定去留 ----
    let mut usage: HashMap<char, (usize, u64, String)> = HashMap::new();
    for w in &words {
        for c in w.text.chars() {
            if is_han(c) && !in_table(c) {
                let e = usage.entry(c).or_insert((0, 0, w.text.clone()));
                e.0 += 1;
                e.1 += w.weight;
            }
        }
    }
    let kept_extra: HashSet<char> = usage
        .iter()
        .filter(|(c, u)| !is_trad(**c) && (u.1 >= KEEP_WEIGHT || u.0 >= KEEP_WORDS))
        .map(|(c, _)| *c)
        .collect();
    let char_ok = |c: char| {
        if is_han(c) {
            in_table(c) || kept_extra.contains(&c)
        } else {
            c == '·' || c == '-' || c.is_ascii_alphanumeric()
        }
    };

    // ---- 音节表 ----
    let mut syllables: HashSet<String> = HashSet::new();
    for (_, py, _) in &chars {
        syllables.insert(py.clone());
    }
    for w in &words {
        for p in w.pinyin.split_whitespace() {
            if p.bytes().all(|b| b.is_ascii_lowercase()) {
                syllables.insert(p.to_string());
            }
        }
    }
    let mut b = DictBuilder::new(syllables);

    // ---- 分数 ----
    let total: u64 = chars.iter().map(|x| x.2).sum::<u64>() + words.iter().map(|w| w.weight).sum::<u64>();
    let ln_total = (total as f64).ln();

    let mut n_chars = 0usize;
    for (c, py, w) in &chars {
        let Some(code) = b.code(py) else { continue };
        let i = info.get(c).copied().unwrap_or(CharInfo { tier: 4, strokes: 30 });
        let t = if char_table.contains(c) && i.tier == 4 { 4 } else { i.tier.clamp(1, 4) };
        let score = ((*w as f64) + 1.0).ln() - ln_total + CHAR_PEN[t as usize] + STROKE_PEN * i.strokes as f64;
        b.add(&code, &c.to_string(), score as f32);
        n_chars += 1;
    }

    let mut seen: HashMap<(String, String), u64> = HashMap::new();
    let mut dropped_rare = 0usize;
    let mut dropped_other = 0usize;
    for w in &words {
        if w.text.chars().count() < 2 {
            continue;
        }
        if !w.text.chars().all(char_ok) {
            if w.text.chars().any(|c| is_han(c) && !in_table(c) && !kept_extra.contains(&c)) {
                dropped_rare += 1;
            } else {
                dropped_other += 1;
            }
            continue;
        }
        let key = (w.text.clone(), w.pinyin.clone());
        let e = seen.entry(key).or_insert(0);
        *e = (*e).max(w.weight);
    }
    let mut n_words = 0usize;
    let mut bad_pinyin = 0usize;
    for ((text, py), weight) in &seen {
        let Some(code) = b.code(py) else {
            bad_pinyin += 1;
            continue;
        };
        if code.len() > MAX_SYL {
            continue;
        }
        let pen: f64 = text.chars().filter(|c| is_han(*c)).map(|c| WORD_PEN[tier(c).clamp(1, 4) as usize]).sum();
        let score = ((*weight as f64) + 1.0).ln() - ln_total + pen;
        b.add(&code, text, score as f32);
        n_words += 1;
    }

    let bytes = b.build();
    if let Some(dir) = args.out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&args.out, &bytes).map_err(|e| format!("写不了 {}：{e}", args.out.display()))?;
    println!("词库已生成：{}（{:.1} MB）", args.out.display(), bytes.len() as f64 / 1048576.0);
    println!("  单字 {n_chars} 条，词 {n_words} 条（腾讯词表补充 {tencent_ok} 条，拼不出拼音跳过 {tencent_skip} 条）");
    println!(
        "  因为含没用的生僻字删掉 {dropped_rare} 个词，含其他符号删掉 {dropped_other} 个词，拼音不认识 {bad_pinyin} 个"
    );
    println!("  表外字：保留 {} 个，删掉 {} 个", kept_extra.len(), usage.len() - kept_extra.len());

    if let Some(report) = &args.report {
        let extra_in_table: Vec<char> = {
            let mut v: Vec<char> = char_table.iter().copied().filter(|c| tier(*c) == 4).collect();
            v.sort();
            v
        };
        let mut kept: Vec<(char, &(usize, u64, String))> =
            usage.iter().filter(|(c, _)| kept_extra.contains(c)).map(|(c, u)| (*c, u)).collect();
        kept.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(b.1 .0.cmp(&a.1 .0)));
        let mut dropped: Vec<(char, &(usize, u64, String))> =
            usage.iter().filter(|(c, _)| !kept_extra.contains(c)).map(|(c, u)| (*c, u)).collect();
        dropped.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(b.1 .0.cmp(&a.1 .0)));

        let mut md = String::new();
        let _ = writeln!(md, "# 表外字清单\n");
        let _ =
            writeln!(md, "> 由 `cargo run --release -p dictgen -- --report docs/表外字清单.md` 自动生成，不要手改。\n");
        let _ = writeln!(md, "规则：");
        let _ = writeln!(md, "- 《通用规范汉字表》8105 字全部保留。");
        let _ =
            writeln!(md, "- 雾凇拼音字表另收的 {} 个表外字保留，可以单独打出来（见第一部分）。", extra_in_table.len());
        let _ = writeln!(
            md,
            "- 其余表外字：在词库里总权重 ≥ {KEEP_WEIGHT} 或出现在 ≥ {KEEP_WORDS} 个词里的保留，只在词里出现，不单独当候选（第二部分）；"
        );
        let _ = writeln!(md, "  其余的连同所在的词一起删掉（第三部分列出用量最高的，拿不准的可以挑出来加回去）。");
        let _ = writeln!(md, "- 表外的繁体字一律不要，连同所在的词一起删掉（第三部分里也会列出）。");
        let _ = writeln!(md, "- 只在大字表里、从来不出现在任何词里的字（比如\"鯙\"）根本不收，这里也不列。\n");
        trad_singles.sort();
        let _ = writeln!(md, "雾凇字表里因为是繁体而去掉的单字：{}\n", trad_singles.iter().collect::<String>());
        let _ = writeln!(md, "## 一、雾凇字表额外收录（{} 个，保留）\n", extra_in_table.len());
        let _ = writeln!(md, "{}\n", extra_in_table.iter().collect::<String>());
        let _ = writeln!(md, "## 二、因为在词里有用量而保留（{} 个，只在词里出现）\n", kept.len());
        let _ = writeln!(md, "| 字 | 出现的词数 | 总权重 | 例词 |\n|---|---|---|---|");
        for (c, u) in &kept {
            let _ = writeln!(md, "| {c} | {} | {} | {} |", u.0, u.1, u.2);
        }
        let _ = writeln!(
            md,
            "\n## 三、删掉的表外字，用量最高的 100 个（共删 {} 个字、{dropped_rare} 个词）\n",
            dropped.len()
        );
        let _ = writeln!(md, "| 字 | 出现的词数 | 总权重 | 例词 |\n|---|---|---|---|");
        for (c, u) in dropped.iter().take(100) {
            let _ = writeln!(md, "| {c} | {} | {} | {} |", u.0, u.1, u.2);
        }
        fs::write(report, md).map_err(|e| format!("写不了 {}：{e}", report.display()))?;
        println!("清单已写到 {}", report.display());
    }
    build_english(&args)?;
    Ok(())
}

/// 英文词表：雾凇拼音的英文词库（整理过的常用词 + 缩写/互联网词）定词表，
/// Google 万亿词语料的频率排名定分数（排名 r 的词频率约为 0.1/r）。
/// 输出 data/build/english.tsv：每行"词<Tab>分数"。
fn build_english(args: &Args) -> Result<(), String> {
    let dir = args.raw.join("english");
    let ranks_text = fs::read_to_string(dir.join("20k.txt")).map_err(|e| format!("读不了 20k.txt：{e}"))?;
    let mut rank: HashMap<String, usize> = HashMap::new();
    for (i, w) in ranks_text.lines().enumerate() {
        let w = w.trim().to_ascii_lowercase();
        if !w.is_empty() {
            rank.entry(w).or_insert(i + 1);
        }
    }
    let mut words: Vec<(String, f64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in ["en.dict.yaml", "en_ext.dict.yaml"] {
        for r in read_rime(&dir.join(f))? {
            let w = r[0].trim();
            // 只要纯字母的词（输入框里打不出空格和横杠）；单个字母不要（除了 I）
            if w.is_empty() || !w.bytes().all(|b| b.is_ascii_alphabetic()) || (w.len() == 1 && w != "I") {
                continue;
            }
            if !seen.insert(w.to_string()) {
                continue;
            }
            let r = rank.get(&w.to_ascii_lowercase()).copied().unwrap_or(25_000);
            words.push((w.to_string(), (0.1 / r as f64).ln()));
        }
    }
    words.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::new();
    for (w, sc) in &words {
        let _ = writeln!(out, "{w}\t{sc:.3}");
    }
    let path = args.out.with_file_name("english.tsv");
    fs::write(&path, out).map_err(|e| format!("写不了 {}：{e}", path.display()))?;
    println!("英文词表已生成：{}（{} 个词）", path.display(), words.len());
    Ok(())
}
