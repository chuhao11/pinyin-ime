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
use pinyin_engine::{Dict, Ime, Key, Mode, Mods, UserDb, GRID_COLS};

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

fn query(inputs: &[String]) -> Result<(), String> {
    let dict = open_dict()?;
    let user = UserDb::in_memory();
    for input in inputs {
        let t = Instant::now();
        let cands = pinyin_engine::lattice::analyze(&dict, &user, input, 0);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("== {input}（{} 个候选，{ms:.2} 毫秒）", cands.len());
        for (i, c) in cands.iter().take(30).enumerate() {
            let code: Vec<&str> = c.code.iter().map(|&s| dict.syllable(s)).collect();
            println!(
                "{:>3}. {}  {:.2}  {:?}  覆盖 {}  [{}]{}",
                i + 1,
                c.text,
                c.score,
                c.kind,
                c.consumed,
                code.join(" "),
                if c.poor { "  (差)" } else { "" }
            );
        }
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
    let mut ime = Ime::new(dict, UserDb::in_memory());
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
    let mut ime = Ime::new(dict, user);
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
    put_line(&mut o, &mut line, "数字选字 · 空格上屏 · - 打开/收起 5×5 矩阵 · 方向键移动 · 回车上屏字母 · Esc 清空")?;
    put_line(&mut o, &mut line, "")?;
    put_line(&mut o, &mut line, "打出的文字：")?;
    for l in typed.split('\n') {
        put_line(&mut o, &mut line, &format!("  {l}"))?;
    }
    put_line(&mut o, &mut line, "")?;
    let v = ime.view();
    if v.composing {
        put_line(&mut o, &mut line, &format!("输入：{}", v.preedit))?;
        match v.mode {
            Mode::Row => {
                queue!(o, cursor::MoveTo(0, line), Print("候选："))?;
                for (i, t) in v.items.iter().enumerate() {
                    let s = format!("{}.{} ", i + 1, t);
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
                put_line(&mut o, &mut line, &format!("矩阵：第 {}/{} 批（共 {} 个）", v.page + 1, v.pages, v.total))?;
                for (r, row) in v.items.chunks(GRID_COLS).enumerate() {
                    queue!(o, cursor::MoveTo(2, line))?;
                    for (c, t) in row.iter().enumerate() {
                        let idx = r * GRID_COLS + c;
                        let s = format!("{}.{}", c + 1, t);
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
