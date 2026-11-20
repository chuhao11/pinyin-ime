#!/usr/bin/env bash
# 下载语言模型（放在本仓库的 GitHub Releases 里），并核对校验码：
#   data/build/pinyin.lm         日常（对话语料比重高）
#   data/build/pinyin-formal.lm  书面（维基百科比重高）
# 语言模型由 lmgen 从几 GB 语料生成，生成一次后上传到 Releases，这样 GitHub 自动编译时不用重新生成。
# 用法：tools/fetch-lm.sh   （需要 gh 已登录；GitHub Actions 里用自带的 GITHUB_TOKEN）
set -euo pipefail
cd "$(dirname "$0")/.."

TAG=lm-20261120b
GH=${GH:-gh}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

fetch() {
  local name=$1 want=$2 out=data/build/$1
  if [ -f "$out" ] && [ "$(sha256 "$out")" = "$want" ]; then
    echo "已有：$out"; return
  fi
  mkdir -p data/build
  "$GH" release download "$TAG" --repo chuhao11/pinyin-ime --pattern "$name" --dir data/build --clobber
  local got; got=$(sha256 "$out")
  if [ "$got" != "$want" ]; then
    echo "校验失败：$out"; echo "  期望 $want"; echo "  实际 $got"; rm -f "$out"; exit 1
  fi
  echo "语言模型就绪：$out"
}

fetch pinyin.lm        79d50d30ef4b5fbe31f452f0312e7e44e7944d355318d28ee8913087a0925cc2
fetch pinyin-formal.lm c59f8caa6e6a1a4b70cebd0ca80596b2c7e43d564f1f9663e9f4a3d0531ef522
