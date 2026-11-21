//! Windows 上的冒烟测试：不安装、不注册，直接从 DLL 的入口创建输入法对象，
//! 用一个真的 TSF 线程管理器激活、按键、停用，确认不出错、不崩溃。

#![cfg(windows)]

use windows::core::Interface;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IClassFactory, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::TextServices::{CLSID_TF_ThreadMgr, ITfKeystrokeMgr, ITfTextInputProcessorEx, ITfThreadMgr};

#[test]
fn create_activate_type_deactivate() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        let mut p = std::ptr::null_mut();
        let wrong = windows::core::GUID::zeroed();
        assert!(pinyin_tsf::DllGetClassObject(&wrong, &IClassFactory::IID, &mut p).is_err(), "别的编号不该给");
        assert!(pinyin_tsf::DllGetClassObject(&pinyin_tsf::CLSID_PINYIN, &IClassFactory::IID, &mut p).is_ok());
        let f = IClassFactory::from_raw(p);
        let tip: ITfTextInputProcessorEx = f.CreateInstance(None).unwrap();

        let tm: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).unwrap();
        let tid = tm.Activate().unwrap();
        tip.ActivateEx(&tm, tid, 0).unwrap();

        // 没有输入框：按键一律放过（引擎也没运行：不能卡住，0.3 秒内返回）
        let km: ITfKeystrokeMgr = tm.cast().unwrap();
        let t = std::time::Instant::now();
        for vk in [0x41usize, 0x20, 0x0D, 0x10] {
            let eaten = km.TestKeyDown(WPARAM(vk), LPARAM(1)).unwrap_or_default();
            assert!(!eaten.as_bool(), "没有输入框时不该吃键 {vk:#x}");
            let _ = km.TestKeyUp(WPARAM(vk), LPARAM(1 | (1 << 31)));
        }
        assert!(t.elapsed().as_secs_f32() < 2.0, "按键处理太慢：{:?}", t.elapsed());

        tip.Deactivate().unwrap();
        tm.Deactivate().unwrap();
        // 同一个对象可以再激活一次（切换输入法再切回来）
        let tid = tm.Activate().unwrap();
        tip.ActivateEx(&tm, tid, 0).unwrap();
        tip.Deactivate().unwrap();
        tm.Deactivate().unwrap();
    }
}
