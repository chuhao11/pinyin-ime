//! 用真实词库测试打字效果。
//!
//! 这些测试检查的是"一类问题"而不是某几个词：比如随机打错一个字母能不能纠正回来、
//! 常用英文单词能不能出来、选过的词会不会把别的候选挤没。

mod common;

use pinyin_engine::{Ime, Key, Mods, UserDb};

fn typ(ime: &mut Ime, s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let k = if c == ' ' { Key::Space } else { Key::Char(c) };
        if let Some(t) = ime.process_key(k, Mods::default()).commit {
            out.push_str(&t);
        }
    }
    out
}

fn esc(ime: &mut Ime) {
    ime.process_key(Key::Escape, Mods::default());
}

/// 打 `input`，选中文字是 `want` 的候选（必要时打开矩阵用方向键），返回上屏的文字。
fn pick(ime: &mut Ime, want: &str) -> Option<String> {
    let i = ime.candidates().iter().position(|c| c.text == want).unwrap_or_else(|| panic!("找不到 {want}"));
    if i < ime.view().items.len() {
        return ime.process_key(Key::Char(char::from(b'1' + i as u8)), Mods::default()).commit;
    }
    ime.process_key(Key::Char('-'), Mods::default());
    while ime.view().page * 25 + ime.view().highlight < i {
        ime.process_key(Key::Right, Mods::default());
    }
    ime.process_key(Key::Space, Mods::default()).commit
}

fn rank_of(ime: &Ime, want: &str) -> Option<usize> {
    ime.candidates().iter().position(|c| c.text == want).map(|p| p + 1)
}

#[test]
fn typing_cases() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
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
        let (input, want, rule) = (f[0], f[1], f[2]);
        typ(&mut ime, input);
        let row = ime.view().items;
        let got = rank_of(&ime, want);
        let ok = if rule == "-" {
            // 不能出现在候选条里
            !row.iter().any(|t| t == want)
        } else {
            let max: usize = rule.parse().unwrap();
            got.is_some_and(|r| r <= max)
        };
        if !ok {
            failures.push(format!("{input} → 「{want}」要求 {rule}，实际第 {got:?}；候选条：{row:?}"));
        }
        esc(&mut ime);
        n += 1;
    }
    assert!(failures.is_empty(), "{} / {n} 条没通过：\n{}", failures.len(), failures.join("\n"));
}

/// 常用词随机打错一个字母（按到旁边的键、两个字母打反、漏打、多打），大部分应该能纠正回来。
#[test]
fn typos_are_corrected_in_general() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let words = [
        ("xihuan", "喜欢"),
        ("pengyou", "朋友"),
        ("diannao", "电脑"),
        ("gongzuo", "工作"),
        ("xuexiao", "学校"),
        ("zhidao", "知道"),
        ("xiexie", "谢谢"),
        ("shijian", "时间"),
        ("wenti", "问题"),
        ("dongxi", "东西"),
        ("shouji", "手机"),
        ("zhongguo", "中国"),
        ("shenme", "什么"),
        ("xianzai", "现在"),
        ("yinwei", "因为"),
        ("suoyi", "所以"),
        ("danshi", "但是"),
        ("yijing", "已经"),
        ("shenghuo", "生活"),
        ("shijie", "世界"),
        ("kaishi", "开始"),
        ("juede", "觉得"),
        ("xuyao", "需要"),
        ("difang", "地方"),
        ("haizi", "孩子"),
        ("laoshi", "老师"),
        ("mingtian", "明天"),
        ("jintian", "今天"),
        ("kuaile", "快乐"),
        ("xingfu", "幸福"),
    ];
    const ROWS: [&[u8]; 3] = [b"qwertyuiop", b"asdfghjkl", b"zxcvbnm"];
    let neighbor = |c: u8| -> u8 {
        for row in ROWS {
            if let Some(i) = row.iter().position(|&x| x == c) {
                return if i + 1 < row.len() { row[i + 1] } else { row[i - 1] };
            }
        }
        c
    };
    let mut total = 0;
    let mut hit = 0;
    let mut misses = Vec::new();
    for (py, want) in words {
        let b = py.as_bytes();
        let k = b.len() / 2;
        let mut variants: Vec<Vec<u8>> = Vec::new();
        // 按到旁边的键
        let mut v = b.to_vec();
        v[k] = neighbor(v[k]);
        variants.push(v);
        // 两个字母打反
        let mut v = b.to_vec();
        v.swap(k - 1, k);
        variants.push(v);
        // 漏打一个
        let mut v = b.to_vec();
        v.remove(k);
        variants.push(v);
        // 多打一个
        let mut v = b.to_vec();
        v.insert(k, b[k]);
        variants.push(v);
        for v in variants {
            let s = String::from_utf8(v).unwrap();
            if s == py {
                continue;
            }
            total += 1;
            typ(&mut ime, &s);
            if rank_of(&ime, want).is_some_and(|r| r <= 3) {
                hit += 1;
            } else {
                misses.push(format!("{s}→{want}（第 {:?}）", rank_of(&ime, want)));
            }
            esc(&mut ime);
        }
    }
    let rate = hit as f64 / total as f64;
    eprintln!("打错纠正：{hit}/{total} = {:.0}%；没纠正的：{misses:?}", rate * 100.0);
    assert!(rate >= 0.75, "纠正率只有 {:.0}%：{misses:?}", rate * 100.0);
}

/// 同样的错打第二次：上次选过的纠正结果直接排第一。
#[test]
fn same_typo_is_remembered() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    // xuwang 本身是"虚妄"，想打的是"希望"（xi 打成了 xu）
    typ(&mut ime, "xuwang");
    assert_ne!(ime.view().items[0], "希望");
    assert_eq!(pick(&mut ime, "希望").as_deref(), Some("希望"));
    typ(&mut ime, "xuwang");
    assert_eq!(ime.view().items[0], "希望");
    esc(&mut ime);
    // 学到的是"u 和 i 容易按错"：别的词里同样的错，扣分也变少了
    let before = {
        let Some(mut fresh) = common::real_ime(UserDb::in_memory()) else { return };
        typ(&mut fresh, "xuhuan");
        fresh.candidates().iter().find(|c| c.text == "喜欢").map(|c| c.score)
    };
    typ(&mut ime, "xuhuan");
    let after = ime.candidates().iter().find(|c| c.text == "喜欢").map(|c| c.score);
    assert!(after.unwrap() > before.unwrap(), "{before:?} → {after:?}");
}

/// 常用英文单词（不管是不是也能当拼音）都应该出现在前面。
#[test]
fn common_english_words_show_up() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let Some(en) = common::real_english() else { return };
    let mut words: Vec<String> = Vec::new();
    for i in 0..en.len() as u32 {
        let w = &en.word(i).text;
        if w.len() >= 4 && w.bytes().all(|b| b.is_ascii_lowercase()) {
            words.push(w.clone());
        }
        if words.len() >= 400 {
            break;
        }
    }
    let mut hit = 0;
    let mut misses = Vec::new();
    for w in &words {
        typ(&mut ime, w);
        if rank_of(&ime, w).is_some_and(|r| r <= 3) {
            hit += 1;
        } else {
            misses.push(format!("{w}（第 {:?}）", rank_of(&ime, w)));
        }
        esc(&mut ime);
    }
    let rate = hit as f64 / words.len() as f64;
    eprintln!("常用英文：{hit}/{} = {:.0}%；没进前三的：{misses:?}", words.len(), rate * 100.0);
    assert!(rate >= 0.85, "只有 {:.0}% 进了前三：{misses:?}", rate * 100.0);
}

/// 英文拼错了也能给出正确的词。
#[test]
fn misspelled_english_is_corrected() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let cases = [
        ("recieve", "receive"),
        ("accomodate", "accommodate"),
        ("definately", "definitely"),
        ("seperate", "separate"),
        ("occured", "occurred"),
        ("untill", "until"),
        ("becuase", "because"),
        ("freind", "friend"),
        ("beleive", "believe"),
        ("tommorow", "tomorrow"),
        ("enviroment", "environment"),
        ("goverment", "government"),
        ("wierd", "weird"),
        ("thier", "their"),
        ("begining", "beginning"),
        ("arguement", "argument"),
        ("calender", "calendar"),
        ("imediately", "immediately"),
        ("helpfull", "helpful"),
        ("whta", "what"),
    ];
    let mut hit = 0;
    let mut misses = Vec::new();
    for (typed, want) in cases {
        typ(&mut ime, typed);
        if rank_of(&ime, want).is_some_and(|r| r <= 5) {
            hit += 1;
        } else {
            misses.push(format!("{typed}→{want}（第 {:?}）", rank_of(&ime, want)));
        }
        esc(&mut ime);
    }
    assert!(hit * 100 >= cases.len() * 80, "{hit}/{}：{misses:?}", cases.len());
}

/// 输入全是完整音节时，首选不能"多猜字母"（zheba 不能出"这般"）。
#[test]
fn first_choice_matches_what_was_typed() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut bad = Vec::new();
    for input in ["zheba", "nihao", "women", "shiqing", "haode", "zouba", "chifan", "taba", "nage", "shide"] {
        typ(&mut ime, input);
        let c = &ime.candidates()[0];
        let code: String = c.code.iter().map(|&s| ime.dict().syllable(s)).collect();
        if code != input {
            bad.push(format!("{input} → {}（{code}）", c.text));
        }
        esc(&mut ime);
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// 选过一个词以后，其他候选还在（之前 fuqin 选了"父亲"就只剩一个候选）。
#[test]
fn learned_word_does_not_hide_others() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    for input in ["fuqin", "fangan", "shiqing", "pengyou"] {
        typ(&mut ime, input);
        let first = ime.view().items[0].clone();
        let n_before = ime.view().items.len();
        assert_eq!(typ(&mut ime, " "), first);
        typ(&mut ime, input);
        let n_after = ime.view().items.len();
        assert!(n_after >= 4.min(n_before), "{input}：选过「{first}」后候选条只剩 {n_after} 个（之前 {n_before} 个）");
        esc(&mut ime);
    }
}

/// 首选总被冷落（你一直选别的），3 次以后它被删掉。
#[test]
fn ignored_first_choice_is_deleted() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    for _ in 0..3 {
        typ(&mut ime, "fangan");
        assert_eq!(pick(&mut ime, "反感").as_deref(), Some("反感"));
    }
    typ(&mut ime, "fangan");
    assert_eq!(ime.view().items[0], "反感");
    assert!(rank_of(&ime, "方案").is_none(), "方案 应该被删掉了");
    esc(&mut ime);
    // 单字不会被删：一直选"事"，"是"还在第一
    for _ in 0..4 {
        typ(&mut ime, "shi");
        pick(&mut ime, "事");
    }
    typ(&mut ime, "shi");
    assert_eq!(ime.view().items[0], "是");
}

#[test]
fn useless_rare_chars_are_gone() {
    let Some(dict) = common::real_dict() else { return };
    for py in ["chun", "tuo", "e"] {
        assert!(dict.lookup(py).iter().all(|(t, _)| t != "鯙"), "{py}");
    }
    assert!(dict.lookup("chun").iter().any(|(t, _)| t == "春"));
}

#[test]
fn rare_chars_are_not_in_front() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    for (input, rare) in [("yi", "弌"), ("shi", "乨"), ("de", "锝"), ("de", "嘚")] {
        typ(&mut ime, input);
        let row = ime.view().items;
        assert!(!row.iter().any(|t| t == rare), "{input}: 候选条里出现了 {rare}：{row:?}");
        esc(&mut ime);
    }
}

#[test]
fn learned_word_does_not_hijack_sentence() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    // 记住"你好"以后，zhenhao 里的 n 不能被当成"你"的简拼
    assert_eq!(typ(&mut ime, "nihao,"), "你好,");
    typ(&mut ime, "jintiantianqizhenhao");
    assert_eq!(ime.view().items[0], "今天天气真好");
}

#[test]
fn name_is_remembered_after_one_use() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    typ(&mut ime, "lizhuoxuan");
    assert_eq!(pick(&mut ime, "李"), None);
    assert_eq!(pick(&mut ime, "卓"), None);
    assert_eq!(pick(&mut ime, "轩").as_deref(), Some("李卓轩"));
    typ(&mut ime, "lizhuoxuan");
    assert_eq!(ime.view().items[0], "李卓轩");
}
