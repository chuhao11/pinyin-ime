use std::io;
use std::path::Path;

use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
    ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    EqualSid, GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, GetFileInformationByHandle, ReadFile, WriteFile, BY_HANDLE_FILE_INFORMATION,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenMutexW, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const BUF: u32 = 64 * 1024;

/// 连接的一方能有的权限：读写数据、读属性，但**没有"再开一个管道实例"**
/// （FILE_CREATE_PIPE_INSTANCE，数值上和 FILE_APPEND_DATA 一样）。
/// = FILE_GENERIC_READ | FILE_GENERIC_WRITE 去掉 FILE_APPEND_DATA。
/// 商店应用沙箱只给这些：沙箱里的程序能用输入法，但开不了"分身"冒充引擎。
pub const CLIENT_ACCESS: u32 = 0x0012_019B;

fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// 当前用户的 SID（字符串）和原始字节。
fn user_sid() -> io::Result<(String, Vec<u8>)> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(win_err)?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len.max(64) as usize];
        let r = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), buf.len() as u32, &mut len);
        let _ = CloseHandle(token);
        r.map_err(win_err)?;
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(tu.User.Sid, &mut s).map_err(win_err)?;
        let text = s.to_string().unwrap_or_default();
        LocalFree(Some(HLOCAL(s.0.cast())));
        Ok((text, buf))
    }
}

/// 管道名：`\\.\pipe\PinyinIME-<当前用户 SID>`。
pub fn pipe_name() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\PinyinIME-{}", user_sid()?.0))
}

/// "引擎在运行"的标记（本次登录内、本用户）。
fn engine_mark_name() -> io::Result<String> {
    Ok(format!(r"Local\PinyinIME-engine-{}", user_sid()?.0))
}

/// 引擎占住"只能有一个引擎"的位置。拿着它直到退出。
pub struct EngineLock(HANDLE);

unsafe impl Send for EngineLock {}

impl Drop for EngineLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 引擎一启动就调用（在读词库之前）：已经有一个引擎在运行或正在启动，返回 None，这个应该马上退出。
/// 开机时好几个程序可能同时帮忙启动引擎，这样只有一个真正读词库，其余的立刻退出。
pub fn claim_engine() -> io::Result<Option<EngineLock>> {
    let name = HSTRING::from(engine_mark_name()?);
    unsafe {
        let h = CreateMutexW(None, false, &name).map_err(win_err)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return Ok(None);
        }
        Ok(Some(EngineLock(h)))
    }
}

/// 引擎是不是已经在运行（或者正在启动）。只看标记，不连管道，不打扰引擎。
pub fn engine_running() -> bool {
    let Ok(name) = engine_mark_name() else { return true };
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            // 只有"没有这个标记"才算没运行；别的错误（没权限之类）当作在运行，不乱启动
            Err(e) => e.code() != ERROR_FILE_NOT_FOUND.to_hresult(),
        }
    }
}

/// 一个连接（引擎那边是阻塞读写；DLL 那边带超时）。
pub struct PipeConn {
    h: HANDLE,
    /// DLL 这边：每次收发最多等多少毫秒（0 表示阻塞）
    timeout_ms: u32,
    event: HANDLE,
    server: bool,
}

unsafe impl Send for PipeConn {}

impl Drop for PipeConn {
    fn drop(&mut self) {
        unsafe {
            if self.server {
                let _ = FlushFileBuffers(self.h);
                let _ = DisconnectNamedPipe(self.h);
            }
            let _ = CloseHandle(self.h);
            if !self.event.is_invalid() && !self.event.0.is_null() {
                let _ = CloseHandle(self.event);
            }
        }
    }
}

impl PipeConn {
    fn overlapped_io(&mut self, write: bool, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
            let mut n = 0u32;
            let r = if write {
                WriteFile(self.h, Some(buf), None, Some(&mut ov))
            } else {
                ReadFile(self.h, Some(buf), None, Some(&mut ov))
            };
            if let Err(e) = r {
                if GetLastError() != ERROR_IO_PENDING {
                    return Err(win_err(e));
                }
                if WaitForSingleObject(self.event, self.timeout_ms) != WAIT_OBJECT_0 {
                    // 超时：取消这次读写，放弃这个连接
                    let _ = CancelIoEx(self.h, Some(&ov));
                    let _ = GetOverlappedResult(self.h, &ov, &mut n, true);
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "引擎没有及时回应"));
                }
            }
            GetOverlappedResult(self.h, &ov, &mut n, false).map_err(win_err)?;
            Ok(n as usize)
        }
    }
}

impl io::Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.timeout_ms > 0 {
            return self.overlapped_io(false, buf);
        }
        let mut n = 0u32;
        unsafe { ReadFile(self.h, Some(buf), Some(&mut n), None).map_err(win_err)? };
        Ok(n as usize)
    }
}

impl io::Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.timeout_ms > 0 {
            let mut copy = buf.to_vec();
            return self.overlapped_io(true, &mut copy);
        }
        let mut n = 0u32;
        unsafe { WriteFile(self.h, Some(buf), Some(&mut n), None).map_err(win_err)? };
        Ok(n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 引擎这边：监听管道。
pub struct PipeServer {
    name: HSTRING,
    sd: PSECURITY_DESCRIPTOR,
    first: bool,
}

unsafe impl Send for PipeServer {}

impl Drop for PipeServer {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

impl PipeServer {
    pub fn new() -> io::Result<PipeServer> {
        Self::named(&pipe_name()?)
    }

    /// 用指定的管道名（测试用）。
    pub fn named(name: &str) -> io::Result<PipeServer> {
        let (sid, _) = user_sid()?;
        // 当前用户、系统：全部权限；商店应用沙箱：只能读写数据，不能开新实例；低完整性：沙箱里的程序也能连
        let sddl = format!(
            "D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;{CLIENT_ACCESS:#x};;;AC)(A;;{CLIENT_ACCESS:#x};;;S-1-15-2-2)S:(ML;;NW;;;LW)"
        );
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl.as_str()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(win_err)?;
        }
        Ok(PipeServer { name: HSTRING::from(name), sd, first: true })
    }

    /// 等一个 DLL 连进来。
    pub fn accept(&mut self) -> io::Result<PipeConn> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: false.into(),
        };
        let mut mode = PIPE_ACCESS_DUPLEX;
        if self.first {
            mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let h = unsafe {
            CreateNamedPipeW(
                &self.name,
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUF,
                BUF,
                0,
                Some(&sa),
            )
        };
        if h.is_invalid() || h == INVALID_HANDLE_VALUE {
            // 名字被占用用 AddrInUse 表示：引擎据此判断"已经有一个在跑"，直接退出；其他错误重试就行
            return Err(if self.first {
                io::Error::new(io::ErrorKind::AddrInUse, "管道名已被占用（引擎已经在运行？）")
            } else {
                io::Error::other("建不了管道")
            });
        }
        self.first = false;
        if let Err(e) = unsafe { ConnectNamedPipe(h, None) } {
            if unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                unsafe {
                    let _ = CloseHandle(h);
                }
                return Err(win_err(e));
            }
        }
        Ok(PipeConn { h, timeout_ms: 0, event: HANDLE::default(), server: true })
    }
}

/// DLL 这边：连上引擎。`engine` 是安装目录里的 pinyin-engine.exe：
/// 管道另一头必须正是这个程序，而且管道的所有者是当前用户，否则不连（按键一个都不发）。
pub fn connect(timeout_ms: u32, engine: &Path) -> io::Result<PipeConn> {
    connect_named(&pipe_name()?, timeout_ms, engine)
}

/// 同 [`connect`]，用指定的管道名（测试用）。
pub fn connect_named(name: &str, timeout_ms: u32, engine: &Path) -> io::Result<PipeConn> {
    let (_, my_sid) = user_sid()?;
    let name = HSTRING::from(name);
    let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    let h = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            CLIENT_ACCESS,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(flags.0),
            None,
        )
        .map_err(win_err)?
    };
    // 管道的所有者必须是自己
    let ok = unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let r = GetSecurityInfo(
            h,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut sd),
        );
        let tu = &*(my_sid.as_ptr() as *const TOKEN_USER);
        let same = r.is_ok() && !owner.0.is_null() && EqualSid(owner, tu.User.Sid).is_ok();
        if !sd.0.is_null() {
            LocalFree(Some(HLOCAL(sd.0)));
        }
        same
    };
    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) };
    let event = match event {
        Ok(e) => e,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            return Err(win_err(e));
        }
    };
    let conn = PipeConn { h, timeout_ms: timeout_ms.max(1), event, server: false };
    if !ok {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "管道不是本人创建的，不连"));
    }
    // 同一个用户的别的程序也能建同名的管道冒充引擎：核对另一头真的是安装目录里的引擎
    if !server_is(h, engine) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "管道另一头不是输入法引擎，不连"));
    }
    Ok(conn)
}

/// 管道另一头（建这个管道实例的程序）是不是 `engine` 这个文件。
fn server_is(h: HANDLE, engine: &Path) -> bool {
    match server_image(h) {
        Some(image) => same_file(&image, engine),
        None => false,
    }
}

/// 管道另一头那个程序的完整路径。
fn server_image(h: HANDLE) -> Option<std::path::PathBuf> {
    unsafe {
        let mut pid = 0u32;
        GetNamedPipeServerProcessId(h, &mut pid).ok()?;
        if pid == 0 {
            return None;
        }
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 2048];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(p);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]).into())
    }
}

/// 两个路径是不是同一个文件：先比路径（不分大小写），不一样再比文件编号（同一个文件换了写法也认得出）。
fn same_file(a: &Path, b: &Path) -> bool {
    if a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase() {
        return true;
    }
    match (file_id(a), file_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 文件所在的卷和文件编号。
fn file_id(p: &Path) -> Option<(u32, u32, u32)> {
    unsafe {
        let h = CreateFileW(
            &HSTRING::from(p.as_os_str()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let r = GetFileInformationByHandle(h, &mut info);
        let _ = CloseHandle(h);
        r.ok()?;
        Some((info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow))
    }
}

/// 引擎启动时调用：让商店应用沙箱里的输入法也能查到"引擎是哪个程序"（只能查名字，别的什么都做不了），
/// 这样它们也能核对引擎的身份。没有这一步，沙箱里打不了中文。
pub fn allow_sandboxed_lookup() -> io::Result<()> {
    unsafe {
        let me = GetCurrentProcess();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        GetSecurityInfo(me, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, None, None, None, None, Some(&mut sd))
            .ok()
            .map_err(win_err)?;
        let mut s = PWSTR::null();
        let conv = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut s,
            None,
        );
        LocalFree(Some(HLOCAL(sd.0)));
        conv.map_err(win_err)?;
        let text = s.to_string().unwrap_or_default();
        LocalFree(Some(HLOCAL(s.0.cast())));
        // 没有访问控制（谁都能查）就不用加
        if !text.starts_with("D:") || text.contains("NO_ACCESS_CONTROL") {
            return Ok(());
        }
        // 0x1000 = PROCESS_QUERY_LIMITED_INFORMATION：只能查程序路径、是否还在运行
        let new = format!("{text}(A;;0x1000;;;AC)(A;;0x1000;;;S-1-15-2-2)");
        let mut nsd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(new), SDDL_REVISION_1, &mut nsd, None)
            .map_err(win_err)?;
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let r = match GetSecurityDescriptorDacl(nsd, &mut present, &mut dacl, &mut defaulted) {
            Ok(()) if present.as_bool() && !dacl.is_null() => {
                SetSecurityInfo(me, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(dacl), None)
                    .ok()
                    .map_err(win_err)
            }
            Ok(()) => Err(io::Error::other("权限表是空的")),
            Err(e) => Err(win_err(e)),
        };
        LocalFree(Some(HLOCAL(nsd.0)));
        r
    }
}
