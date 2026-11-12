#!/usr/bin/env bash
# 复现 hermesfix.exe 的构建。
#
# 用法:
#   export MINGW=/c/Dev/w64devkit        # 含 bin/x86_64-w64-mingw32-gcc.exe
#   export RUSTC=/c/rt/bin/rustc.exe     # 见下方"中文路径"说明
#   bash scripts/build-windows-gnu.sh
#
# 只在本机（Windows x64 + GNU toolchain）验证过；Linux/macOS 请改用各自的
# x86_64-pc-windows-gnu 交叉工具链，其余步骤相同。
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
cd "$root/hermesfix"

: "${MINGW:?需要设置 MINGW 指向 MinGW-w64 根目录（含 bin/x86_64-w64-mingw32-gcc.exe）}"
MINGW_UNIX=$(cygpath -u "$MINGW" 2>/dev/null || echo "$MINGW")
MINGW_NATIVE=$(cygpath -w "$MINGW_UNIX" 2>/dev/null | tr '\\' '/' || echo "$MINGW_UNIX")

GCC="$MINGW_UNIX/bin/x86_64-w64-mingw32-gcc.exe"
AR="$MINGW_UNIX/bin/x86_64-w64-mingw32-ar.exe"
DLLTOOL="$MINGW_UNIX/bin/x86_64-w64-mingw32-dlltool.exe"
for f in "$GCC" "$AR" "$DLLTOOL"; do
    [ -x "$f" ] || { echo "缺少 $f" >&2; exit 1; }
done

# ---------------------------------------------------------------- 中文路径
# binutils 的 Windows 版用 ANSI 代码页处理路径。用户名含中文时，cc/dlltool
# 会把 C:\Users\<中文> 读成 C:\Users\Ò¶Ö¾ºê，编译直接失败。
# 唯一可靠的解法是把 rustc 放到纯 ASCII 路径上，再用 RUSTC 指过去
# （RUSTUP_HOME / subst 都不行：rustup 会 canonicalize 回真实路径，
#   一旦发现 toolchain 位置变了就会触发全量重编）。
if [ -z "${RUSTC:-}" ]; then
    RUSTC="${MINGW_NATIVE%/bin}/rustc.exe"
    if command -v rustc >/dev/null 2>&1; then
        RUSTC=$(command -v rustc)
    fi
fi
if printf '%s' "$RUSTC" | grep -qP '[^\x00-\x7F]'; then
    echo "警告: RUSTC=$RUSTC 含非 ASCII 字符，binutils 很可能失败" >&2
fi
export RUSTC

# ---------------------------------------------------------------- 编译器
export CC_x86_64_pc_windows_gnu="$MINGW_NATIVE/bin/x86_64-w64-mingw32-gcc.exe"
export AR_x86_64_pc_windows_gnu="$MINGW_NATIVE/bin/x86_64-w64-mingw32-ar.exe"
export CFLAGS_x86_64_pc_windows_gnu=""
# rustc 自己找不到 dlltool，必须显式给
export RUSTFLAGS="-C dlltool=$MINGW_NATIVE/bin/x86_64-w64-mingw32-dlltool.exe"

# ---------------------------------------------------------------- 两个已知缺口
# 1) ring 的 build.rs 把 nasm 路径写死成 <target>/tools/windows/nasm/nasm
NasmDir="target/tools/windows/nasm"
if [ ! -x "$NasmDir/nasm" ]; then
    echo "[i] 放置 nasm 到 $NasmDir/nasm 以让 ring 的 build.rs 找到它"
    mkdir -p "$NasmDir"
    if command -v nasm >/dev/null 2>&1; then
        cp "$(command -v nasm)" "$NasmDir/nasm"
    else
        echo "    未找到 nasm；如果编译在 ring 处停止，手动拷贝 nasm.exe 到 $NasmDir/nasm" >&2
    fi
fi

# 2) GCC 16 起 libgcc_eh 被并进 libgcc，rustc 仍在找 -lgcc_eh
LibFile=$("$GCC" -print-libgcc-file-name)
LibDir=${LibFile%[/\\]*}
if [ ! -e "$LibDir/libgcc_eh.a" ]; then
    echo "[i] 补一个空的 libgcc_eh.a 占位 -> $LibDir"
    "$AR" crs "$LibDir/libgcc_eh.a" 2>/dev/null \
        || echo "    无写权限，跳过（仅 GCC>=16 需要）" >&2
fi

# ---------------------------------------------------------------- 构建
echo "[i] rustc    = $RUSTC"
echo "[i] mingw    = $MINGW_NATIVE"
cargo build --release --offline

echo
echo "产物: $root/hermesfix/target/release/hermesfix.exe"
