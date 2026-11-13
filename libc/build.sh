#!/bin/sh
# Build libc.a (and crt in the archive) with zig cc. Usage: libc/build.sh OUTDIR
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/../build/libc}
mkdir -p "$out/obj"
CC="zig cc -target x86_64-linux-none"
CFLAGS="-std=c11 -O2 -g -ffreestanding -fno-builtin -fno-stack-protector -fno-pie -nostdlibinc -isystem $here/include -Wall -Wextra -Wno-unused-parameter -Wno-sign-compare"
objs=""
for src in "$here"/src/*.c "$here"/src/*.S; do
    obj="$out/obj/$(basename "$src").o"
    $CC $CFLAGS -c "$src" -o "$obj"
    objs="$objs $obj"
done
rm -f "$out/libc.a"
zig ar rcs "$out/libc.a" $objs
echo "libc: $out/libc.a"
