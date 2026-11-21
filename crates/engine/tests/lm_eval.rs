//! 用"没参加统计的句子"测整句转换的准确率：有语言模型 vs 没有语言模型。
//!
//! 句子来自 lmgen 生成的 tests/eval_sentences.tsv（对话语料和维基百科里各留出来的一部分）。
//! 把每句话转成拼音交给输入法，看第一个候选是不是原句。

mod common;

use std::path::PathBuf;

use pinyin_engine::{analyze_with, Query, UserDb};

struct Score {
    total: usize,
    exact: usize,
    chars: usize,
    char_ok: usize,
    /// 含"吧/把/的/得/地"这些看上下文的字的句子
    ctx_total: usize,
    ctx_exact: usize,
}

impl Score {
    fn new() -> Self {
        Score { total: 0, exact: 0, chars: 0, char_ok: 0, ctx_total: 0, ctx_exact: 0 }
    }
    fn pct(a: usize, b: usize) -> f64 {
        100.0 * a as f64 / b.max(1) as f64
    }
    fn line(&self, name: &str) -> String {
        format!(
            "{name}：整句全对 {:.1}%，字准确率 {:.1}%，含吧/把/的/得/地的句子全对 {:.1}%（共 {} 句）",
            Self::pct(self.exact, self.total),
            Self::pct(self.char_ok, self.chars),
            Self::pct(self.ctx_exact, self.ctx_total),
            self.total
        )
    }
}

#[test]
fn language_model_improves_sentences() {
    let Some(ime) = common::real_ime(UserDb::in_memory()) else { return };
    let Some(lm) = ime.lm() else {
        if std::env::var("REQUIRE_DICT").is_ok() {
            panic!("找不到语言模型 pinyin.lm");
        }
        eprintln!("跳过：没有语言模型");
        return;
    };
    let path = std::env::var("PINYIN_EVAL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval_sentences.tsv"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("跳过：没有测试句子 {}", path.display());
        return;
    };
    let user = UserDb::in_memory();
    let mut with = [Score::new(), Score::new()];
    let mut without = [Score::new(), Score::new()];
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        let (py, want, src) = (f[0], f[1], f[2]);
        let k = usize::from(src != "对话");
        let ctx_chars = want.chars().any(|c| "吧把的得地".contains(c));
        for (lm_opt, sc) in [(Some(lm), &mut with[k]), (None, &mut without[k])] {
            let q = Query { dict: ime.dict(), en: None, lm: lm_opt, user: &user, prev: None, now: 0 };
            let c = analyze_with(&q, py);
            let got = c.first().map(|c| c.text.as_str()).unwrap_or("");
            sc.total += 1;
            let ok = got == want;
            sc.exact += usize::from(ok);
            let (a, b): (Vec<char>, Vec<char>) = (got.chars().collect(), want.chars().collect());
            sc.chars += b.len();
            if a.len() == b.len() {
                sc.char_ok += a.iter().zip(&b).filter(|(x, y)| x == y).count();
            }
            if ctx_chars {
                sc.ctx_total += 1;
                sc.ctx_exact += usize::from(ok);
            }
        }
    }
    for (k, name) in [(0, "对话"), (1, "维基")] {
        if with[k].total > 0 {
            eprintln!("{}", without[k].line(&format!("{name}·没有语言模型")));
            eprintln!("{}", with[k].line(&format!("{name}·有语言模型")));
        }
    }
    let all = |s: &[Score; 2]| (s[0].exact + s[1].exact, s[0].total + s[1].total);
    let (we, wt) = all(&with);
    let (ne, nt) = all(&without);
    let (w, n) = (Score::pct(we, wt), Score::pct(ne, nt));
    eprintln!("合计：整句全对 没有模型 {n:.1}% → 有模型 {w:.1}%");
    // 语言模型必须明显更好
    assert!(w >= n + 5.0, "语言模型没有带来明显提升：{n:.1}% → {w:.1}%");
}
