#!/bin/sh
# Downloads the official Lua 5.4 sources and test suite from lua.org into third_party/ (not
# kept in git) and checks them against known SHA-256 sums.
set -eu
SRC=$(cd "$(dirname "$0")/.." && pwd)
DEST=$SRC/third_party
VER=5.4.7
LUA_SHA=9fbf5e28ef86c69858f6d3d34eccc32e911c1a28b4120ff3e84aaa70cfbf1e30
TESTS_SHA=8a4898ffe4c7613c8009327a0722db7a41ef861d526c77c5b46114e59ebf811e
mkdir -p "$DEST"
get() { # url file sha
    if [ ! -s "$2" ]; then
        curl -fsSL --retry 5 --retry-all-errors -o "$2.tmp" "$1" || curl -fsSL --retry 5 -o "$2.tmp" "$(echo "$1" | sed 's|https://www|http://www|')"
        mv "$2.tmp" "$2"
    fi
    if [ -n "$3" ]; then
        echo "$3  $2" | sha256sum -c --quiet - || { echo "checksum mismatch for $2"; rm -f "$2"; exit 1; }
    else
        sha256sum "$2"
    fi
}
get "https://www.lua.org/ftp/lua-$VER.tar.gz" "$DEST/lua-$VER.tar.gz" "$LUA_SHA"
get "https://www.lua.org/tests/lua-$VER-tests.tar.gz" "$DEST/lua-$VER-tests.tar.gz" "$TESTS_SHA"
rm -rf "$DEST/lua" "$DEST/lua-tests"
mkdir -p "$DEST/lua" "$DEST/lua-tests"
tar -xzf "$DEST/lua-$VER.tar.gz" -C "$DEST/lua" --strip-components=1
tar -xzf "$DEST/lua-$VER-tests.tar.gz" -C "$DEST/lua-tests" --strip-components=1
echo "lua $VER in $DEST/lua, tests in $DEST/lua-tests"
