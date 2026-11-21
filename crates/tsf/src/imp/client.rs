//! 和引擎进程的连接。
//!
//! - 连不上（引擎还没启动、正在启动）：所有按键原样放过，过 2 秒再试；必要时帮忙启动一次引擎。
//! - 每次请求最多等 0.3 秒；超时或出错就断开，按键原样放过。绝不拖住程序。
//! - 版本不一致：不用引擎。

use std::time::{Duration, Instant};

use pinyin_ipc::{call, Reply, Request, PROTO};
use pinyin_winpipe::PipeConn;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken, CREATE_NO_WINDOW, DETACHED_PROCESS,
    PROCESS_INFORMATION, STARTUPINFOW,
};

use super::register::dll_path;

const TIMEOUT_MS: u32 = 300;
const RETRY: Duration = Duration::from_secs(2);
const RELAUNCH: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct EngineClient {
    conn: Option<PipeConn>,
    next_try: Option<Instant>,
    launched: Option<Instant>,
}

impl EngineClient {
    /// 发一个请求。连不上、超时、出错都返回 None（调用方把按键原样放过）。
    pub fn call(&mut self, req: &Request) -> Option<Reply> {
        if self.conn.is_none() {
            self.try_connect();
        }
        let c = self.conn.as_mut()?;
        match call(c, req) {
            Ok(r) => Some(r),
            Err(_) => {
                self.conn = None;
                self.next_try = Some(Instant::now() + RETRY);
                None
            }
        }
    }

    pub fn connected(&self) -> bool {
        self.conn.is_some()
    }

    pub fn disconnect(&mut self) {
        self.conn = None;
    }

    fn try_connect(&mut self) {
        if self.next_try.is_some_and(|t| Instant::now() < t) {
            return;
        }
        self.next_try = Some(Instant::now() + RETRY);
        match pinyin_winpipe::connect(TIMEOUT_MS) {
            Ok(mut c) => {
                let pid = unsafe { GetCurrentProcessId() };
                if let Ok(Reply::Hello { proto }) = call(&mut c, &Request::Hello { proto: PROTO, pid }) {
                    if proto == PROTO {
                        self.conn = Some(c);
                    }
                }
            }
            Err(_) => self.launch_engine(),
        }
    }

    /// 是不是普通权限的程序。管理员权限的程序、商店应用沙箱、低权限进程都不去启动引擎：
    /// 它们启动的引擎权限不对，别的程序连不上（引擎平时在登录时就自动启动了）。
    fn normal_process() -> bool {
        unsafe {
            let mut tok = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() {
                return false;
            }
            // u64 数组：保证按指针对齐
            let mut buf = [0u64; 16];
            let mut len = 0u32;
            let size = std::mem::size_of_val(&buf) as u32;
            let ok =
                GetTokenInformation(tok, TokenIntegrityLevel, Some(buf.as_mut_ptr() as *mut _), size, &mut len).is_ok();
            let _ = CloseHandle(tok);
            if !ok {
                return false;
            }
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let sid = label.Label.Sid;
            let n = *GetSidSubAuthorityCount(sid) as u32;
            if n == 0 {
                return false;
            }
            let rid = *GetSidSubAuthority(sid, n - 1);
            // 中等（普通程序）≤ rid < 高（管理员）
            (0x2000..0x3000).contains(&rid)
        }
    }

    /// 引擎没在运行：从安装目录启动一次（商店应用沙箱里启动不了，没关系，登录时会自动启动）。
    fn launch_engine(&mut self) {
        if self.launched.is_some_and(|t| t.elapsed() < RELAUNCH) {
            return;
        }
        self.launched = Some(Instant::now());
        if !Self::normal_process() {
            return;
        }
        let dll = dll_path();
        let Some(dir) = std::path::Path::new(&dll).parent() else { return };
        let exe = dir.join("pinyin-engine.exe");
        if !exe.exists() {
            return;
        }
        let mut cmd: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        unsafe {
            if CreateProcessW(
                &HSTRING::from(exe.as_os_str()),
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                DETACHED_PROCESS | CREATE_NO_WINDOW,
                None,
                None,
                &si,
                &mut pi,
            )
            .is_ok()
            {
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
            }
        }
    }
}
