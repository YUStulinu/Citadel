#!/bin/sh
# Build the user programs as static Linux x86-64 ELF files against Citadel's libc.
# Usage: user/build.sh OUTDIR   (expects OUTDIR/../libc/libc.a from libc/build.sh)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out=${1:-$root/build/user}
libc=${LIBC:-$(dirname "$out")/libc}
mkdir -p "$out"
CC="zig cc -target x86_64-linux-none"
CFLAGS="-std=gnu11 -O2 -g -ffreestanding -fno-stack-protector -fno-pie -nostdlibinc -isystem $root/libc/include -Wall -Wno-unused-parameter -Wno-sign-compare"
LDFLAGS="-nostdlib -static -no-pie -Wl,--gc-sections"
link() { # name sources...
    name=$1
    shift
    $CC $CFLAGS "$@" "$libc/obj/arch.S.o" "$libc/libc.a" $LDFLAGS -o "$out/$name"
}
link init "$here/init.c"
link sh "$here/sh.c"
link coreutils "$here/coreutils.c"
for t in "$here"/tests/*.c; do
    [ -e "$t" ] || continue
    link "$(basename "$t" .c)" "$t"
done
for b in "$here"/bench/*.c; do
    [ -e "$b" ] || continue
    link "$(basename "$b" .c)" "$b"
done
echo "user: $out"
