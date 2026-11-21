//! 诊断日志，默认关闭。
//!
//! 打开：在 %APPDATA%\PinyinIME 里放一个名叫 `debug` 的空文件（不用重启，两秒内生效）；删掉就关。
//! 写到同一个文件夹的 tsf-debug.log，最多 256 KB。
//! 只记 Shift、中英切换、连接引擎，以及空格、回车、方向键这类不会打出字的键交给了谁；**绝不记打的字**。

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

const MAX: u64 = 256 * 1024;

static CHECKED_AT: AtomicU64 = AtomicU64::new(0);
static ON: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 日志开着没有。每两秒最多看一次文件，按键时不会每次都去读磁盘。
pub fn enabled() -> bool {
    let now = now_ms();
    if now.saturating_sub(CHECKED_AT.load(Ordering::Relaxed)) >= 2000 {
        CHECKED_AT.store(now, Ordering::Relaxed);
        ON.store(pinyin_ipc::user_dir().join("debug").exists(), Ordering::Relaxed);
    }
    ON.load(Ordering::Relaxed)
}

pub fn log(msg: &str) {
    if !enabled() {
        return;
    }
    let p = pinyin_ipc::user_dir().join("tsf-debug.log");
    let append = std::fs::metadata(&p).map(|m| m.len() < MAX).unwrap_or(true);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(append).write(true).truncate(!append).open(&p)
    else {
        return;
    };
    let exe = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let _ = writeln!(f, "[{}] {exe}({}) {msg}", now_ms(), std::process::id());
}
