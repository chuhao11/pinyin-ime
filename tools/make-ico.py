"""把各尺寸的 PNG 打包成 Windows 的 .ico 文件（每一项直接存 PNG，Windows Vista 以后都支持）。

用法：python3 tools/make-ico.py assets/icon assets/icon/pinyin.ico
"""
import struct
import sys
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]


def main() -> None:
    src = Path(sys.argv[1])
    out = Path(sys.argv[2])
    images = [(s, (src / f"pinyin-{s}.png").read_bytes()) for s in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    blobs = b""
    for size, data in images:
        b = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    out.write_bytes(header + entries + blobs)
    print(f"生成 {out}（{len(images)} 种尺寸）")


if __name__ == "__main__":
    main()
