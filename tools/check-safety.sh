#!/usr/bin/env bash
# 安全检查（GitHub 每次编译都跑，本机也能跑）：
# 1. 发出去的程序只用审查过的第三方代码（tools/allowed-deps.txt），多一个都不行
# 2. 发出去的程序的源码里没有联网、剪贴板、动态加载别的模块这些东西
# Windows 上编译出来以后，CI 还会再查一遍程序文件本身（见 .github/workflows/ci.yml 的"安全检查"）。
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

# ---------- 1. 依赖清单 ----------
got=$(
  for t in x86_64-pc-windows-msvc i686-pc-windows-msvc; do
    for p in pinyin-tsf pinyin-winsrv pinyin-settings; do
      cargo tree --locked -p "$p" --target "$t" -e normal,build --prefix none --format '{p}'
    done
  done | awk '{print $1}' | sort -u
)
want=$(grep -v '^#' tools/allowed-deps.txt | sed '/^[[:space:]]*$/d' | sort -u)
extra=$(comm -13 <(echo "$want") <(echo "$got"))
if [ -n "$extra" ]; then
  echo "✗ 发出去的程序多了没审查过的依赖（审查后再加进 tools/allowed-deps.txt）："
  echo "$extra"
  fail=1
else
  echo "✓ 依赖都在审查过的清单里（$(echo "$got" | wc -l | tr -d ' ') 个）"
fi

# ---------- 2. 源码里不许出现的东西 ----------
# 联网：Rust 的网络模块、Windows 的网络接口、常见的联网库
# 剪贴板：Windows 剪贴板接口（windows-rs 里在 Win32_System_DataExchange 下）、WinRT 的 DataTransfer
# 动态加载：LoadLibrary / GetProcAddress（能绕过上面的检查）
pattern='std::net|TcpStream|TcpListener|UdpSocket|Win32_Networking|Win32_NetworkManagement|Win32_System_DataExchange|"Networking|"Web|DataTransfer|Clipboard|LoadLibrary|GetProcAddress|WinHttp|InternetOpen|URLDownload|WSAStartup|reqwest|ureq|hyper|curl'
# 唯一的例外：设置页从系统目录（System32）加载 input.dll，调用微软的"安装输入法"接口
allowed='input\.dll|InstallLayoutOrTip|LOAD_LIBRARY_SEARCH_SYSTEM32'
hits=$(grep -rnE "$pattern" crates/tsf crates/winsrv crates/settings crates/winpipe crates/ipc crates/types crates/engine \
  --include='*.rs' --include='Cargo.toml' | grep -vE "$allowed" || true)
if [ -n "$hits" ]; then
  echo "✗ 源码里出现了联网 / 剪贴板 / 动态加载相关的代码："
  echo "$hits"
  fail=1
else
  echo "✓ 源码里没有联网、剪贴板、动态加载"
fi

exit $fail
