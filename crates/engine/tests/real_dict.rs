//! 用真实词库跑打字回归测试（tests/cases.tsv）。

mod common;

use pinyin_engine::lattice::analyze;
use pinyin_engine::{Ime, Key, Mods, UserDb};

#[test]
fn typing_cases() {
    let Some(dict) = common::real_dict() else { return };
    let user = UserDb::in_memory();
    let cases = include_str!("cases.tsv");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in cases.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        assert_eq!(f.len(), 3, "格式不对：{line}");
        let (input, want, max_rank) = (f[0], f[1], f[2].parse::<usize>().unwrap());
        let cands = analyze(&dict, &user, input, 0);
        let got = cands.iter().position(|c| c.text == want).map(|p| p + 1);
        if got.is_none_or(|r| r > max_rank) {
            let top: Vec<&str> = cands.iter().take(8).map(|c| c.text.as_str()).collect();
            failures.push(format!("{input} → 期望「{want}」在前 {max_rank}，实际第 {got:?}；前几个：{top:?}"));
        }
        n += 1;
    }
    assert!(failures.is_empty(), "{} / {n} 条没通过：\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn useless_rare_chars_are_gone() {
    let Some(dict) = common::real_dict() else { return };
    // "鯙"只在大字表里，不应该出现
    for py in ["chun", "tuo", "e"] {
        assert!(dict.lookup(py).iter().all(|(t, _)| t != "鯙"), "{py}");
    }
    // 常用的单字还在
    assert!(dict.lookup("chun").iter().any(|(t, _)| t == "春"));
}

#[test]
fn rare_chars_are_not_in_front() {
    let Some(dict) = common::real_dict() else { return };
    let mut ime = Ime::new(dict, UserDb::in_memory());
    // 候选条里不应该出现生僻字——抽查几个
    for (input, rare) in [("yi", "弌"), ("shi", "乨"), ("de", "锝"), ("de", "嘚")] {
        for c in input.chars() {
            ime.process_key(Key::Char(c), Mods::default());
        }
        let row = ime.view().items;
        assert!(!row.iter().any(|t| t == rare), "{input}: 候选条里出现了 {rare}：{row:?}");
        ime.process_key(Key::Escape, Mods::default());
    }
}

#[test]
fn learned_word_does_not_hijack_sentence() {
    let Some(dict) = common::real_dict() else { return };
    let mut ime = Ime::new(dict, UserDb::in_memory());
    ime.set_clock(|| 1_800_000_000);
    let typ = |ime: &mut Ime, s: &str| -> String {
        let mut out = String::new();
        for c in s.chars() {
            if let Some(t) = ime.process_key(Key::Char(c), Mods::default()).commit {
                out.push_str(&t);
            }
        }
        out
    };
    // 记住"你好"以后，zhenhao 里的 n 不能被当成"你"的简拼
    assert_eq!(typ(&mut ime, "nihao,"), "你好,");
    typ(&mut ime, "jintiantianqizhenhao");
    assert_eq!(ime.view().items[0], "今天天气真好");
}

#[test]
fn name_is_remembered_after_one_use() {
    let Some(dict) = common::real_dict() else { return };
    let mut ime = Ime::new(dict, UserDb::in_memory());
    ime.set_clock(|| 1_800_000_000);
    let typ = |ime: &mut Ime, s: &str| {
        for c in s.chars() {
            ime.process_key(Key::Char(c), Mods::default());
        }
    };
    // 一个词库里没有的名字：分几次选出来
    typ(&mut ime, "lizhuoxuan");
    let pick = |ime: &mut Ime, t: &str| -> Option<String> {
        let i = ime.candidates().iter().position(|c| c.text == t).unwrap_or_else(|| panic!("找不到 {t}"));
        // 不在候选条里就打开矩阵，用方向键走过去
        if i < ime.view().items.len() {
            return ime.process_key(Key::Char(char::from(b'1' + i as u8)), Mods::default()).commit;
        }
        ime.process_key(Key::Char('-'), Mods::default());
        while ime.view().page * 25 + ime.view().highlight < i {
            ime.process_key(Key::Right, Mods::default());
        }
        ime.process_key(Key::Space, Mods::default()).commit
    };
    assert_eq!(pick(&mut ime, "李"), None);
    assert_eq!(pick(&mut ime, "卓"), None);
    assert_eq!(pick(&mut ime, "轩").as_deref(), Some("李卓轩"));
    typ(&mut ime, "lizhuoxuan");
    assert_eq!(ime.view().items[0], "李卓轩");
}
