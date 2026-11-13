/* Internal: raw system calls (Linux x86-64 convention, which Citadel follows). */
#ifndef _LIBC_SYSCALL_H
#define _LIBC_SYSCALL_H
#include <errno.h>

static inline long __syscall6(long n, long a, long b, long c, long d, long e, long f) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return r;
}

#define __sc0(n) __syscall6((n), 0, 0, 0, 0, 0, 0)
#define __sc1(n, a) __syscall6((n), (long)(a), 0, 0, 0, 0, 0)
#define __sc2(n, a, b) __syscall6((n), (long)(a), (long)(b), 0, 0, 0, 0)
#define __sc3(n, a, b, c) __syscall6((n), (long)(a), (long)(b), (long)(c), 0, 0, 0)
#define __sc4(n, a, b, c, d) __syscall6((n), (long)(a), (long)(b), (long)(c), (long)(d), 0, 0)
#define __sc5(n, a, b, c, d, e) __syscall6((n), (long)(a), (long)(b), (long)(c), (long)(d), (long)(e), 0)
#define __sc6(n, a, b, c, d, e, f) __syscall6((n), (long)(a), (long)(b), (long)(c), (long)(d), (long)(e), (long)(f))

/* -errno results become -1 with errno set. */
static inline long __ret(long r) {
    if (r < 0 && r > -4096) {
        errno = (int)-r;
        return -1;
    }
    return r;
}

enum {
    SYS_read = 0, SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_stat = 4, SYS_fstat = 5, SYS_lstat = 6,
    SYS_poll = 7, SYS_lseek = 8, SYS_mmap = 9, SYS_mprotect = 10, SYS_munmap = 11, SYS_brk = 12,
    SYS_rt_sigaction = 13, SYS_rt_sigprocmask = 14, SYS_ioctl = 16, SYS_pread64 = 17, SYS_pwrite64 = 18,
    SYS_readv = 19, SYS_writev = 20, SYS_access = 21, SYS_pipe = 22, SYS_select = 23, SYS_sched_yield = 24, SYS_madvise = 28,
    SYS_dup = 32, SYS_dup2 = 33, SYS_pause = 34, SYS_nanosleep = 35, SYS_alarm = 37, SYS_getpid = 39,
    SYS_socket = 41, SYS_connect = 42, SYS_accept = 43, SYS_sendto = 44, SYS_recvfrom = 45, SYS_shutdown = 48,
    SYS_bind = 49, SYS_listen = 50, SYS_getsockname = 51, SYS_getpeername = 52, SYS_setsockopt = 54,
    SYS_getsockopt = 55, SYS_fork = 57, SYS_vfork = 58, SYS_execve = 59, SYS_exit = 60, SYS_wait4 = 61,
    SYS_kill = 62, SYS_uname = 63, SYS_fcntl = 72, SYS_fsync = 74, SYS_truncate = 76, SYS_ftruncate = 77,
    SYS_getcwd = 79, SYS_chdir = 80, SYS_fchdir = 81, SYS_rename = 82, SYS_mkdir = 83, SYS_rmdir = 84,
    SYS_link = 86, SYS_unlink = 87, SYS_symlink = 88, SYS_readlink = 89, SYS_chmod = 90, SYS_fchmod = 91,
    SYS_chown = 92, SYS_umask = 95, SYS_gettimeofday = 96, SYS_getrlimit = 97, SYS_getrusage = 98,
    SYS_times = 100, SYS_getuid = 102, SYS_getgid = 104, SYS_setuid = 105, SYS_setgid = 106,
    SYS_geteuid = 107, SYS_getegid = 108, SYS_setpgid = 109, SYS_getppid = 110, SYS_getpgrp = 111,
    SYS_setsid = 112, SYS_getpgid = 121, SYS_getsid = 124, SYS_rt_sigpending = 127, SYS_rt_sigsuspend = 130,
    SYS_mknod = 133, SYS_statfs = 137, SYS_mount = 165, SYS_reboot = 169, SYS_setrlimit = 160, SYS_sync = 162, SYS_gettid = 186,
    SYS_sched_setaffinity = 203, SYS_sched_getaffinity = 204, SYS_getdents64 = 217, SYS_clock_gettime = 228, SYS_clock_getres = 229, SYS_clock_nanosleep = 230,
    SYS_exit_group = 231, SYS_openat = 257, SYS_mkdirat = 258, SYS_newfstatat = 262, SYS_unlinkat = 263,
    SYS_utimensat = 280, SYS_accept4 = 288, SYS_pipe2 = 293, SYS_getrandom = 318,
};
#endif
