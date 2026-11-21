//! 在终端里试打输入法。
//!
//! - `pinyin-try`：交互试打（像输入法一样，按 Ctrl+C 退出）
//! - `pinyin-try query xian`：列出候选和分数
//! - `pinyin-try keys "zhangzihan{2}..."`：模拟一串按键，打印上屏结果（自动测试用）
//!
//! 词库默认找 data/build/pinyin.dict，可以用环境变量 PINYIN_DICT 指定。

use std::io::{stdout, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, execute, queue, terminal};
use pinyin_engine::{Dict, EnglishDict, Ime, Key, Lm, Mode, Mods, Query, UserDb, GRID_COLS};

fn find_dict() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("PINYIN_DICT") {
        return Ok(p.into());
    }
    let mut cands = vec![PathBuf::from("data/build/pinyin.dict")];
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors().skip(1).take(5) {
            cands.push(anc.join("data/build/pinyin.dict"));
        }
    }
    cands
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| "找不到词库 data/build/pinyin.dict，请先运行：cargo run --release -p dictgen".to_string())
}

fn user_db_path() -> PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".pinyin-ime-try").join("user.db")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(|s| s.as_str()) {
        Some("query") => query(&args[1..]),
        Some("keys") => keys(&args[1..]),
        Some("uni") => uni(&args[1..]),
        Some("-h") | Some("--help") => {
            println!("用法：pinyin-try [query <拼音>... | keys <按键串>]");
            Ok(())
        }
        _ => interactive(),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("出错：{e}");
            ExitCode::FAILURE
        }
    }
}

fn open_dict() -> Result<Dict, String> {
    let p = find_dict()?;
    Dict::open(&p).map_err(|e| e.to_string())
}

/// 英文词表和词库放在同一个文件夹里（english.tsv），没有就不用英文。
fn open_english() -> Option<EnglishDict> {
    let p = find_dict().ok()?.with_file_name("english.tsv");
    EnglishDict::load(&p).ok()
}

/// 语言模型也在同一个文件夹（pinyin.lm）；没有就不用。
fn open_lm() -> Option<Lm> {
    let p = find_dict().ok()?.with_file_name("pinyin.lm");
    Lm::open(&p).ok()
}

fn new_ime(dict: Dict, user: UserDb) -> Ime {
    let mut ime = Ime::new(dict, user);
    if let Some(en) = open_english() {
        ime.set_english(en);
    }
    if std::env::var("PINYIN_NO_LM").is_err() {
        if let Some(lm) = open_lm() {
            ime.set_lm(lm);
        }
    }
    ime
}

fn query(inputs: &[String]) -> Result<(), String> {
    let dict = open_dict()?;
    let en = open_english();
    let lm = if std::env::var("PINYIN_NO_LM").is_ok() { None } else { open_lm() };
    let user = UserDb::in_memory();
    for input in inputs {
        let t = Instant::now();
        let q = Query { dict: &dict, en: en.as_ref(), lm: lm.as_ref(), user: &user, prev: None, now: 0 };
        let cands = pinyin_engine::analyze_with(&q, input);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("== {input}（{} 个候选，{ms:.2} 毫秒）", cands.len());
        for (i, c) in cands.iter().take(30).enumerate() {
            let code: Vec<&str> = c.code.iter().map(|&s| dict.syllable(s)).collect();
            println!(
                "{:>3}. {}  {:.2}  {:?}  覆盖 {}  [{}]{}{}",
                i + 1,
                c.text,
                c.score,
                c.kind,
                c.consumed,
                code.join(" "),
                if c.corrected() { "  (纠错)" } else { "" },
                if c.poor { "  (差)" } else { "" }
            );
        }
    }
    Ok(())
}

/// 调试：打印字词在词库里的分数和在语言模型里的单独概率。
fn uni(words: &[String]) -> Result<(), String> {
    let dict = open_dict()?;
    let lm = open_lm().ok_or("没有语言模型")?;
    let mut best: std::collections::HashMap<String, (f32, f32, String)> = std::collections::HashMap::new();
    dict.for_each_word(|code, w| {
        let t = dict.word_text(w);
        if words.iter().any(|x| x == t) {
            let py: Vec<&str> = code.iter().map(|&s| dict.syllable(s)).collect();
            let e = best.entry(format!("{t} {}", py.join(" "))).or_insert((0.0, 0.0, String::new()));
            *e = (dict.word_score(w), dict.word_reading(w), py.join(" "));
        }
    });
    let mut v: Vec<_> = best.into_iter().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, (sc, rd, _)) in v {
        let t = k.split(' ').next().unwrap_or("");
        let u = lm.id(t).map(|id| lm.uni(id)).unwrap_or(f32::NAN);
        println!("{k}\t词库 {sc:.2}\t读音比例 {rd:.2}\t模型单独 {u:.2}\t模型+读音 {:.2}", u + rd);
    }
    Ok(())
}

/// 把 "nihao{space}{2}{-}{down}{enter}" 这样的串拆成按键。
fn parse_keys(s: &str) -> Result<Vec<Key>, String> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '{' {
            let mut name = String::new();
            for d in it.by_ref() {
                if d == '}' {
                    break;
                }
                name.push(d);
            }
            out.push(match name.as_str() {
                "space" => Key::Space,
                "shift" => Key::ShiftTap,
                "enter" => Key::Enter,
                "bs" => Key::Backspace,
                "esc" => Key::Escape,
                "left" => Key::Left,
                "right" => Key::Right,
                "up" => Key::Up,
                "down" => Key::Down,
                n if n.chars().count() == 1 => Key::Char(n.chars().next().unwrap()),
                n => return Err(format!("不认识的按键 {{{n}}}")),
            });
        } else {
            out.push(Key::Char(c));
        }
    }
    Ok(out)
}

fn keys(args: &[String]) -> Result<(), String> {
    let dict = open_dict()?;
    let mut ime = new_ime(dict, UserDb::in_memory());
    let mut typed = String::new();
    for a in args {
        for k in parse_keys(a)? {
            let r = ime.process_key(k, Mods::default());
            apply(&mut typed, k, &r);
        }
    }
    println!("{typed}");
    let v = ime.view();
    if v.composing {
        println!("[输入中] {} → {}", v.preedit, v.items.join(" "));
    }
    Ok(())
}

/// 模拟程序收到按键/上屏文字后的样子。
fn apply(typed: &mut String, k: Key, r: &pinyin_engine::Response) {
    if let Some(t) = &r.commit {
        typed.push_str(t);
    }
    if !r.consumed {
        match k {
            Key::Char(c) => typed.push(c),
            Key::Space => typed.push(' '),
            Key::Enter => typed.push('\n'),
            Key::Backspace => {
                typed.pop();
            }
            _ => {}
        }
    }
}

struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), cursor::Show, terminal::LeaveAlternateScreen);
    }
}

fn interactive() -> Result<(), String> {
    let dict = open_dict()?;
    let up = user_db_path();
    if let Some(d) = up.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let user = UserDb::open(&up, &dict)?;
    let mut ime = new_ime(dict, user);
    let mut typed = String::new();

    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _guard = TermGuard;
    execute!(stdout(), terminal::EnterAlternateScreen, cursor::Hide).map_err(|e| e.to_string())?;
    let mut last_ms = 0.0;
    loop {
        draw(&ime, &typed, &up, last_ms).map_err(|e| e.to_string())?;
        let ev = event::read().map_err(|e| e.to_string())?;
        let Event::Key(ke) = ev else { continue };
        if ke.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = ke.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(ke.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            break;
        }
        if ctrl && ke.code == KeyCode::Char('l') {
            typed.clear();
            continue;
        }
        let key = match ke.code {
            // 终端收不到"单按 Shift"，用 Tab 代替
            KeyCode::Tab => Key::ShiftTap,
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Enter => Key::Enter,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Esc => Key::Escape,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            _ => Key::Other,
        };
        let mods = Mods { ctrl, alt: ke.modifiers.contains(KeyModifiers::ALT), caps_lock: false };
        let t = Instant::now();
        let r = ime.process_key(key, mods);
        last_ms = t.elapsed().as_secs_f64() * 1000.0;
        apply(&mut typed, key, &r);
    }
    Ok(())
}

fn draw(ime: &Ime, typed: &str, user_path: &Path, ms: f64) -> std::io::Result<()> {
    let mut o = stdout();
    queue!(o, terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
    let mut line = 0u16;
    fn put_line(o: &mut std::io::Stdout, line: &mut u16, s: &str) -> std::io::Result<()> {
        queue!(o, cursor::MoveTo(0, *line), Print(s))?;
        *line += 1;
        Ok(())
    }
    put_line(&mut o, &mut line, "拼音输入法 · 试打工具        Ctrl+C 退出    Ctrl+L 清空下面打出的文字")?;
    let mode = if ime.view().english_mode { "英（纯英文）" } else { "中" };
    let lm = if ime.lm().is_some() { "有语言模型" } else { "没有语言模型" };
    put_line(&mut o, &mut line, &format!("现在：{mode} · {lm}        Tab = 单按 Shift（中/英切换）"))?;
    put_line(
        &mut o,
        &mut line,
        "数字选字 · 空格上屏 · - 打开/收起 5×5 矩阵 · 方向键移动 · 回车上屏字母 · Esc 清空 · ~ 表示打错纠正 · 按两下 ` 删掉高亮的词",
    )?;
    put_line(&mut o, &mut line, "")?;
    put_line(&mut o, &mut line, "打出的文字：")?;
    for l in typed.split('\n') {
        put_line(&mut o, &mut line, &format!("  {l}"))?;
    }
    put_line(&mut o, &mut line, "")?;
    let v = ime.view();
    if v.composing {
        put_line(&mut o, &mut line, &format!("输入：{}", v.preedit))?;
        if v.armed {
            let w = v.items.get(v.highlight).cloned().unwrap_or_default();
            put_line(&mut o, &mut line, &format!("再按一次 ` 删除「{w}」，按别的键取消"))?;
        }
        match v.mode {
            Mode::Row => {
                queue!(o, cursor::MoveTo(0, line), Print("候选："))?;
                for (i, t) in v.items.iter().enumerate() {
                    let mark = if v.corrected[i] { "~" } else { "" };
                    let s = format!("{}.{}{} ", i + 1, t, mark);
                    if i == v.highlight {
                        queue!(o, SetAttribute(Attribute::Reverse), Print(&s), SetAttribute(Attribute::Reset))?;
                    } else {
                        queue!(o, Print(&s))?;
                    }
                    queue!(o, Print(" "))?;
                }
                line += 1;
                put_line(&mut o, &mut line, &format!("（共 {} 个候选，按 - 看更多；这次用了 {ms:.1} 毫秒）", v.total))?;
            }
            Mode::Grid => {
                put_line(
                    &mut o,
                    &mut line,
                    &format!(
                        "矩阵：第 {}/{} 批（共 {} 个）  高亮那一行可以按 1–5 直接选，空格或回车上屏高亮的",
                        v.page + 1,
                        v.pages,
                        v.total
                    ),
                )?;
                let hl_row = v.highlight / GRID_COLS;
                for (r, row) in v.items.chunks(GRID_COLS).enumerate() {
                    queue!(o, cursor::MoveTo(2, line))?;
                    for (c, t) in row.iter().enumerate() {
                        let idx = r * GRID_COLS + c;
                        let mark = if v.corrected[idx] { "~" } else { "" };
                        // 只有高亮的那一行标数字
                        let s = if r == hl_row { format!("{}.{}{}", c + 1, t, mark) } else { format!("  {t}{mark}") };
                        let pad = 14usize.saturating_sub(pinyin_engine::display_width(&s));
                        if idx == v.highlight {
                            queue!(o, SetAttribute(Attribute::Reverse), Print(&s), SetAttribute(Attribute::Reset))?;
                        } else {
                            queue!(o, Print(&s))?;
                        }
                        queue!(o, Print(" ".repeat(pad)))?;
                    }
                    line += 1;
                }
            }
        }
    } else {
        put_line(&mut o, &mut line, "（直接打拼音试试）")?;
    }
    put_line(&mut o, &mut line, "")?;
    put_line(&mut o, &mut line, &format!("记住的词：{} 个（存在 {}）", ime.user().len(), user_path.display()))?;
    o.flush()
}
