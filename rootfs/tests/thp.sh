grep -i huge /proc/meminfo
grep thp_fault_alloc /proc/vmstat
/tests/bench pagefault
grep thp_fault_alloc /proc/vmstat
