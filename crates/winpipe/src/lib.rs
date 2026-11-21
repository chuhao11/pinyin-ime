//! Windows 命名管道：输入法 DLL（每个程序里）和引擎进程之间的连接。
//!
//! 安全：
//! - 管道名带着当前用户的 SID：`\\.\pipe\PinyinIME-<SID>`。
//! - 管道只允许：当前用户、系统、商店应用沙箱（ALL APPLICATION PACKAGES，否则商店应用和开始菜单搜索里用不了），
//!   并且标成低完整性，沙箱里的程序也能连；拒绝网络上的连接。
//! - 引擎用 FILE_FLAG_FIRST_PIPE_INSTANCE 抢第一个实例：名字已经被别人占了就不启动。
//! - DLL 连上以后核对管道的所有者是不是自己这个用户（防止别的用户冒充引擎偷看按键），
//!   并且只允许引擎"识别"自己、不允许"冒充"自己（SECURITY_IDENTIFICATION）。
//! - DLL 这边每次收发最多等一小会儿（超时就放弃），引擎卡住也不会拖住程序。

#[cfg(windows)]
mod imp;

#[cfg(windows)]
pub use imp::*;
