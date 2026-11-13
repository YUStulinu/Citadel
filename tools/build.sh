#!/bin/sh
# Builds Citadel: the UEFI loader (Zig), the kernel (Rust), the C library and the user programs
# (zig cc), the image tools (Zig), the initrd and finally the bootable disk image.
#
#   OUT=build CMDLINE="console=ttyS0" tools/build.sh
#
# Result: $OUT/citadel.img (GPT: FAT32 EFI partition + ext2 partition from disk/).
set -eu
SRC=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$SRC/build}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
CMDLINE=${CMDLINE:-console=ttyS0}
ZCACHE="--cache-dir $OUT/zig-cache"

echo "== loader"
(cd "$SRC/boot" && zig build $ZCACHE --prefix "$OUT/boot")
echo "== kernel"
(cd "$SRC/kernel" && CARGO_TARGET_DIR="$OUT/target" cargo build --release --quiet)
echo "== libc and user programs"
sh "$SRC/libc/build.sh" "$OUT/libc" >/dev/null
sh "$SRC/user/build.sh" "$OUT/user" >/dev/null
if [ -d "$SRC/third_party/lua" ]; then sh "$SRC/user/lua/build.sh" "$OUT/user" >/dev/null; fi
echo "== image tools"
mkdir -p "$OUT/tools"
zig build-exe -O ReleaseSafe "$SRC/tools/mkinitrd.zig" -femit-bin="$OUT/tools/mkinitrd" $ZCACHE
zig build-exe -O ReleaseSafe "$SRC/tools/mkimage.zig" -femit-bin="$OUT/tools/mkimage" $ZCACHE

echo "== initrd"
R=$OUT/initrd-root
rm -rf "$R"
mkdir -p "$R/bin" "$R/etc" "$R/root" "$R/tests"
for p in init sh coreutils; do cp "$OUT/user/$p" "$R/bin/"; done
for t in $("$OUT/user/coreutils" --list); do ln -s coreutils "$R/bin/$t"; done
for f in "$OUT"/user/*; do
  case $(basename "$f") in
    init | sh | coreutils) ;;
    lua | httpd) cp "$f" "$R/bin/" ;;
    *) cp "$f" "$R/tests/" ;;
  esac
done
cp -r "$SRC/rootfs/." "$R/"
cp -r "$SRC/disk/www" "$R/www" # also in memory, for benchmarks that should not touch a disk
"$OUT/tools/mkinitrd" "$R" "$OUT/initrd.cpio"

echo "== disk image"
E=$OUT/esp
rm -rf "$E"
mkdir -p "$E/EFI/BOOT" "$E/CITADEL"
cp "$OUT/boot/bin/BOOTX64.efi" "$E/EFI/BOOT/BOOTX64.EFI"
cp "$OUT/target/x86_64-unknown-none/release/kernel" "$E/CITADEL/KERNEL.ELF"
cp "$OUT/initrd.cpio" "$E/CITADEL/INITRD"
printf '%s\n' "$CMDLINE" > "$E/CITADEL/CMDLINE"
D=$OUT/disk-root
rm -rf "$D"
mkdir -p "$D"
cp -r "$SRC/disk/." "$D/"
"$OUT/tools/mkimage" "$OUT/citadel.img" "$E" "$D" "${ESP_MIB:-64}" "${ROOT_MIB:-128}"
