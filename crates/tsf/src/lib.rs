//! Windows 输入法框架（TSF）里的输入法 DLL：每个程序里加载的那个很薄的部分。
//!
//! 它只做三件事：把按键转给引擎进程、把结果写进程序（组字、上屏）、画候选窗。
//! 词库、语言模型、你的记忆都在引擎进程里，这个 DLL 不带它们。
//!
//! 和别的程序和平共处（详见 docs/方案.md）：
//! - 不打字时不出任何窗口；候选窗不抢焦点、不进任务栏和 Alt+Tab。
//! - Ctrl/Alt/Win 组合键、功能键一律放过；没有输入框、只读区域、密码框、全屏游戏里一律放过。
//! - 引擎没启动或者卡住：按键原样放过，最多等 0.3 秒，绝不拖住程序。
//! - DLL 里任何地方出错都兜住，不会让宿主程序崩溃。

pub mod fields;
pub mod gate;
#[cfg(windows)]
mod imp;
pub mod screen;

/// COM 入口（系统通过它们使用输入法）；导出给 Windows 自动测试用。
#[cfg(windows)]
pub use imp::{DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer, CLSID_PINYIN};

pub use pinyin_ipc;
pub use pinyin_types;
