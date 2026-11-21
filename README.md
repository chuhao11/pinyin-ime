<p align="center"><img src="assets/icon/pinyin-256.png" width="128" alt="拼音输入法图标"></p>

<h1 align="center">拼音输入法</h1>

<p align="center"><b>写给中国人的 Windows 拼音输入法：所有符号默认都是英文的。</b><br>
再也不会因为一个中文逗号、中文引号，让代码报错、命令跑不起来。</p>

---

## 为什么做这个

用中文输入法写代码、写命令、填表格的人，大概都被这种事坑过：

```python
print("你好"，name)     # 中文逗号，报错
```

```bash
git commit -m “修复”    # 中文引号，命令出错
```

这个输入法反过来设计：**打中文的时候，标点和符号也和英文键盘完全一样**。
`, . ? ! : ; ( ) " '` 全都是英文半角，空格也永远是普通的半角空格。
不用再切中英文、不用回头找那个看不出来的全角符号。
（真的想要中文标点的话，设置里一个开关就能打开，开了就全部是中文标点。）

## 特点

- **符号永远是英文的**（默认）：写代码、写命令、写邮件地址、写 Markdown 都不出错。
- **整句输入准**：内置语言模型，`jintiantianqizhenhao` 直接出“今天天气真好”；有“日常”“书面”两种文风可选。
- **打错了也能认出来**：键盘上按偏了、拼音顺序打反了，照样给出你想要的字，还会记住你常犯的错。
- **会记住你的词**：人名、菜名、专业词，选过一次就排到前面；常用字的顺序不会被一两次误选打乱。
- **中英混打**：打英文单词时，第一个候选永远是你打的原样字母，后面给出拼写建议。
- **单按 Shift 切换中英**：光标旁边闪一下“中 / 英”，然后自己消失。
- **候选多一屏**：按 `-` 打开 5×5 候选矩阵，方向键挑，数字键选。
- **安静**：不打字时不弹任何窗口；没有皮肤商店、新闻弹窗、广告。
- **完全不联网**：不上传任何东西，你的词语记忆只存在自己电脑上。
- **不打扰别的程序**：快捷键、密码框、全屏游戏里自动让开，引擎出问题也绝不卡住你正在用的程序。
- **省资源**：不用显卡；词库和模型按需读取，引擎自己只占十几 MB 内存。

## 安装（Windows 10 / 11，64 位）

1. 打开 [Actions](../../actions) 里最新一次成功的“编译和测试”，在页面最下面下载 `PinyinIME-Setup`，解压得到 `PinyinIME-Setup-0.1.x.exe`。
2. 双击安装。安装程序没有数字签名，Windows 可能提示“已保护你的电脑”，点“更多信息 → 仍要运行”。
3. 按 **Win+空格** 切到“拼音”就能用了。桌面上的“拼音输入法设置”可以改文风和中文标点。

卸载：设置 → 应用 → 拼音输入法。你的词语记忆保留在 `%APPDATA%\PinyinIME`，不想要了删掉这个文件夹即可。

> 这是个人项目，主要按作者自己的习惯设计。设计细节和按键规则见 [docs/方案.md](docs/方案.md)。

## 开发

用 Rust 写的。引擎核心在 Mac 上也能跑（终端里试打）；Windows 部分由 GitHub Actions 编译和测试。

### 在 Mac 上试打

```bash
tools/fetch-data.sh                      # 下载词库源数据（固定版本，核对校验码）
cargo run --release -p dictgen           # 编译词库到 data/build/pinyin.dict
tools/fetch-lm.sh                        # 下载语言模型（GitHub Releases 里的 pinyin.lm）
cargo run --release -p pinyin-cli        # 打开试打工具
```

试打工具记住的词存在 `~/.pinyin-ime-try/user.db`，删掉这个文件就清空记忆。

### 目录

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

### 测试

```bash
cargo test --release --workspace
```

包括：按键规则、记忆、打字回归测试（用真实词库）、连续 12 万次按键的内存测试。GitHub 上每次提交都会在 Windows 和 Mac 上各跑一遍。

## 许可证和数据来源

本项目以 [GPL-3.0](LICENSE) 发布：可以自由使用、修改、再发布；修改后再发布也要以 GPL-3.0 开源。

用到的数据（编译时下载，不在仓库里）：

| 数据 | 用途 | 来源 |
|---|---|---|
| 雾凇拼音 rime-ice（GPL-3.0） | 中文词库、英文词库 | [iDvel/rime-ice](https://github.com/iDvel/rime-ice) |
| Unicode Unihan 18.0 | 字的等级、笔画 | [unicode.org](https://www.unicode.org/charts/unihan.html) |
| google-10000-english | 英文词频 | [first20hours/google-10000-english](https://github.com/first20hours/google-10000-english) |
| LCCC 对话语料（清华） | 语言模型：日常口语 | [thu-coai/CDial-GPT](https://github.com/thu-coai/CDial-GPT) |
| 中文维基百科（CC BY-SA 4.0） | 语言模型：书面语 | [zh.wikipedia.org](https://zh.wikipedia.org) |

语言模型只保存词和词之间的统计次数，不包含原文。
