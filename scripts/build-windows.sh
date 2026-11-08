#!/bin/bash
# Cross-build the Windows zip on Linux: hyprclick.exe plus WinDivert's DLL,
# driver and license. Needs rustup's x86_64-pc-windows-gnu target and Nix
# (for MinGW's pthreads, which rust's windows-gnu target links).
set -eu
cd "$(dirname "$0")/.."
WINDIVERT=2.2.2
WINDIVERT_SHA256=63cb41763bb4b20f600b6de04e991a9c2be73279e317d4d82f237b150c5f3f15
OUT=dist/windows
CACHE=target/windivert

pthreads=$(nix build --no-link --print-out-paths nixpkgs#pkgsCross.mingwW64.windows.pthreads)
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L $pthreads/lib" \
    cargo build --release --target x86_64-pc-windows-gnu

zip="$CACHE/WinDivert-$WINDIVERT-A.zip"
if [ ! -f "$zip" ]; then
    mkdir -p "$CACHE"
    curl -fsSL -o "$zip.part" "https://github.com/basil00/WinDivert/releases/download/v$WINDIVERT/WinDivert-$WINDIVERT-A.zip"
    mv "$zip.part" "$zip"
fi
echo "$WINDIVERT_SHA256  $zip" | sha256sum -c --quiet
rm -rf "$CACHE/x" && mkdir -p "$CACHE/x"
unzip -q "$zip" -d "$CACHE/x"
src="$CACHE/x/WinDivert-$WINDIVERT-A"

rm -rf "$OUT" && mkdir -p "$OUT/hyprclick"
cp target/x86_64-pc-windows-gnu/release/hyprclick.exe "$OUT/hyprclick/"
cp "$src/x64/WinDivert.dll" "$src/x64/WinDivert64.sys" "$OUT/hyprclick/"
cp "$src/LICENSE" "$OUT/hyprclick/WinDivert-LICENSE.txt"
cp windows/README.txt "$OUT/hyprclick/README.txt"
(cd "$OUT" && rm -f hyprclick-windows-x64.zip && zip -qr hyprclick-windows-x64.zip hyprclick)
ls -l "$OUT/hyprclick" "$OUT/hyprclick-windows-x64.zip"
