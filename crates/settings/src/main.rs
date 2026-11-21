//! 设置页（pinyin-settings.exe，桌面上的"拼音输入法设置"）。
//!
//! 只有两行：文风（日常 / 书面）、中文标点（开 / 关）。点一下立刻保存、立刻生效。
//! 关掉窗口就退出，不在后台常驻；画界面用软件渲染，不加载显卡驱动，占用的内存很小。

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    win::run();
    #[cfg(not(windows))]
    {
        let p = pinyin_ipc::settings_path();
        let s = pinyin_ipc::Settings::load(&p);
        println!("设置页只在 Windows 上有窗口。设置文件：{}", p.display());
        println!(
            "文风：{}　中文标点：{}",
            if s.formal { "书面" } else { "日常" },
            if s.chinese_punct { "开" } else { "关" }
        );
    }
}
