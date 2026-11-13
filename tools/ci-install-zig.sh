#!/bin/sh
# Installs Zig from ziglang.org (CI). Usage: tools/ci-install-zig.sh VERSION
set -eu
V=$1
dest=$HOME/.local/zig-$V
for name in "zig-x86_64-linux-$V" "zig-linux-x86_64-$V"; do
    if curl -fsSL --retry 5 --retry-all-errors -o /tmp/zig.tar.xz "https://ziglang.org/download/$V/$name.tar.xz"; then
        mkdir -p "$dest"
        tar -xJf /tmp/zig.tar.xz -C "$dest" --strip-components=1
        echo "$dest" >> "${GITHUB_PATH:-/dev/null}"
        "$dest/zig" version
        exit 0
    fi
done
echo "could not download Zig $V" >&2
exit 1
