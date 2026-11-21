//! COM 类工厂：系统通过它创建输入法对象。

use std::ffi::c_void;

use windows::core::{implement, IUnknown, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

use super::guard;
use super::service::TextService;

#[implement(IClassFactory)]
pub struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, punkouter: Ref<IUnknown>, riid: *const GUID, ppvobject: *mut *mut c_void) -> Result<()> {
        guard(
            || unsafe {
                if ppvobject.is_null() || riid.is_null() {
                    return Err(E_POINTER.into());
                }
                *ppvobject = std::ptr::null_mut();
                if !punkouter.is_null() {
                    return Err(CLASS_E_NOAGGREGATION.into());
                }
                let unk: IUnknown = TextService::new().into();
                unk.query(riid, ppvobject).ok()
            },
            Err(E_FAIL.into()),
        )
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}
