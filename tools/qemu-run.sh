#!/bin/sh
# Boots Citadel in QEMU with KVM: OVMF firmware, an ESP built from a directory (QEMU's vvfat),
# COM1 on stdio. Usage: tools/qemu-run.sh ESP_DIR [extra qemu args]
ESP=$1; shift
VARS=/tmp/citadel-ovmf-vars.fd
[ -f $VARS ] || cp /usr/share/OVMF/OVMF_VARS_4M.fd $VARS
exec qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -smp ${SMP:-4} -m ${MEM:-1024} \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=$VARS \
  -drive format=raw,file=fat:rw:$ESP \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -display none -serial stdio -no-reboot "$@"
