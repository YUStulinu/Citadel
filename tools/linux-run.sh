#!/bin/sh
# Boots the Linux baseline: Alpine's "virt" kernel in the same QEMU/KVM machine as Citadel
# (q35, same CPU count and memory), with Citadel's own initrd, so /bin/init and the test and
# benchmark programs are the very same static binaries.
# Usage: tools/linux-run.sh INITRD "program args"   (env: SMP, MEM, CACHE)
set -eu
INITRD=$1
CMD=$2
CACHE=${CACHE:-$HOME/.cache/citadel}
ALPINE=v3.20
KERNEL=$CACHE/vmlinuz-virt-$ALPINE
mkdir -p "$CACHE"
if [ ! -s "$KERNEL" ]; then
    curl -fsSL -o "$KERNEL.tmp" "https://dl-cdn.alpinelinux.org/alpine/$ALPINE/releases/x86_64/netboot/vmlinuz-virt"
    mv "$KERNEL.tmp" "$KERNEL"
fi
exec qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -smp "${SMP:-4}" -m "${MEM:-1024}" \
  -kernel "$KERNEL" -initrd "$INITRD" \
  -append "console=ttyS0 quiet rdinit=/bin/init -- $CMD" \
  -display none -serial stdio -no-reboot
