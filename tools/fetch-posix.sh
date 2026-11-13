#!/bin/sh
# Downloads the Open POSIX Test Suite (part of the Linux Test Project release) into
# third_party/openposix (not kept in git), checking the archive's SHA-256.
set -eu
SRC=$(cd "$(dirname "$0")/.." && pwd)
DEST=$SRC/third_party
REL=20240930
SHA=3e9e806405aa602e2d21d221d885fd4914ff2da23eb8bba3d0f0f468bdf60238
TAR=$DEST/ltp-full-$REL.tar.xz
mkdir -p "$DEST"
if [ ! -s "$TAR" ]; then
    curl -fsSL --retry 5 -o "$TAR.tmp" "https://github.com/linux-test-project/ltp/releases/download/$REL/ltp-full-$REL.tar.xz"
    mv "$TAR.tmp" "$TAR"
fi
echo "$SHA  $TAR" | sha256sum -c --quiet - || { echo "checksum mismatch"; rm -f "$TAR"; exit 1; }
rm -rf "$DEST/openposix"
mkdir -p "$DEST/openposix"
tar -xJf "$TAR" -C "$DEST/openposix" --strip-components=3 "ltp-full-$REL/testcases/open_posix_testsuite"
echo "Open POSIX Test Suite in $DEST/openposix"
