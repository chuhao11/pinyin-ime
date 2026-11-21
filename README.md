# 拼音输入法（自用）

给自己用的 Windows 拼音输入法。设计和已定规则见 [docs/方案.md](docs/方案.md)。

引擎核心（拼音切分、词库、排序、记忆、按键规则、打错纠正、英文、语言模型）可以在 Mac 上试打；
Windows 外壳（输入法 DLL、引擎进程、候选窗、设置页、安装程序）由 GitHub 自动编译。

## 在 Windows 上安装

1. 在 GitHub 仓库的 Actions 里打开最新一次成功的"编译和测试"，下载 `PinyinIME-Setup`（里面是 `PinyinIME-Setup-0.1.x.exe`）。
2. 拷到 Windows 电脑上双击安装。
3. 按 Win+空格 切到"拼音"。单按 Shift 切换中英；桌面上的"拼音输入法设置"可以改文风和中文标点。

卸载：设置 → 应用 → 拼音输入法。你的词语记忆保留在 `%APPDATA%\PinyinIME`，不想要了手动删掉这个文件夹。

## 在 Mac 上试打

```bash
tools/fetch-data.sh                      # 下载词库源数据（固定版本，核对校验码）
cargo run --release -p dictgen           # 编译词库到 data/build/pinyin.dict
tools/fetch-lm.sh                        # 下载语言模型（GitHub Releases 里的 pinyin.lm）
cargo run --release -p pinyin-cli        # 打开试打工具
```

试打工具记住的词存在 `~/.pinyin-ime-try/user.db`，删掉这个文件就清空记忆。

## 目录

| 目录 | 内容 |
|---|---|
| `crates/engine` | 引擎核心（Mac 和 Windows 通用） |
| `crates/dictgen` | 词库编译工具 |
| `crates/cli` | 终端试打工具 |
| `crates/lmgen` | 语言模型生成工具（要先下载几 GB 语料，见 docs/方案.md） |
| `crates/types`、`crates/ipc` | DLL 和引擎之间的通信协议 |
| `crates/winpipe` | Windows 命名管道（只允许本机本人连接） |
| `crates/winsrv` | 引擎进程 `pinyin-engine.exe` |
| `crates/tsf` | 输入法 DLL（按键、组字上屏、候选窗） |
| `crates/settings` | 设置页 `pinyin-settings.exe` |
| `installer/` | 安装程序脚本（Inno Setup） |
| `assets/icon/` | 图标 |
| `crates/engine/tests/cases.tsv` | 打字回归测试：拼音 → 应该排在前几位的字词 |
| `docs/` | 方案、表外字清单 |
| `tools/fetch-data.sh` | 下载词库源数据 |

## 测试

```bash
cargo test --release --workspace
```

包括：按键规则、记忆、打字回归测试（用真实词库）、连续 12 万次按键的内存测试。GitHub 上每次提交都会在 Windows 和 Mac 上各跑一遍。
