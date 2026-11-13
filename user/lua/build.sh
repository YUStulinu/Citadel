#!/bin/sh
# Builds the official Lua 5.4 interpreter (third_party/lua, see tools/fetch-lua.sh) as a static
# program against Citadel's libc. Usage: user/lua/build.sh OUTDIR
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:-$root/build/user}
libc=${LIBC:-$(dirname "$out")/libc}
lua=$root/third_party/lua/src
mkdir -p "$out/lua-obj"
CC="zig cc -target x86_64-linux-none"
CFLAGS="-std=gnu99 -O2 -g -ffreestanding -fno-stack-protector -fno-pie -nostdlibinc -isystem $root/libc/include -DLUA_USE_POSIX -Wno-everything"
objs=""
for f in "$lua"/*.c; do
  case $(basename "$f") in luac.c) continue ;; esac
  o="$out/lua-obj/$(basename "$f" .c).o"
  $CC $CFLAGS -c "$f" -o "$o"
  objs="$objs $o"
done
$CC $objs "$libc/obj/arch.S.o" "$libc/libc.a" -nostdlib -static -no-pie -Wl,--gc-sections -o "$out/lua"
echo "lua: $out/lua"
