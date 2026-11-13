#!/bin/sh
# Runs one program inside Citadel and returns its exit status: builds the image with
# "init=/bin/init -- PROGRAM ARGS" on the kernel command line, boots it in QEMU and decodes the
# status that /bin/init passes to the debug-exit device when the program ends.
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
# isa-debug-exit: QEMU exits with (code << 1) | 1.
if [ $((status & 1)) -eq 1 ]; then
    code=$(((status - 1) / 2))
    echo "guest-test: $NAME exited with status $code"
    exit "$code"
fi
echo "guest-test: QEMU stopped with status $status (kernel panic or power-off failure)"
exit 3
