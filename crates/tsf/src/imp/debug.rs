//! 诊断日志，默认关闭。
//!
//! 打开：在 %APPDATA%\PinyinIME 里放一个名叫 `debug` 的空文件（不用重启，立刻生效）；删掉就关。
//! 写到同一个文件夹的 tsf-debug.log，最多 256 KB。
//! 只记 Shift、中英切换、连接引擎这类事件，**绝不记打的字**。

use std::io::Write;
use std::time::SystemTime;

const MAX: u64 = 256 * 1024;

pub fn log(msg: &str) {
    let dir = pinyin_ipc::user_dir();
    if !dir.join("debug").exists() {
        return;
    }
    let p = dir.join("tsf-debug.log");
    let append = std::fs::metadata(&p).map(|m| m.len() < MAX).unwrap_or(true);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(append).write(true).truncate(!append).open(&p)
    else {
        return;
    };
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let exe = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let _ = writeln!(f, "[{t}] {exe}({}) {msg}", std::process::id());
}
