#!/bin/sh
# Runs one program inside Citadel and returns its exit status: builds the image with
# "init=/bin/init -- PROGRAM ARGS" on the kernel command line, boots it in QEMU and decodes the
# status that /bin/init reports when the program ends.
#
#   OUT=build tools/guest-test.sh SECONDS PROGRAM [ARGS...]
#
# The serial log is kept in $OUT/guest-<name>.log. Environment: NET=1 for networking.
set -u
SRC=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$SRC/build}
LIMIT=$1
shift
NAME=$(basename "$1")
[ $# -gt 1 ] && NAME="$NAME-$(basename "$2")"
LOG=$OUT/guest-$NAME.log
CMDLINE="console=ttyS0 ${EXTRA_CMDLINE:-} init=/bin/init -- $*" OUT=$OUT sh "$SRC/tools/build.sh" >"$OUT/build-$NAME.log" 2>&1 || {
    echo "guest-test: build failed (see $OUT/build-$NAME.log)"
    tail -20 "$OUT/build-$NAME.log"
    exit 2
}
timeout "$LIMIT" sh "$SRC/tools/qemu-run.sh" "$OUT/citadel.img" </dev/null >"$LOG" 2>&1
status=$?
tr -d '\r' <"$LOG" | grep -av '^\[[0-9;]*[a-zA-Z]' | sed -n '/^citadel: booting/,$p'
if [ $status -eq 124 ]; then
    echo "guest-test: $NAME timed out after $LIMIT s"
    exit 124
fi
# /bin/init prints how the program ended, then powers off through isa-debug-exit (QEMU exits
# with (code << 1) | 1). Both must agree: a QEMU that failed to start also exits with 1, which
# must not count as "the program exited with 0".
reported=$(tr -d '\r' <"$LOG" | sed -n 's/^init: .* exited with status \([0-9]*\)$/\1/p' | tail -1)
if [ -z "$reported" ]; then
    echo "guest-test: $NAME never finished (QEMU status $status)"
    head -5 "$LOG"
    exit 3
fi
if [ $((status & 1)) -eq 1 ] && [ $(((status - 1) / 2)) -eq "$reported" ]; then
    echo "guest-test: $NAME exited with status $reported"
    exit "$reported"
fi
echo "guest-test: QEMU stopped with status $status (kernel panic or power-off failure)"
exit 3
