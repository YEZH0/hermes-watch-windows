#!/usr/bin/env python3
"""生成 gui/app.ico —— 橙底圆角方块 + 深色 H，纯标准库实现（不依赖 Pillow）。

用法：python scripts/make-icon.py
"""
from __future__ import annotations

import struct
import zlib
from pathlib import Path

SIZE = 256
SS = 3  # 超采样倍数，用来做边缘抗锯齿

ACCENT = (0xF2, 0x73, 0x1C)   # Hermès 橙
INK = (0x12, 0x15, 0x1A)      # 与界面深色一致


def rounded_rect(x: float, y: float, m: float, r: float) -> bool:
    """点 (x,y) 是否落在 边距 m、圆角 r 的圆角矩形里。"""
    lo, hi = m, SIZE - m
    if not (lo <= x <= hi and lo <= y <= hi):
        return False
    cx = min(max(x, lo + r), hi - r)
    cy = min(max(y, lo + r), hi - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_glyph(x: float, y: float) -> bool:
    """H 字形：两根竖笔 + 一根横笔。"""
    left = 86 <= x <= 114
    right = 142 <= x <= 170
    stem = 74 <= y <= 182
    bar = 116 <= y <= 140 and 86 <= x <= 170
    return (stem and (left or right)) or bar


def build_pixels() -> bytearray:
    rows = bytearray()
    inv = 1.0 / (SS * SS)
    for py in range(SIZE):
        rows.append(0)  # PNG filter type 0
        for px in range(SIZE):
            bg_hits = 0
            fg_hits = 0
            for sy in range(SS):
                for sx in range(SS):
                    x = px + (sx + 0.5) / SS
                    y = py + (sy + 0.5) / SS
                    if rounded_rect(x, y, 6.0, 58.0):
                        bg_hits += 1
                        if in_glyph(x, y):
                            fg_hits += 1
            if bg_hits == 0:
                rows.extend((0, 0, 0, 0))
                continue
            bg_cov = bg_hits * inv
            fg_cov = fg_hits * inv
            # 前景压在背景上
            r = ACCENT[0] * (1 - fg_cov) + INK[0] * fg_cov
            g = ACCENT[1] * (1 - fg_cov) + INK[1] * fg_cov
            b = ACCENT[2] * (1 - fg_cov) + INK[2] * fg_cov
            rows.extend((int(r + 0.5), int(g + 0.5), int(b + 0.5), int(bg_cov * 255 + 0.5)))
    return rows


def png_bytes(raw: bytearray) -> bytes:
    def chunk(tag: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def main() -> None:
    png = png_bytes(build_pixels())
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 6 + 16)
    out = Path(__file__).resolve().parent.parent / "gui" / "app.ico"
    out.write_bytes(header + entry + png)
    print(f"wrote {out} ({len(png)} bytes png, {out.stat().st_size} bytes total)")


if __name__ == "__main__":
    main()
