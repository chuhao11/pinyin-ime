//! 管道的安全性（只在 Windows 上跑）：
//! - 冒充引擎的程序连不上：管道另一头不是指定的引擎程序就不连
//! - 商店应用沙箱里的程序能用输入法（连得上、能核对引擎身份），但开不了"分身"冒充引擎
//! - 同时只能有一个引擎
#![cfg(windows)]

use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pinyin_winpipe::{connect_named, PipeConn, PipeServer};

fn unique(tag: &str) -> String {
    format!(r"\\.\pipe\PinyinIME-test-{tag}-{}", std::process::id())
}

/// 起一个"引擎"：接一个连接，把收到的 4 个字节原样发回去。
fn echo_server(name: &str) -> std::thread::JoinHandle<()> {
    let mut server = PipeServer::named(name).unwrap();
    std::thread::spawn(move || {
        if let Ok(mut c) = server.accept() {
            let mut b = [0u8; 4];
            if c.read_exact(&mut b).is_ok() {
                let _ = c.write_all(&b);
            }
            // 等对方断开
            let _ = c.read(&mut b);
        }
    })
}

/// 连上（管道可能还没建好：等一会儿）。身份核对不过立刻返回。
fn connect_retry(name: &str, engine: &Path) -> io::Result<PipeConn> {
    let start = Instant::now();
    loop {
        match connect_named(name, 5000, engine) {
            Err(e) if e.kind() != io::ErrorKind::PermissionDenied && start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            r => return r,
        }
    }
}

fn ping(c: &mut PipeConn) -> bool {
    let mut b = [0u8; 4];
    c.write_all(b"ping").is_ok() && c.read_exact(&mut b).is_ok() && &b == b"ping"
}

#[test]
fn connects_to_the_real_engine() {
    let name = unique("real");
    let t = echo_server(&name);
    let me = std::env::current_exe().unwrap();
    let mut c = connect_retry(&name, &me).unwrap();
    assert!(ping(&mut c));
    drop(c);
    t.join().unwrap();
}

#[test]
fn same_program_written_differently_still_counts() {
    let name = unique("case");
    let t = echo_server(&name);
    // 路径大小写不同：还是同一个程序
    let me = PathBuf::from(std::env::current_exe().unwrap().to_string_lossy().to_uppercase());
    let mut c = connect_retry(&name, &me).unwrap();
    assert!(ping(&mut c));
    drop(c);
    t.join().unwrap();
}

#[test]
fn refuses_a_pipe_run_by_some_other_program() {
    let name = unique("impostor");
    let t = echo_server(&name);
    // 说好了引擎是记事本：管道另一头是这个测试程序，不是记事本，不连
    let notepad = PathBuf::from(std::env::var("WINDIR").unwrap()).join("notepad.exe");
    let e = connect_retry(&name, &notepad).err().expect("冒充的管道居然连上了");
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    t.join().unwrap();
}

#[test]
fn only_one_engine_at_a_time() {
    // 测试时没有真的引擎在运行
    assert!(!pinyin_winpipe::engine_running(), "测试前就有引擎在运行");
    let first = pinyin_winpipe::claim_engine().unwrap().expect("第一个引擎应该占得到位置");
    assert!(pinyin_winpipe::engine_running());
    // 开机时好几个程序同时帮忙启动引擎：后来的那些马上退出
    assert!(pinyin_winpipe::claim_engine().unwrap().is_none(), "第二个引擎应该退出");
    drop(first);
    assert!(!pinyin_winpipe::engine_running());
}

// ---------------- 商店应用沙箱 ----------------

/// 探测程序的结果
fn explain(code: u32) -> &'static str {
    match code {
        0 => "全部正常",
        10 => "沙箱里连不上管道（权限给少了，商店应用里会打不了中文）",
        11 => "沙箱里核对不了引擎身份（商店应用里会打不了中文）",
        12 => "沙箱里收发数据失败",
        14 => "沙箱里能给管道开分身（能冒充引擎，危险）",
        15 => "探测程序没在沙箱里运行（测试本身有问题）",
        _ => "探测程序异常退出",
    }
}

#[test]
#[ignore = "要建一个商店应用沙箱：CI 里专门跑"]
fn sandboxed_app_can_use_but_not_impersonate() {
    pinyin_winpipe::allow_sandboxed_lookup().unwrap();
    let name = unique("sandbox");
    let t = echo_server(&name);
    let me = std::env::current_exe().unwrap();
    // 把测试程序复制到一个沙箱读得到的地方，在沙箱里运行它的 sandbox_probe
    let dir = std::env::temp_dir().join(format!("pyime-sandbox-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let probe = dir.join("probe.exe");
    std::fs::copy(&me, &probe).unwrap();
    for p in [&dir, &probe] {
        let grant = if p == &dir { "*S-1-15-2-1:(OI)(CI)(RX)" } else { "*S-1-15-2-1:(RX)" };
        let ok = std::process::Command::new("icacls").arg(p).args(["/grant", grant]).status().unwrap().success();
        assert!(ok, "icacls 失败");
    }
    let env = [("PINYIN_PROBE_PIPE", name.clone()), ("PINYIN_PROBE_ENGINE", me.to_string_lossy().into_owned())];
    let r = appcontainer::run(&probe, &dir, &env);
    let _ = std::fs::remove_dir_all(&dir);
    let code = r.unwrap();
    assert_eq!(code, 0, "沙箱里的探测程序：{}（{code}）", explain(code));
    t.join().unwrap();
}

/// 在沙箱里由上面的测试启动；平时直接返回。
#[test]
#[ignore = "只在沙箱里由 sandboxed_app_can_use_but_not_impersonate 启动"]
fn sandbox_probe() {
    let Ok(name) = std::env::var("PINYIN_PROBE_PIPE") else { return };
    let engine = PathBuf::from(std::env::var("PINYIN_PROBE_ENGINE").unwrap());
    std::process::exit(probe(&name, &engine));
}

fn probe(name: &str, engine: &Path) -> i32 {
    if !appcontainer::inside() {
        return 15;
    }
    // 1. 连得上，核对得了引擎身份，收发正常
    let mut c = match connect_retry(name, engine) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return 11,
        Err(_) => return 10,
    };
    if !ping(&mut c) {
        return 12;
    }
    // 2. 开不了分身
    if appcontainer::can_create_instance(name) {
        return 14;
    }
    0
}

mod appcontainer {
    use super::*;
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Security::Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile};
    use windows::Win32::Security::{
        FreeSid, GetTokenInformation, TokenIsAppContainer, SECURITY_CAPABILITIES, TOKEN_QUERY,
    };
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, OpenProcessToken, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
    };

    /// 这个程序是不是在商店应用沙箱里运行。
    pub fn inside() -> bool {
        unsafe {
            let mut tok = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() {
                return false;
            }
            let mut v = 0u32;
            let mut len = 0u32;
            let ok = GetTokenInformation(
                tok,
                TokenIsAppContainer,
                Some(&mut v as *mut u32 as *mut _),
                std::mem::size_of::<u32>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(tok);
            ok && v != 0
        }
    }

    /// 能不能给这个管道再开一个实例（能的话就能冒充引擎）。
    pub fn can_create_instance(name: &str) -> bool {
        unsafe {
            let h = CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                None,
            );
            if h.is_invalid() {
                return false;
            }
            let _ = CloseHandle(h);
            true
        }
    }

    /// 在一个新建的商店应用沙箱（AppContainer，没有任何额外能力）里运行 `exe` 的 sandbox_probe，返回退出码。
    pub fn run(exe: &Path, dir: &Path, env: &[(&str, String)]) -> Result<u32, String> {
        unsafe {
            let name = HSTRING::from(format!("PinyinIME.SandboxTest.{}", std::process::id()));
            let _ = DeleteAppContainerProfile(&name);
            let sid = CreateAppContainerProfile(&name, &name, &name, None).map_err(|e| format!("建沙箱失败：{e}"))?;
            let caps = SECURITY_CAPABILITIES { AppContainerSid: sid, ..Default::default() };
            let mut size = 0usize;
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
            let mut buf = vec![0u8; size.max(64)];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
            let result = (|| {
                InitializeProcThreadAttributeList(Some(list), 1, None, &mut size).map_err(|e| e.to_string())?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some(&caps as *const SECURITY_CAPABILITIES as *const _),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;
                let mut si = STARTUPINFOEXW::default();
                si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
                si.lpAttributeList = list;
                let mut cmd: Vec<u16> =
                    format!("\"{}\" sandbox_probe --exact --ignored --nocapture --test-threads=1", exe.display())
                        .encode_utf16()
                        .chain([0])
                        .collect();
                // 环境变量：照抄这个进程的，再加上探测要用的
                let mut block: Vec<u16> = Vec::new();
                let mut add = |k: &std::ffi::OsStr, v: &std::ffi::OsStr| {
                    block.extend(k.encode_wide());
                    block.push('=' as u16);
                    block.extend(v.encode_wide());
                    block.push(0);
                };
                for (k, v) in std::env::vars_os() {
                    add(&k, &v);
                }
                for (k, v) in env {
                    add(std::ffi::OsStr::new(k), std::ffi::OsStr::new(v));
                }
                block.push(0);
                let mut pi = PROCESS_INFORMATION::default();
                CreateProcessW(
                    &HSTRING::from(exe.as_os_str()),
                    Some(PWSTR(cmd.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                    Some(block.as_ptr().cast()),
                    &HSTRING::from(dir.as_os_str()),
                    &si.StartupInfo,
                    &mut pi,
                )
                .map_err(|e| format!("在沙箱里启动失败：{e}"))?;
                let done = WaitForSingleObject(pi.hProcess, 60_000) == WAIT_OBJECT_0;
                if !done {
                    let _ = TerminateProcess(pi.hProcess, 99);
                }
                let mut code = 0u32;
                let _ = GetExitCodeProcess(pi.hProcess, &mut code);
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
                if done {
                    Ok(code)
                } else {
                    Err("沙箱里的探测程序一分钟没结束".to_string())
                }
            })();
            DeleteProcThreadAttributeList(list);
            FreeSid(sid);
            let _ = DeleteAppContainerProfile(&name);
            result
        }
    }
}
