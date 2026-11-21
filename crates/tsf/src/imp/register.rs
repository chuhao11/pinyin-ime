//! 注册 / 注销输入法（安装程序用 regsvr32 调用）。
//!
//! - 注册表：HKLM\Software\Classes\CLSID\{…}\InprocServer32 = 本 DLL（32 位 DLL 会自动写到 WOW6432Node）
//! - TSF：在"中文(简体)"下登记一个叫"拼音"的键盘输入法，图标用同目录的 pinyin.ico
//! - 类别：键盘输入法、支持商店应用（沉浸式）、支持系统托盘的输入指示
//!   不登记"安全模式"：登录界面、UAC 弹窗里不会加载

use windows::core::{Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr, ITfInputProcessorProfileMgr,
    ITfInputProcessorProfiles, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIP_KEYBOARD,
};

use super::{dll_instance, CLSID_PINYIN, GUID_PROFILE, LANGID_ZH_CN};

const CATEGORIES: [windows::core::GUID; 3] =
    [GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT];

pub fn dll_path() -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(Some(dll_instance().into()), &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn set_value(key: &str, name: &str, value: &str) -> Result<()> {
    unsafe {
        let mut h = HKEY::default();
        let r = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h,
            None,
        );
        r.ok()?;
        let mut data: Vec<u8> = value.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
        data.shrink_to_fit();
        let name_h = HSTRING::from(name);
        let name_p = if name.is_empty() { PCWSTR::null() } else { PCWSTR(name_h.as_ptr()) };
        let r = RegSetValueExW(h, name_p, None, REG_SZ, Some(&data));
        let _ = RegCloseKey(h);
        r.ok()
    }
}

fn clsid_key() -> String {
    format!("Software\\Classes\\CLSID\\{}", pinyin_types::guid_string(pinyin_types::TIP_CLSID))
}

pub fn register() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let path = dll_path();
    let key = clsid_key();
    set_value(&key, "", "拼音输入法")?;
    set_value(&format!("{key}\\InprocServer32"), "", &path)?;
    set_value(&format!("{key}\\InprocServer32"), "ThreadingModel", "Apartment")?;

    let icon = std::path::Path::new(&path)
        .parent()
        .map(|d| d.join("pinyin.ico").to_string_lossy().into_owned())
        .unwrap_or_default();
    unsafe {
        let pm: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        pm.RegisterProfile(
            &CLSID_PINYIN,
            LANGID_ZH_CN,
            &GUID_PROFILE,
            &utf16("拼音"),
            &utf16(&icon),
            0,
            HKL::default(),
            0,
            true,
            0,
        )?;
        let cm: ITfCategoryMgr = CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for c in CATEGORIES {
            cm.RegisterCategory(&CLSID_PINYIN, &c, &CLSID_PINYIN)?;
        }
    }
    Ok(())
}

pub fn unregister() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Ok(pm) = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            let _ = pm.UnregisterProfile(&CLSID_PINYIN, LANGID_ZH_CN, &GUID_PROFILE, 0);
        }
        if let Ok(cm) = CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) {
            for c in CATEGORIES {
                let _ = cm.UnregisterCategory(&CLSID_PINYIN, &c, &CLSID_PINYIN);
            }
        }
        // 旧接口能把整个输入法登记删干净（新接口只删配置，会留下空的登记）
        if let Ok(pp) = CoCreateInstance::<_, ITfInputProcessorProfiles>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            let _ = pp.Unregister(&CLSID_PINYIN);
        }
        let tip = format!("Software\\Microsoft\\CTF\\TIP\\{}", pinyin_types::guid_string(pinyin_types::TIP_CLSID));
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(tip));
        let r = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(clsid_key()));
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return r.ok();
        }
    }
    Ok(())
}
