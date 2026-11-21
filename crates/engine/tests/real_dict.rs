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
    // 去掉上文（刚上屏的"希望"），只比较纠错扣分的变化
    ime.clear_context();
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

/// 选过一次的冷门词：第一个仍是常用词，冷门词放进前 4 个，下次一眼能看到。
#[test]
fn rare_pick_goes_to_front_four_not_first() {
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut checked = 0;
    for input in ["shiqing", "zhidao", "keyi", "shijian", "dongxi", "xianzai", "shi", "yi", "de"] {
        typ(&mut ime, input);
        let first = ime.view().items[0].clone();
        // 选一个排在第 10 个以后、同样把输入全对上的候选
        let n = ime.candidates().len();
        let Some(i) = (10..n).find(|&i| ime.candidates()[i].consumed == ime.candidates()[0].consumed) else {
            esc(&mut ime);
            continue;
        };
        let want = ime.candidates()[i].text.clone();
        assert_eq!(pick(&mut ime, &want).as_deref(), Some(want.as_str()));
        ime.clear_context();
        typ(&mut ime, input);
        let row = ime.view().items;
        assert_eq!(row[0], first, "{input}：选了一次「{want}」，第一个不该变");
        assert!(row.iter().take(4).any(|t| *t == want), "{input}：「{want}」不在前 4 个：{row:?}");
        esc(&mut ime);
        checked += 1;
    }
    assert!(checked >= 5);
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
    // 单字不会被删：一直选"事"，"是"还在第一（每次之间断开上文，只看单字本身的记忆）
    for _ in 0..4 {
        fresh(&mut ime);
        typ(&mut ime, "shi");
        pick(&mut ime, "事");
    }
    fresh(&mut ime);
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

/// 所有音节（不含 lve 这种别的写法以外的特殊情况），给下面的整类测试用。
fn all_syllables() -> Vec<String> {
    common::real_dict().map(|d| d.syllables().to_vec()).unwrap_or_default()
}

fn is_single_char(ime: &Ime, i: usize) -> bool {
    let c = &ime.candidates()[i];
    c.code.len() == 1 && c.text.chars().count() == 1 && c.fixes.is_empty()
}

/// 清空输入和上文（按一下回车：没在打字时回车原样交给程序，上文就断了）。
fn fresh(ime: &mut Ime) {
    esc(ime);
    ime.process_key(Key::Enter, Mods::default());
}

#[test]
fn full_syllable_shows_single_chars_first() {
    // 打的正好是一个完整音节（gun、lin、ban、xian）：前 3 个都是这个音的单字，
    // 不能是把音节拆开的简拼词（gu+n=姑娘、li+n=理念）或别的词，除非你经常打那个词
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut bad = Vec::new();
    let mut checked = 0;
    for syl in all_syllables() {
        fresh(&mut ime);
        typ(&mut ime, &syl);
        let v = ime.view();
        // 整个音节都对上的常用单字（生僻字标成"差"，不算；只对上前半截的也不算）
        let full = |i: usize| ime.candidates()[i].consumed == syl.len();
        let singles = (0..ime.candidates().len())
            .filter(|&i| is_single_char(&ime, i) && full(i) && !ime.candidates()[i].poor)
            .count();
        let want = singles.min(3).min(v.items.len());
        if want == 0 {
            continue;
        }
        checked += 1;
        if !(0..want).all(|i| is_single_char(&ime, i) && full(i)) {
            bad.push(format!("{syl}: {}", v.items.join(" ")));
        }
    }
    esc(&mut ime);
    assert!(checked > 350, "只查了 {checked} 个音节");
    assert!(bad.is_empty(), "{} 个音节前面不是单字：\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn often_used_word_can_still_come_first() {
    // 你经常打的词例外：比如一直用 gun 打"姑娘"，它可以排到单字前面
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    for _ in 0..3 {
        typ(&mut ime, "gun");
        pick(&mut ime, "姑娘");
    }
    typ(&mut ime, "gun");
    assert!(rank_of(&ime, "姑娘").unwrap() <= 2, "{:?}", ime.view().items);
}

#[test]
fn your_choice_after_a_word_beats_the_context() {
    // 上文让别的字排了第一（比如刚打了"滚"，再打 gun 排出"棍"），你选了原来那个：
    // 下次同样的上文，第一个就是你选的那个。对每个音节都检查
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut bad = Vec::new();
    for syl in all_syllables() {
        fresh(&mut ime);
        typ(&mut ime, &syl);
        if ime.candidates().is_empty() || !is_single_char(&ime, 0) {
            continue;
        }
        let first = ime.candidates()[0].text.clone();
        pick(&mut ime, &first);
        for _ in 0..2 {
            typ(&mut ime, &syl);
            if ime.view().items.first() != Some(&first) {
                if !ime.candidates().iter().any(|c| c.text == first) {
                    break;
                }
                pick(&mut ime, &first);
            } else {
                esc(&mut ime);
                break;
            }
        }
        fresh(&mut ime);
        typ(&mut ime, &syl);
        pick(&mut ime, &first);
        typ(&mut ime, &syl);
        let now = ime.view().items.first().cloned().unwrap_or_default();
        if now != first {
            bad.push(format!("{syl}: 一直选 {first}，排第一的却是 {now}"));
        }
    }
    esc(&mut ime);
    assert!(bad.is_empty(), "{} 个音节：\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn char_picked_dozens_of_times_comes_first() {
    // 一直选第二个字，几十次以后它排第一（连续选，上文就是它自己）
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut bad = Vec::new();
    let mut checked = 0;
    for syl in all_syllables() {
        fresh(&mut ime);
        typ(&mut ime, &syl);
        let full = |i: usize| ime.candidates()[i].consumed == syl.len();
        if ime.candidates().len() < 2 || !is_single_char(&ime, 0) || !is_single_char(&ime, 1) || !full(1) {
            continue;
        }
        let want = ime.candidates()[1].text.clone();
        fresh(&mut ime);
        checked += 1;
        let mut became = None;
        for n in 1..=50 {
            typ(&mut ime, &syl);
            if ime.view().items.first() == Some(&want) {
                became = Some(n);
                esc(&mut ime);
                break;
            }
            if !ime.candidates().iter().any(|c| c.text == want) {
                bad.push(format!("{syl}: 第 {n} 次找不到 {want}：{}", ime.view().items.join(" ")));
                break;
            }
            pick(&mut ime, &want);
        }
        if became.is_none() && !bad.last().is_some_and(|b| b.starts_with(&format!("{syl}:"))) {
            typ(&mut ime, &syl);
            bad.push(format!("{syl}: 选了 50 次 {want}，还是 {}", ime.view().items.join(" ")));
        }
    }
    esc(&mut ime);
    assert!(checked > 300, "只查了 {checked} 个音节");
    assert!(bad.is_empty(), "{} 个音节选几十次也换不过来：\n{}", bad.len(), bad.join("\n"));
}

/// 常用的双字词（词库分数最高的若干个），给下面的整类测试用：(全拼, 词, 音节)
fn common_two_char_words_syl(limit: usize) -> Vec<(String, String, Vec<String>)> {
    let Some(d) = common::real_dict() else { return Vec::new() };
    let mut v: Vec<(f32, String, String, Vec<String>)> = Vec::new();
    d.for_each_word(|code, w| {
        let t = d.word_text(w);
        if code.len() == 2 && t.chars().count() == 2 {
            let syl: Vec<String> = code.iter().map(|&c| d.syllable(c).to_string()).collect();
            v.push((d.word_score(w), syl.concat(), t.to_string(), syl));
        }
    });
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.1.clone())).take(limit).map(|x| (x.1, x.2, x.3)).collect()
}

fn common_two_char_words(limit: usize) -> Vec<(String, String)> {
    common_two_char_words_syl(limit).into_iter().map(|x| (x.0, x.1)).collect()
}

#[test]
fn real_word_beats_combination_of_single_chars() {
    // 打常用双字词的全拼，第一个不能是两个单字拼出来的组合（kanpan 不能是"坎潘"，zhangting 不能是"张廷"）
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut bad = Vec::new();
    let words = common_two_char_words(2000);
    assert!(words.len() > 1000);
    for (py, _) in &words {
        fresh(&mut ime);
        typ(&mut ime, py);
        if let Some(c) = ime.candidates().first() {
            if c.kind == pinyin_engine::CandKind::Sentence && c.text.chars().count() == 2 {
                bad.push(format!("{py}: {}", ime.view().items.join(" ")));
            }
        }
    }
    esc(&mut ime);
    assert!(bad.is_empty(), "{} 个：\n{}", bad.len(), bad.join("\n"));
}

/// 常见的"前一个词"
const PREVS: [(&str, &str); 14] = [
    ("wo", "我"),
    ("ni", "你"),
    ("yao", "要"),
    ("shi", "是"),
    ("de", "的"),
    ("le", "了"),
    ("ta", "他"),
    ("zai", "在"),
    ("xiang", "想"),
    ("gei", "给"),
    ("hao", "好"),
    ("ba", "吧"),
    ("qu", "去"),
    ("kan", "看"),
];

#[test]
fn context_only_nudges() {
    // 上文只能微调：有上文时排第一的，必须本来就在前 15 个（大约两排候选条）里——
    // 好+烦、你+疯 这种前后关联可以把稍后面的提上来，但不相干的字词不能从很后面突然跳到第一
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let mut inputs: Vec<String> = all_syllables();
    inputs.extend(common_two_char_words(300).into_iter().map(|x| x.0));
    inputs.extend(["l", "d", "zuoyige", "zheye", "quanzhong", "kanpan", "made", "xijie"].map(String::from));
    let mut bad = Vec::new();
    let mut checked = 0;
    for inp in &inputs {
        fresh(&mut ime);
        typ(&mut ime, inp);
        let top3: Vec<String> = ime.candidates().iter().take(15).map(|c| c.text.clone()).collect();
        if top3.is_empty() {
            continue;
        }
        for (py, w) in PREVS {
            fresh(&mut ime);
            typ(&mut ime, py);
            pick(&mut ime, w);
            typ(&mut ime, inp);
            let first = ime.view().items.first().cloned().unwrap_or_default();
            checked += 1;
            if !top3.contains(&first) {
                bad.push(format!("{w} + {inp}: {first}（没有上文时前 15 个：{}）", top3.join(" ")));
            }
        }
    }
    esc(&mut ime);
    let rate = bad.len() as f64 / checked as f64;
    eprintln!("上文稳定：查了 {checked} 次，{} 次超出前 15", bad.len());
    assert!(
        rate < 0.001,
        "{} / {checked} 次：\n{}",
        bad.len(),
        bad.iter().take(60).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn picking_the_exact_spelling_beats_a_correction() {
    // 常用词按错一个旁边的键，按错后恰好还是能用的拼音（zhege 打成 zheye）：
    // 纠错的（这个）排第一时你选了原样拼写的（这也），下次原样拼写的排第一（顺位能被替换）
    let Some(d) = common::real_dict() else { return };
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    const ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
    let neighbors = |c: char| -> Vec<char> {
        for row in ROWS {
            if let Some(i) = row.find(c) {
                let b = row.as_bytes();
                return [i.checked_sub(1), Some(i + 1)]
                    .into_iter()
                    .flatten()
                    .filter_map(|j| b.get(j).map(|&x| x as char))
                    .collect();
            }
        }
        Vec::new()
    };
    let parses = |py: &str| -> bool {
        // 能整个切成完整音节
        let n = py.len();
        let mut ok = vec![false; n + 1];
        ok[0] = true;
        for i in 0..n {
            if ok[i] {
                for l in 1..=6.min(n - i) {
                    if d.syllable_id(&py[i..i + l]).is_some() {
                        ok[i + l] = true;
                    }
                }
            }
        }
        ok[n]
    };
    let mut checked = 0;
    let mut bad = Vec::new();
    for (py, _) in common_two_char_words(600) {
        for (k, c) in py.char_indices() {
            for nb in neighbors(c) {
                let typed = format!("{}{}{}", &py[..k], nb, &py[k + 1..]);
                if !parses(&typed) {
                    continue;
                }
                fresh(&mut ime);
                typ(&mut ime, &typed);
                let cands = ime.candidates();
                if cands.first().is_none_or(|c| !c.corrected()) {
                    esc(&mut ime);
                    continue;
                }
                let Some(exact) =
                    cands.iter().find(|c| !c.corrected() && c.consumed == typed.len()).map(|c| c.text.clone())
                else {
                    esc(&mut ime);
                    continue;
                };
                checked += 1;
                pick(&mut ime, &exact);
                fresh(&mut ime);
                typ(&mut ime, &typed);
                if ime.candidates().first().is_some_and(|c| c.corrected()) {
                    bad.push(format!("{typed}: 选了 {exact}，第一个还是纠错的：{}", ime.view().items.join(" ")));
                }
            }
        }
    }
    esc(&mut ime);
    eprintln!("纠错让位：查了 {checked} 次");
    assert!(checked > 20, "只查了 {checked} 次");
    assert!(bad.is_empty(), "{} 个：\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn front_back_nasal_mixups_stay_visible() {
    // 前后鼻音打混（禁止 打成 jingzhi）：常用词要在候选条里能看到，但不抢第一
    let Some(d) = common::real_dict() else { return };
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let swap = |s: &str| -> Option<String> {
        let alt = if let Some(x) = s.strip_suffix("ng") {
            format!("{x}n")
        } else if s.ends_with('n') {
            format!("{s}g")
        } else {
            return None;
        };
        d.syllable_id(&alt).map(|_| alt)
    };
    let mut checked = 0;
    let mut bad = Vec::new();
    let mut stolen = Vec::new();
    for (py, w, sy) in common_two_char_words_syl(400) {
        // 把第一个能换的鼻音换掉
        let Some(i) = sy.iter().position(|s| swap(s).is_some()) else { continue };
        let mut typed = sy.clone();
        typed[i] = swap(&sy[i]).unwrap();
        let typed = typed.concat();
        fresh(&mut ime);
        typ(&mut ime, &typed);
        checked += 1;
        let row = ime.view().items;
        let pos = row.iter().position(|t| *t == w);
        if pos.is_none() && rank_of(&ime, &w).is_none_or(|r| r > 8) {
            bad.push(format!("{w}（{py}）打成 {typed}：{}", row.join(" ")));
        }
        let exact_word = ime.candidates().iter().any(|c| {
            c.kind == pinyin_engine::CandKind::Word
                && c.exact
                && !c.poor
                && c.consumed == typed.len()
                && c.text.chars().count() >= 2
        });
        if pos == Some(0) && ime.candidates()[0].corrected() && exact_word {
            stolen.push(format!("{w}（{py}）打成 {typed}：第一个就是它，原样拼写的被抢了"));
        }
    }
    esc(&mut ime);
    eprintln!("前后鼻音：查了 {checked} 个");
    assert!(checked > 100, "只查了 {checked} 个");
    assert!(bad.len() * 20 <= checked, "{} / {checked} 个看不到：\n{}", bad.len(), bad.join("\n"));
    assert!(stolen.is_empty(), "{}", stolen.join("\n"));
}

#[test]
fn added_words_come_second_then_learn_normally() {
    // 设置页加入的词：用全拼打排第二个；选几次以后排第一
    let Some(mut ime) = common::real_ime(UserDb::in_memory()) else { return };
    let r = ime.add_words("卷王，张子涵 螺蛳粉；abc 好(hao)\n曾经(zeng jing)");
    assert_eq!(r.added, 4, "{r:?}");
    assert_eq!(r.skipped.len(), 2, "{r:?}");
    assert!(r.skipped.iter().any(|x| x.starts_with("abc")));
    assert!(r.skipped.iter().any(|x| x.starts_with("好")));
    for (py, w) in [("juanwang", "卷王"), ("zhangzihan", "张子涵"), ("luosifen", "螺蛳粉"), ("zengjing", "曾经")]
    {
        fresh(&mut ime);
        typ(&mut ime, py);
        let r = rank_of(&ime, w);
        // 本来就是第一的（螺蛳粉、曾经）保持第一，其余的排第二
        assert!(r.is_some_and(|r| r <= 2), "{py} → {w} 排第 {r:?}：{:?}", ime.view().items);
    }
    // 自己写了拼音的：按你写的拼音加（曾经 写成 zeng jing，打 zengjing 也能出来）
    fresh(&mut ime);
    typ(&mut ime, "zengjing");
    assert!(rank_of(&ime, "曾经").is_some_and(|r| r <= 2), "{:?}", ime.view().items);
    // 人名选一次就排第一
    fresh(&mut ime);
    typ(&mut ime, "zhangzihan");
    pick(&mut ime, "张子涵");
    fresh(&mut ime);
    typ(&mut ime, "zhangzihan");
    assert_eq!(ime.view().items[0], "张子涵");
    // 再加一次：说已经加过了
    let r = ime.add_words("卷王");
    assert_eq!(r.added, 0);
    esc(&mut ime);
}
