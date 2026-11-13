# Smoke test for the shell, the libc and the kernel's process, pipe and file system calls.
fail=0
t() { # name expected actual
    if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected '$2', got '$3'"; fail=$((fail+1)); fi
}
t echo "hello" "$(echo hello)"
t pipe "3" "$(seq 3 | wc -l)"
t pipeline-3 "2" "$(seq 10 | grep 1 | wc -l)"
t sort "3 2 1" "$(echo $(seq 3 | sort -r))"
mkdir -p /tmp/s/a/b
echo one > /tmp/s/f
echo two >> /tmp/s/f
t append "2" "$(wc -l < /tmp/s/f)"
cp /tmp/s/f /tmp/s/g
t cp "one" "$(head -n 1 /tmp/s/g)"
mv /tmp/s/g /tmp/s/a/h
t mv "two" "$(tail -n 1 /tmp/s/a/h)"
ln -s /tmp/s/f /tmp/s/link
t symlink "one" "$(head -n 1 /tmp/s/link)"
rm -r /tmp/s/a
t rm-r "no" "$(if [ -e /tmp/s/a ]; then echo yes; else echo no; fi)"
t arith "42" "$((6*7))"
i=0; while [ $i -lt 100 ]; do i=$((i+1)); done
t while "100" "$i"
t subshell "inner" "$( (echo inner) )"
t status "1" "$(false; echo $?)"
t and-or "y" "$(false && echo x || echo y)"
t glob "/tmp/s/f /tmp/s/link" "$(echo /tmp/s/*)"
sleep 0.1 &
wait
t bg-wait "0" "$?"
t stderr "err" "$(ls /nonexistent 2>&1 >/dev/null | grep -c x >/dev/null; echo err)"
n=0; for f in $(seq 50); do n=$((n+$(echo 1))); done
t forks "50" "$n"
t devnull "" "$(cat /dev/null)"
t uname "Citadel" "$(uname)"
echo "smoke: $fail failure(s)"
exit $fail
