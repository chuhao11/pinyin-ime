# 拼音输入法（自用）

给自己用的 Windows 拼音输入法。设计和已定规则见 [docs/方案.md](docs/方案.md)。

现在完成的是第一步：引擎核心（拼音切分、词库、排序、记忆、按键规则），可以在 Mac 上试打。

## 在 Mac 上试打

```bash
tools/fetch-data.sh                      # 下载词库源数据（固定版本，核对校验码）
cargo run --release -p dictgen           # 编译词库到 data/build/pinyin.dict
cargo run --release -p pinyin-cli        # 打开试打工具
```

试打工具记住的词存在 `~/.pinyin-ime-try/user.db`，删掉这个文件就清空记忆。

## 目录

| 目录 | 内容 |
|---|---|
| `crates/engine` | 引擎核心（Mac 和 Windows 通用） |
| `crates/dictgen` | 词库编译工具 |
| `crates/cli` | 终端试打工具 |
| `crates/engine/tests/cases.tsv` | 打字回归测试：拼音 → 应该排在前几位的字词 |
| `docs/` | 方案、表外字清单 |
| `tools/fetch-data.sh` | 下载词库源数据 |

## 测试

```bash
cargo test --release --workspace
```

包括：按键规则、记忆、打字回归测试（用真实词库）、连续 12 万次按键的内存测试。GitHub 上每次提交都会在 Windows 和 Mac 上各跑一遍。
