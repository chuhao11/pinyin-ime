#!/usr/bin/env bash
# 下载词库源数据到 data/raw/（固定版本，并核对 SHA-256 校验码，防止被篡改）。
# 用法：tools/fetch-data.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# 雾凇拼音（iDvel/rime-ice）固定在这个提交上，更新词库时改这里并更新下面的校验码。
RIME_COMMIT=927e1cf1d3eeb9bf4be85e3f684dba4dd485b6f6
RIME_BASE="https://raw.githubusercontent.com/iDvel/rime-ice/${RIME_COMMIT}/cn_dicts"
UNIHAN_URL="https://www.unicode.org/Public/18.0.0/ucd/Unihan.zip"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

fetch() {
  local url=$1 out=$2 want=$3
  if [ -f "$out" ] && [ "$(sha256 "$out")" = "$want" ]; then
    echo "已有：$out"; return
  fi
  echo "下载：$url"
  curl -fsSL --retry 3 -o "$out.part" "$url"
  local got; got=$(sha256 "$out.part")
  if [ "$got" != "$want" ]; then
    echo "校验失败：$out"; echo "  期望 $want"; echo "  实际 $got"; rm -f "$out.part"; exit 1
  fi
  mv "$out.part" "$out"
}

mkdir -p data/raw/rime-ice data/raw/unihan

fetch "$RIME_BASE/8105.dict.yaml"   data/raw/rime-ice/8105.dict.yaml   1f9a42b91dea6982baee2551981780271aeffd78876662b9c9f324e56b37b120
fetch "$RIME_BASE/41448.dict.yaml"  data/raw/rime-ice/41448.dict.yaml  39102601365554433396fa64947010f5a15e7a2c27d22f6cce4d80775919e7bc
fetch "$RIME_BASE/base.dict.yaml"   data/raw/rime-ice/base.dict.yaml   c1f07daa4680a437a04169211e4a80b19923fd61cff322753d3ee98a7e1bffc8
fetch "$RIME_BASE/ext.dict.yaml"    data/raw/rime-ice/ext.dict.yaml    f713d82f5a3c38bf955fef631353c6bd08c3e1ae6674d7db0c4f286480d760a1
fetch "$RIME_BASE/others.dict.yaml" data/raw/rime-ice/others.dict.yaml c7475d773e619f61b88d47fa8ec789aba02aa0d1c98edaed309e53487b048b32
fetch "$RIME_BASE/tencent.dict.yaml" data/raw/rime-ice/tencent.dict.yaml af31db5df83239e50ee6da015ab5801953be544117fcf7419988ca315f55102b
fetch "$UNIHAN_URL" data/raw/unihan/Unihan.zip 4c93ea9c1f636451729a840978f1667a53886af37ba854fdcce109721c63d43e

if [ ! -f data/raw/unihan/Unihan_OtherMappings.txt ] || [ ! -f data/raw/unihan/Unihan_IRGSources.txt ]; then
  echo "解压 Unihan.zip"
  if command -v unzip >/dev/null 2>&1; then
    (cd data/raw/unihan && unzip -o -q Unihan.zip)
  else
    (cd data/raw/unihan && tar -xf Unihan.zip)
  fi
fi
echo "词库源数据就绪。"
