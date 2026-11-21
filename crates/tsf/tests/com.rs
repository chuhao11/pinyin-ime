//! Windows 上的冒烟测试。
//!
//! - `entry_points`：不安装、不注册，直接从 DLL 的入口创建输入法对象，调它的按键接口，确认不出错、不卡住。
//! - `tsf_loads_registered_dll`：DLL 已经用 regsvr32 注册后（CI 里"注册"那一步），让 Windows 自己加载、
//!   激活这个输入法，再停用。默认不跑（`--ignored` 才跑），因为要管理员权限先注册。

#![cfg(windows)]

use std::time::Instant;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IClassFactory, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfCompositionSink, ITfContext, ITfInputProcessorProfileMgr,
    ITfKeyEventSink, ITfKeystrokeMgr, ITfTextInputProcessor, ITfTextInputProcessorEx, ITfThreadMgr,
    ITfThreadMgrEventSink, GUID_TFCAT_TIP_KEYBOARD, TF_INPUTPROCESSORPROFILE, TF_IPPMF_FORPROCESS,
    TF_PROFILETYPE_INPUTPROCESSOR,
};

const KEYS: [usize; 5] = [0x41, 0x20, 0x0D, 0x10, 0x31]; // A、空格、回车、Shift、1

fn tip() -> ITfTextInputProcessorEx {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut p = std::ptr::null_mut();
        let wrong = GUID::zeroed();
        assert!(pinyin_tsf::DllGetClassObject(&wrong, &IClassFactory::IID, &mut p).is_err(), "别的编号不该给");
        assert!(pinyin_tsf::DllGetClassObject(&pinyin_tsf::CLSID_PINYIN, &IClassFactory::IID, &mut p).is_ok());
        let f = IClassFactory::from_raw(p);
        f.CreateInstance(None).unwrap()
    }
}

#[test]
fn entry_points() {
    let tip = tip();
    // 系统会用到的接口都在
    tip.cast::<ITfTextInputProcessor>().unwrap();
    tip.cast::<ITfThreadMgrEventSink>().unwrap();
    tip.cast::<ITfCompositionSink>().unwrap();
    let sink: ITfKeyEventSink = tip.cast().unwrap();
    unsafe {
        // 没有输入框：按键一律放过；引擎也没运行：不能卡住
        let t = Instant::now();
        for vk in KEYS {
            let down = sink.OnTestKeyDown(None::<&ITfContext>, WPARAM(vk), LPARAM(1)).unwrap();
            assert!(!down.as_bool(), "没有输入框时不该吃键 {vk:#x}");
            let up = sink.OnTestKeyUp(None::<&ITfContext>, WPARAM(vk), LPARAM(1 | (1 << 31))).unwrap();
            assert!(!up.as_bool(), "松开的键不该吃 {vk:#x}");
        }
        assert!(t.elapsed().as_secs_f32() < 2.0, "按键处理太慢：{:?}", t.elapsed());
        // 没激活就停用、停用两次：都不出错
        tip.Deactivate().unwrap();
        tip.Deactivate().unwrap();
    }
}

#[test]
#[ignore = "要先用 regsvr32 注册 DLL（CI 里注册之后单独运行）"]
fn tsf_loads_registered_dll() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let tm: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).unwrap();
        tm.Activate().unwrap();
        let pm: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER).unwrap();
        let clsid = GUID::from_u128(pinyin_types::TIP_CLSID);
        let profile = GUID::from_u128(pinyin_types::TIP_PROFILE);
        // 让 Windows 自己加载注册好的 DLL 并激活（和用户按 Win+空格 切过来一样）
        pm.ActivateProfile(
            TF_PROFILETYPE_INPUTPROCESSOR,
            0x0804,
            &clsid,
            &profile,
            HKL::default(),
            TF_IPPMF_FORPROCESS,
        )
        .unwrap();
        let mut active = TF_INPUTPROCESSORPROFILE::default();
        pm.GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut active).unwrap();
        assert_eq!(active.clsid, clsid, "激活的不是这个输入法");

        let km: ITfKeystrokeMgr = tm.cast().unwrap();
        let t = Instant::now();
        for vk in KEYS {
            let eaten = km.TestKeyDown(WPARAM(vk), LPARAM(1)).unwrap_or_default();
            assert!(!eaten.as_bool(), "没有输入框时不该吃键 {vk:#x}");
            let _ = km.TestKeyUp(WPARAM(vk), LPARAM(1 | (1 << 31)));
        }
        assert!(t.elapsed().as_secs_f32() < 2.0, "按键处理太慢：{:?}", t.elapsed());
        tm.Deactivate().unwrap();
    }
}
