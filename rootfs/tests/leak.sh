# Memory must come back after processes exit: free memory before and after 2000 fork+exec+exit.
set -- $(grep MemFree /proc/meminfo)
before=$2
i=0
while [ $i -lt 2000 ]; do /bin/true; i=$((i+1)); done
set -- $(grep MemFree /proc/meminfo)
after=$2
echo "free before $before kB, after $after kB, lost $((before - after)) kB"
[ $((before - after)) -lt 4096 ]
