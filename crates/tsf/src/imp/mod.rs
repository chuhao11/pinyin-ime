//! DLL 入口：注册、创建输入法对象。

mod candwin;
mod client;
mod edit;
mod factory;
mod keys;
mod register;
mod service;

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::core::{Interface, BOOL, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_POINTER, HINSTANCE, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

/// 输入法的 COM 类编号（固定不变，注册表里用）。
pub const CLSID_PINYIN: GUID = GUID::from_u128(pinyin_types::TIP_CLSID);
/// 输入法在"中文(简体)"下的配置编号。
pub const GUID_PROFILE: GUID = GUID::from_u128(pinyin_types::TIP_PROFILE);
/// 中文(简体，中国)
pub const LANGID_ZH_CN: u16 = 0x0804;

static DLL_INSTANCE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub fn dll_instance() -> HINSTANCE {
    HINSTANCE(DLL_INSTANCE.load(Ordering::Relaxed))
}

/// 任何代码出错（panic）都在这里兜住，返回 `fallback`，绝不让宿主程序崩溃。
pub fn guard<T>(f: impl FnOnce() -> T, fallback: T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DLL_INSTANCE.store(hinst.0, Ordering::Relaxed);
    }
    true.into()
}

/// # Safety
/// 由 COM 调用，参数是 COM 约定的指针。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    guard(
        || unsafe {
            if ppv.is_null() || rclsid.is_null() || riid.is_null() {
                return E_POINTER;
            }
            *ppv = std::ptr::null_mut();
            if *rclsid != CLSID_PINYIN {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let f: IClassFactory = factory::ClassFactory.into();
            f.query(riid, ppv)
        },
        E_FAIL,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // 一直留在内存里：简单、安全（卸载时机不对会让程序崩溃）
    S_FALSE
}

#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    guard(
        || match register::register() {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        },
        E_FAIL,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    guard(
        || match register::unregister() {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        },
        E_FAIL,
    )
}
