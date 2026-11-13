#!/bin/sh
# Boots the Linux baseline: Alpine's "virt" kernel in the same QEMU/KVM machine as Citadel
# (q35, same CPU count and memory), with Citadel's own initrd, so /bin/init and the test and
# benchmark programs are the very same static binaries.
#
# With NET=1 the machine gets the same virtio network card as Citadel. Alpine builds virtio_net
# as a module, so its modules (and their dependencies) are taken from the modloop image of the
# same netboot release and appended to the initrd as a second cpio archive; /bin/init loads
# them and configures the interfaces when it runs on Linux.
#
# Usage: tools/linux-run.sh INITRD "program args"   (env: SMP, MEM, CACHE, NET=1, HOSTPORT)
set -eu
SRC=$(cd "$(dirname "$0")/.." && pwd)
INITRD=$1
CMD=$2
CACHE=${CACHE:-$HOME/.cache/citadel}
ALPINE=v3.20
URL=https://dl-cdn.alpinelinux.org/alpine/$ALPINE/releases/x86_64/netboot
KERNEL=$CACHE/vmlinuz-virt-$ALPINE
mkdir -p "$CACHE"
fetch() { # url file
    [ -s "$2" ] && return 0
    curl -fsSL -o "$2.tmp" "$1"
    mv "$2.tmp" "$2"
}
fetch "$URL/vmlinuz-virt" "$KERNEL"
NETARGS=""
BOOT_INITRD=$INITRD
if [ "${NET:-0}" = 1 ]; then
    NETARGS="-netdev user,id=n0,hostfwd=tcp:127.0.0.1:${HOSTPORT:-8080}-:80 -device virtio-net-pci,netdev=n0"
    MODS=$CACHE/linux-modules-$ALPINE
    if [ ! -f "$MODS.cpio" ]; then
        fetch "$URL/modloop-virt" "$CACHE/modloop-virt-$ALPINE"
        rm -rf "$MODS.x" "$MODS"
        unsquashfs -q -d "$MODS.x" "$CACHE/modloop-virt-$ALPINE" >/dev/null
        mkdir -p "$MODS/lib/modules"
        for m in net/core/failover drivers/net/net_failover drivers/net/virtio_net; do
            f=$(find "$MODS.x" -path "*/kernel/$m.ko*" | head -1)
            case $f in
                *.gz) gzip -dc "$f" > "$MODS/lib/modules/$(basename "$m").ko" ;;
                *.xz) xz -dc "$f" > "$MODS/lib/modules/$(basename "$m").ko" ;;
                *.zst) zstd -qdc "$f" > "$MODS/lib/modules/$(basename "$m").ko" ;;
                *) cp "$f" "$MODS/lib/modules/$(basename "$m").ko" ;;
            esac
        done
        rm -rf "$MODS.x"
        MKINITRD=${MKINITRD:-$(dirname "$INITRD")/tools/mkinitrd}
        "$MKINITRD" "$MODS" "$MODS.cpio" >/dev/null 2>&1
    fi
    BOOT_INITRD=$CACHE/initrd-with-modules.cpio
    cat "$INITRD" "$MODS.cpio" > "$BOOT_INITRD"
fi
exec qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -smp "${SMP:-4}" -m "${MEM:-1024}" \
  -kernel "$KERNEL" -initrd "$BOOT_INITRD" $NETARGS \
  -append "console=ttyS0 quiet rdinit=/bin/init -- $CMD" \
  -display none -serial stdio -no-reboot
