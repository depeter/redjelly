#!/usr/bin/env bash
# Cross-compiles redjelly.exe for Windows x64 from Linux and packages it with
# libmpv-2.dll into dist/redjelly-windows-x64.zip.
#
#   LLVM_MINGW=/path/to/llvm-mingw-*-ucrt-ubuntu-*-x86_64 \
#   MPV_DEV=/path/to/extracted/mpv-dev-x86_64 \
#   [MAKENSIS=/path/to/makensis NSISDIR=/path/to/share/nsis] \
#   scripts/build-windows.sh
#
# With MAKENSIS set it also builds dist/redjelly-setup-<version>.exe.
# NSIS without root: `apt-get download nsis nsis-common` + `dpkg-deb -x`.
#
# llvm-mingw: https://github.com/mstorsjo/llvm-mingw/releases
# mpv-dev:    https://github.com/zhongfly/mpv-winbuild/releases (mpv-dev-x86_64-*.7z)
set -euo pipefail

: "${LLVM_MINGW:?set LLVM_MINGW to the extracted llvm-mingw directory}"
: "${MPV_DEV:?set MPV_DEV to the extracted mpv-dev directory}"
TARGET=x86_64-pc-windows-gnullvm
root="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="$LLVM_MINGW/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++
export AR_x86_64_pc_windows_gnullvm=llvm-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS="-L native=$MPV_DEV"
export WINDRES=x86_64-w64-mingw32-windres

cargo build --release --target "$TARGET" -p redjelly --manifest-path "$root/Cargo.toml"

dist="$root/dist/redjelly"
rm -rf "$dist" && mkdir -p "$dist"
cp "$root/target/$TARGET/release/redjelly.exe" "$MPV_DEV/libmpv-2.dll" "$dist/"
cp "$root/scripts/README-windows.txt" "$dist/README.txt"
# C++/unwind runtime from llvm-mingw, needed by some native deps.
for dll in libc++.dll libunwind.dll; do
  [ -f "$LLVM_MINGW/x86_64-w64-mingw32/bin/$dll" ] && cp "$LLVM_MINGW/x86_64-w64-mingw32/bin/$dll" "$dist/"
done
(cd "$root/dist" && rm -f redjelly-windows-x64.zip && python3 -c "import shutil; shutil.make_archive('redjelly-windows-x64', 'zip', '.', 'redjelly')")
echo "built $root/dist/redjelly-windows-x64.zip"

if [ -n "${MAKENSIS:-}" ]; then
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
  setup="$root/dist/redjelly-setup-$version.exe"
  "$MAKENSIS" -V2 -DVERSION="$version" -DSRC="$dist" -DOUT="$setup" "$root/installer/redjelly.nsi"
  echo "built $setup"
fi
