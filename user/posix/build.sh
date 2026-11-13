#!/bin/sh
# Builds the selected Open POSIX Test Suite tests (third_party/openposix, see
# tools/fetch-posix.sh) as static programs against Citadel's libc.
# Usage: user/posix/build.sh OUTDIR   -> OUTDIR/posix/<interface>.<test>, OUTDIR/posix-build.log
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$root/build/user}
libc=${LIBC:-$(dirname "$out")/libc}
ops=$root/third_party/openposix
CC="zig cc -target x86_64-linux-none"
CFLAGS="-std=gnu99 -O1 -ffreestanding -fno-stack-protector -fno-pie -nostdlibinc -isystem $root/libc/include -I$ops/include -Wno-everything"
rm -rf "$out/posix"
mkdir -p "$out/posix"
log=$out/posix-build.log
: > "$log"
built=0
failed=0
for iface in $(grep -v '^#' "$here/interfaces.txt"); do
    for src in "$ops/conformance/interfaces/$iface"/*.c; do
        name=$(basename "$src" .c)
        case $name in [0-9]*-[0-9]* | [0-9]*-buildonly) ;; *) continue ;; esac
        bin="$out/posix/$iface.$name"
        if $CC $CFLAGS "$src" "$libc/obj/arch.S.o" "$libc/libc.a" -nostdlib -static -no-pie -s -Wl,--gc-sections -o "$bin" >>"$log" 2>&1; then
            built=$((built + 1))
        else
            echo "BUILD-FAILED $iface.$name" >>"$log"
            failed=$((failed + 1))
        fi
    done
done
echo "posix: $built built, $failed failed to build (see $log)"
