#!/bin/sh
# Boots Citadel in QEMU with KVM: OVMF firmware, the disk image made by tools/mkimage.zig as a
# virtio block device (the firmware boots its EFI partition; the kernel mounts its ext2 partition
# on /mnt), COM1 on stdio.
#   tools/qemu-run.sh DISK_IMAGE [extra qemu args]
# Environment: SMP (CPUs, 4), MEM (MiB, 1024), NET=1 adds a virtio network card with QEMU's
# user-mode network and host port HOSTPORT (8080) forwarded to port 80 of the guest.
IMG=$1; shift
VARS=${VARS:-/tmp/citadel-ovmf-vars.fd}
[ -f "$VARS" ] || cp /usr/share/OVMF/OVMF_VARS_4M.fd "$VARS"
NETARGS=""
if [ "${NET:-0}" = 1 ]; then
  NETARGS="-netdev user,id=n0,hostfwd=tcp:127.0.0.1:${HOSTPORT:-8080}-:80 -device virtio-net-pci,netdev=n0"
fi
ACCEL="-enable-kvm -cpu host"
[ -e /dev/kvm ] || ACCEL="-cpu max"
exec qemu-system-x86_64 -machine q35 $ACCEL -smp "${SMP:-4}" -m "${MEM:-1024}" \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file="$VARS" \
  -drive if=none,id=d0,format=raw,file="$IMG" -device virtio-blk-pci,drive=d0,bootindex=1 \
  $NETARGS \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  ${MONITOR:+-monitor unix:$MONITOR,server,nowait} \
  -display none -serial stdio -no-reboot "$@"
