| benchmark | unit | Citadel | Linux | Citadel vs Linux |
|---|---|---:|---:|---:|
| ctxswitch | ns | 1392.8 | 1764.2 | 1.27x faster |
| fork_exec_wait | ns | 148456.7 | 190473.2 | 1.28x faster |
| fork_exit_wait | ns | 36212.0 | 91627.2 | 2.53x faster |
| http_p50 | us | 434.0 | 554.0 | 1.28x faster |
| http_req_per_s | req/s | 69608.0 | 54982.0 | 1.27x faster |
| lua_fib | ms | 73.2 | 65.9 | 0.90x slower |
| lua_sieve | ms | 412.3 | 354.2 | 0.86x slower |
| lua_strings | ms | 460.6 | 482.3 | 1.05x faster |
| open_close | ns | 1021.3 | 1542.0 | 1.51x faster |
| pagefault | ns | 357.6 | 544.8 | 1.52x faster |
| pagefault_4k | ns | 1010.8 | 1435.9 | 1.42x faster |
| pipe_bw | ns/KiB | 160.8 | 429.1 | 2.67x faster |
| pipe_rtt_2cpu | ns | 29721.8 | 25170.6 | 0.85x slower |
| stat | ns | 753.1 | 902.5 | 1.20x faster |
| syscall | ns | 94.5 | 170.4 | 1.80x faster |
| tmpfs_write | ns/KiB | 65.6 | 332.2 | 5.06x faster |
