//! 引擎进程（pinyin-engine.exe）：登录时启动，在后台等各个程序里的输入法 DLL 连进来。
//!
//! - 词库、语言模型、英文词表在安装目录的 data 文件夹里（只读）。
//! - 你的记忆、设置在 %APPDATA%\PinyinIME 里（只有你能读写）。
//! - 完全不联网。没有窗口、不弹任何东西。
//! - 每个连接（每个程序）一个线程、一个 Session；所有连接共用一个引擎，用锁排队。
//! - 设置文件改了（设置页保存时），两秒内自动重新读取。
//!
//! 在 Mac / Linux 上也能跑（用本地套接字代替命名管道），方便测试。

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use pinyin_engine::{Dict, Engine, EnglishDict, Service, Session, UserDb};
use pinyin_ipc::{read_frame, write_frame, Reply, Request};

/// 日志最大字节数（超过就从头写），日志里不记任何打字内容。
const LOG_MAX: u64 = 256 * 1024;

fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PINYIN_DATA") {
        return PathBuf::from(p);
    }
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("data"))).unwrap_or_else(|| "data".into())
}

fn log(dir: &Path, msg: &str) {
    let p = dir.join("engine.log");
    let append = std::fs::metadata(&p).map(|m| m.len() < LOG_MAX).unwrap_or(true);
    let f = std::fs::OpenOptions::new().create(true).append(append).write(true).truncate(!append).open(&p);
    if let Ok(mut f) = f {
        let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

fn load(data: &Path, user: &Path) -> Result<Service, String> {
    let dict = Dict::open(&data.join("pinyin.dict")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(user).map_err(|e| format!("建不了 {}：{e}", user.display()))?;
    let db = UserDb::open(&user.join("user.db"), &dict)?;
    let mut engine = Engine::new(dict, db);
    if let Ok(en) = EnglishDict::load(&data.join("english.tsv")) {
        engine.set_english(en);
    }
    Ok(Service::new(engine, data.to_path_buf(), user.join("settings.ini")))
}

/// 处理一个连接：读请求、交给引擎、回复，直到对方断开。
fn serve<S: Read + Write>(mut conn: S, svc: Arc<Mutex<Service>>) {
    let mut session = Session::new();
    while let Ok(frame) = read_frame(&mut conn) {
        let reply = match Request::decode(&frame) {
            Ok(req) => {
                let mut g = svc.lock().unwrap_or_else(|e| e.into_inner());
                g.handle(&mut session, req)
            }
            Err(e) => Reply::Error(e.to_string()),
        };
        if write_frame(&mut conn, &reply.encode()).is_err() {
            break;
        }
    }
}

/// 设置文件改了就重新读。
fn watch_settings(svc: Arc<Mutex<Service>>, path: PathBuf) {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut last = mtime(&path);
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let now = mtime(&path);
        if now != last {
            last = now;
            svc.lock().unwrap_or_else(|e| e.into_inner()).reload_settings();
        }
    }
}

fn main() {
    let user = pinyin_ipc::user_dir();
    let data = data_dir();
    let svc = match load(&data, &user) {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            log(&user, &format!("启动失败：{e}"));
            std::process::exit(1);
        }
    };
    log(&user, "引擎已启动");
    {
        let svc = svc.clone();
        let path = user.join("settings.ini");
        std::thread::spawn(move || watch_settings(svc, path));
    }
    if let Err(e) = listen(svc) {
        log(&user, &format!("监听失败：{e}"));
        std::process::exit(2);
    }
}

#[cfg(windows)]
fn listen(svc: Arc<Mutex<Service>>) -> std::io::Result<()> {
    // 第一次建管道用"第一个实例"：已经有一个引擎在跑（或者名字被别人占了），这里就退出
    let mut server = pinyin_winpipe::PipeServer::new()?;
    loop {
        match server.accept() {
            Ok(conn) => {
                let svc = svc.clone();
                std::thread::spawn(move || serve(conn, svc));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn listen(svc: Arc<Mutex<Service>>) -> std::io::Result<()> {
    let path = std::env::var("PINYIN_SOCKET").unwrap_or_else(|_| "/tmp/pinyin-ime.sock".into());
    let _ = std::fs::remove_file(&path);
    let l = std::os::unix::net::UnixListener::bind(&path)?;
    for conn in l.incoming() {
        let conn = conn?;
        let svc = svc.clone();
        std::thread::spawn(move || serve(conn, svc));
    }
    Ok(())
}
