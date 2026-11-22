//! 诊断日志，默认关闭。
//!
//! 打开：在 %APPDATA%\PinyinIME 里放一个名叫 `debug` 的空文件（不用重启，两秒内生效）；删掉就关。
//! **放进去 24 小时后自动失效**（免得忘了关）；失效或关掉一天以后，旧日志自动删掉。
//! 写到同一个文件夹的 tsf-debug.log，最多 256 KB。
//! 只记 Shift、中英切换、连接引擎、为什么这里没打中文，以及空格、回车、方向键这类不会打出字的键交给了谁；
//! **绝不记打的字**；在密码框里连退格、回车这些键也不记。

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const MAX: u64 = 256 * 1024;

/// 打开以后管多久
const ON_FOR: Duration = Duration::from_secs(24 * 3600);

static CHECKED_AT: AtomicU64 = AtomicU64::new(0);
static ON: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 文件上次改动到现在多久了（时间不对、读不到都当作"刚刚"）。
fn age(p: &Path) -> Option<Duration> {
    let m = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    Some(SystemTime::now().duration_since(m).unwrap_or_default())
}

/// 日志开着没有。每两秒最多看一次文件，按键时不会每次都去读磁盘。
pub fn enabled() -> bool {
    let now = now_ms();
    if now.saturating_sub(CHECKED_AT.load(Ordering::Relaxed)) >= 2000 {
        CHECKED_AT.store(now, Ordering::Relaxed);
        let dir = pinyin_ipc::user_dir();
        let on = age(&dir.join("debug")).is_some_and(|a| a < ON_FOR);
        if !on {
            // 关着：旧日志放了一天以上就删掉
            let log = dir.join("tsf-debug.log");
            if age(&log).is_some_and(|a| a >= ON_FOR) {
                let _ = std::fs::remove_file(log);
            }
        }
        ON.store(on, Ordering::Relaxed);
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
