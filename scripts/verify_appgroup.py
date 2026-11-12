#!/usr/bin/env python3
"""校验签名后的 IPA 是否真的带上了 App Groups 权限。

用法:
    python verify_appgroup.py <已签名的.ipa> [--expect-group group.xxx]
    python verify_appgroup.py <解包后的目录>  [--expect-group group.xxx]

第二个用法用于签名工具只留下解包目录、没有回写 .ipa 的情况
（iLoader / isideload 在 delete_app_after_install=false 时会保留
%TEMP%\\<name>.ipa_extracted，目录里应含 Payload/）。

检查两处来源、四个对象:
  * 每个 embedded.mobileprovision 的 Entitlements（iPhone App / Watch App）
  * 每个 Mach-O 可执行文件内嵌的 entitlements blob

任一必需项缺失都会以非 0 退出码结束。
"""
from __future__ import annotations

import os
import plistlib
import sys
import tempfile
import zipfile
from pathlib import Path

GROUP_KEY = "com.apple.security.application-groups"
APPID_KEY = "application-identifier"

MACHO_MAGICS = (
    b"\xcf\xfa\xed\xfe",  # MH_MAGIC_64 little endian
    b"\xfe\xed\xfa\xcf",  # MH_MAGIC_64 big endian
    b"\xce\xfa\xed\xfe",  # MH_MAGIC 32
    b"\xfe\xed\xfa\xce",
    b"\xca\xfe\xba\xbe",  # FAT
    b"\xbe\xba\xfe\xca",
)


def plist_blobs(data: bytes):
    """在二进制里找出所有内嵌 XML plist —— entitlements blob 就是这样存的。"""
    pos = 0
    while True:
        start = data.find(b"<?xml", pos)
        if start < 0:
            return
        end = data.find(b"</plist>", start)
        if end < 0:
            return
        try:
            yield plistlib.loads(data[start : end + len(b"</plist>")])
        except Exception:
            pass
        pos = start + 1


def cms_payload(raw: bytes) -> bytes:
    """mobileprovision 是 CMS 签名包，里面裹着一段 XML plist。"""
    start, end = raw.find(b"<?xml"), raw.find(b"</plist>")
    if start < 0 or end < 0:
        raise ValueError("no plist inside mobileprovision")
    return raw[start : end + len(b"</plist>")]


def tier(path: str) -> str:
    return "watch " if "/Watch/" in path or path.endswith("watchkitapp") else "iphone "


def show(label: str, appid, groups, expect: str | None) -> bool:
    """groups 可能是 ['group.a']（profile）或 [['group.a']]（多个 entitlements blob）。"""
    flat: set[str] = set()
    for item in groups or []:
        if isinstance(item, str):
            flat.add(item)
        else:
            flat.update(item)
    flat = sorted(flat)
    print(f"  {APPID_KEY:<22}= {appid}")
    print(f"  {GROUP_KEY} = {flat or 'MISSING'}")
    ok = bool(flat) and (expect is None or expect in flat)
    print("  -> " + ("OK" if ok else "FAIL"))
    return ok


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    target = Path(sys.argv[1])
    expect = None
    if "--expect-group" in sys.argv:
        expect = sys.argv[sys.argv.index("--expect-group") + 1]

    tmp_zip = None
    if target.is_dir():
        if not (target / "Payload").is_dir():
            print(f"目录里没有 Payload/：{target}")
            return 2
        fd, tmp_zip = tempfile.mkstemp(suffix=".ipa")
        os.close(fd)
        with zipfile.ZipFile(tmp_zip, "w", zipfile.ZIP_DEFLATED) as z:
            for root, _dirs, files in os.walk(target):
                for name in files:
                    full = os.path.join(root, name)
                    z.write(full, os.path.relpath(full, target).replace(os.sep, "/"))
        print(f"(目录已打包为临时 ipa: {tmp_zip})")
        target = Path(tmp_zip)

    try:
        return _check(target, expect)
    finally:
        if tmp_zip and os.path.exists(tmp_zip):
            os.unlink(tmp_zip)


def _check(ipa: Path, expect: str | None) -> int:
    results: list[bool] = []
    with zipfile.ZipFile(ipa) as z:
        names = z.namelist()
        profiles = sorted(n for n in names if n.endswith("embedded.mobileprovision"))
        if not profiles:
            print("IPA 里没有 embedded.mobileprovision —— 尚未签名")
            return 1

        for p in profiles:
            info = plistlib.loads(cms_payload(z.read(p)))
            ent = info.get("Entitlements", {})
            print(f"\n[{tier(p)}profile] {p}")
            print(f"  Name = {info.get('Name', '?')}")
            print(f"  Expires = {info.get('ExpirationDate', '?')}")
            results.append(show("profile", ent.get(APPID_KEY), ent.get(GROUP_KEY), expect))

        for n in names:
            if n.endswith("/") or "/_CodeSignature/" in n:
                continue
            head = z.open(n).read(4)
            if head not in MACHO_MAGICS:
                continue
            data = z.read(n)
            groups, appid = [], None
            for blob in plist_blobs(data):
                if isinstance(blob, dict) and blob.get(GROUP_KEY):
                    groups.append(blob[GROUP_KEY])
                    appid = appid or blob.get(APPID_KEY)
            print(f"\n[{tier(n)}binary] {n}")
            results.append(show("binary", appid, groups, expect))

    print("\n" + "=" * 64)
    if all(results):
        print(f"全部通过（{len(results)} 项）—— App Groups 已生效")
        return 0
    print(f"失败 {results.count(False)}/{len(results)} 项")
    return 1


if __name__ == "__main__":
    sys.exit(main())
