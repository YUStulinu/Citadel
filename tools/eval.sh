#!/bin/sh
# Reproduces the evaluation: Citadel and Linux (Alpine's kernel) in the same QEMU/KVM machine
# running the very same static binaries, alternating ROUNDS times to spread out host noise.
#   OUT=build ROUNDS=3 tools/eval.sh   ->  results/eval.md (table) and results/raw/ (logs)
set -u
SRC=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$SRC/build}
ROUNDS=${ROUNDS:-3}
RES=$SRC/results
RAW=$RES/raw
mkdir -p "$RAW"
: > "$RAW/samples.txt"   # system bench value

note() { echo "eval: $*" >&2; }

# 1. Micro-benchmarks and the Lua workload.
OUT=$OUT CMDLINE="console=ttyS0 init=/bin/init -- /bin/sh /tests/benchall.sh" sh "$SRC/tools/build.sh" >"$RAW/build.log" 2>&1 || { note "build failed"; exit 1; }
for r in $(seq 1 "$ROUNDS"); do
  note "round $r: micro-benchmarks"
  timeout 900 sh "$SRC/tools/qemu-run.sh" "$OUT/citadel.img" </dev/null >"$RAW/bench-citadel-$r.log" 2>&1
  timeout 900 sh "$SRC/tools/linux-run.sh" "$OUT/initrd.cpio" "/bin/sh /tests/benchall.sh" </dev/null >"$RAW/bench-linux-$r.log" 2>&1
  for sys in citadel linux; do
    tr -d '\r' <"$RAW/bench-$sys-$r.log" | awk -v s=$sys '$1=="bench" {print s, $2, $3, $NF}' >>"$RAW/samples.txt"
  done
done

# 2. HTTP: the same httpd serving the same page from memory, load from the host.
gcc -O2 -o "$OUT/tools/httpload" "$SRC/tools/httpload.c"
OUT=$OUT CMDLINE="console=ttyS0 init=/bin/init -- /bin/httpd -r /www -q" sh "$SRC/tools/build.sh" >>"$RAW/build.log" 2>&1
wait_up() { for i in $(seq 1 120); do curl -s -m 1 -o /dev/null http://127.0.0.1:8080/ && return 0; sleep 0.5; done; return 1; }
stop_vm() { kill "$1" 2>/dev/null; wait "$1" 2>/dev/null; }
for r in $(seq 1 "$ROUNDS"); do
  for sys in citadel linux; do
    note "round $r: HTTP on $sys"
    if [ $sys = citadel ]; then
      NET=1 sh "$SRC/tools/qemu-run.sh" "$OUT/citadel.img" </dev/null >"$RAW/http-$sys-$r.log" 2>&1 &
    else
      NET=1 sh "$SRC/tools/linux-run.sh" "$OUT/initrd.cpio" "/bin/httpd -r /www -q" </dev/null >"$RAW/http-$sys-$r.log" 2>&1 &
    fi
    vm=$!
    if wait_up; then
      "$OUT/tools/httpload" 127.0.0.1 8080 /index.html 32 10 | tee -a "$RAW/http-$sys-$r.log" |
        awk -v s=$sys '{for (i=1;i<=NF;i++) { if ($i=="req/s,") print s, "http_req_per_s", $(i-1), "req/s"; if ($i=="p50") print s, "http_p50", $(i+1), "us" }}' >>"$RAW/samples.txt"
    else
      note "$sys: server did not come up"
    fi
    pkill -P $vm 2>/dev/null; stop_vm $vm
    sleep 2
  done
done

# 3. Table: median over rounds; ratio Linux/Citadel for "lower is better" units, inverse for req/s.
{
  echo "| benchmark | unit | Citadel | Linux | Citadel vs Linux |"
  echo "|---|---|---:|---:|---:|"
  awk '{k=$2" "$4; v[$1" "k]=v[$1" "k]" "$3; keys[k]=1}
       END {for (k in keys) print k, v["citadel "k], "|", v["linux "k]}' "$RAW/samples.txt" | sort |
  awk -F'|' '
    function median(s,   a, n, i, j, t) { n = split(s, a, " "); for (i=1;i<=n;i++) for (j=i+1;j<=n;j++) if (a[j]+0 < a[i]+0) {t=a[i]; a[i]=a[j]; a[j]=t}
      return n ? (n%2 ? a[(n+1)/2] : (a[n/2]+a[n/2+1])/2) : "" }
    { split($1, h, " "); name=h[1]; unit=h[2]; c=""; for (i=3;i<=length(h);i++) c=c" "h[i];
      cm = median(c); lm = median($2);
      if (cm == "" || lm == "") next;
      better = (unit == "req/s") ? cm/lm : lm/cm;
      printf "| %s | %s | %.1f | %.1f | %.2fx %s |\n", name, unit, cm, lm, better, (better >= 1 ? "faster" : "slower") }'
} >"$RES/eval.md"
cat "$RES/eval.md"
