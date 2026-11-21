use std::io;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HLOCAL, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const BUF: u32 = 64 * 1024;

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
        let (sid, _) = user_sid()?;
        // 当前用户、系统：全部权限；商店应用沙箱：读写；低完整性：沙箱里的程序也能连
        let sddl = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GRGW;;;AC)(A;;GRGW;;;S-1-15-2-2)S:(ML;;NW;;;LW)");
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
        Ok(PipeServer { name: HSTRING::from(pipe_name()?.as_str()), sd, first: true })
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

/// DLL 这边：连上引擎。连上后核对管道的所有者是不是当前用户。
pub fn connect(timeout_ms: u32) -> io::Result<PipeConn> {
    let (_, my_sid) = user_sid()?;
    let name = HSTRING::from(pipe_name()?.as_str());
    let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    let h = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
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
    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()).map_err(win_err)? };
    let conn = PipeConn { h, timeout_ms: timeout_ms.max(1), event, server: false };
    if !ok {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "管道不是本人创建的，不连"));
    }
    Ok(conn)
}
