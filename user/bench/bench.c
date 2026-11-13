/* Operating-system micro-benchmarks. The same static binary runs on Citadel and on Linux, so
   the two kernels are compared on identical user code in the same virtual machine.

   Each benchmark runs REPS times; the median is reported (min and max too). Output lines:
     bench <name> <median> <min> <max> <unit>
   Usage: bench [name...]   (no names = all) */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define REPS 7

static double now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e9 + ts.tv_nsec;
}

static long raw_syscall0(long n) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n) : "rcx", "r11", "memory");
    return r;
}

static void pin(int cpu) {
    cpu_set_t s;
    CPU_ZERO(&s);
    CPU_SET(cpu, &s);
    sched_setaffinity(0, sizeof s, &s);
}

static void unpin(void) {
    cpu_set_t s;
    memset(&s, 0xff, sizeof s);
    sched_setaffinity(0, sizeof s, &s);
}

/* ---------------------------------------------------------------- benchmarks (ns per op) */

static double b_syscall(void) {
    const int n = 1000000;
    double t0 = now_ns();
    for (int i = 0; i < n; i++) raw_syscall0(110); /* getppid: does no work in either kernel */
    return (now_ns() - t0) / n;
}

/* Two processes on one CPU bounce a byte through two pipes: every round trip is two switches. */
static double pingpong(int same_cpu) {
    const int n = 50000;
    int a[2], b[2];
    if (pipe(a) < 0 || pipe(b) < 0) return -1;
    if (same_cpu) pin(0);
    pid_t p = fork();
    if (p == 0) {
        if (!same_cpu) pin(1);
        char c;
        for (int i = 0; i < n; i++) {
            if (read(a[0], &c, 1) != 1) _exit(1);
            if (write(b[1], &c, 1) != 1) _exit(1);
        }
        _exit(0);
    }
    if (!same_cpu) pin(0);
    char c = 'x';
    double t0 = now_ns();
    for (int i = 0; i < n; i++) {
        write(a[1], &c, 1);
        read(b[0], &c, 1);
    }
    double t = now_ns() - t0;
    waitpid(p, NULL, 0);
    close(a[0]), close(a[1]), close(b[0]), close(b[1]);
    unpin();
    return same_cpu ? t / (2.0 * n) : t / n;
}

static double b_ctxswitch(void) { return pingpong(1); }
static double b_pipe_rtt_2cpu(void) { return pingpong(0); }

static double b_fork(void) {
    const int n = 2000;
    double t0 = now_ns();
    for (int i = 0; i < n; i++) {
        pid_t p = fork();
        if (p == 0) _exit(0);
        waitpid(p, NULL, 0);
    }
    return (now_ns() - t0) / n;
}

static double b_exec(void) {
    const int n = 500;
    double t0 = now_ns();
    for (int i = 0; i < n; i++) {
        pid_t p = fork();
        if (p == 0) {
            char *argv[] = {"true", NULL};
            execv("/bin/true", argv);
            _exit(127);
        }
        waitpid(p, NULL, 0);
    }
    return (now_ns() - t0) / n;
}

/* Bytes per microsecond = MB/s: reported as ns per KiB to keep "lower is better" everywhere. */
static double b_pipe_bw(void) {
    const size_t chunk = 65536, total = 256u << 20;
    int fds[2];
    if (pipe(fds) < 0) return -1;
    pid_t p = fork();
    if (p == 0) {
        close(fds[1]);
        char *buf = malloc(chunk);
        while (read(fds[0], buf, chunk) > 0) {}
        _exit(0);
    }
    close(fds[0]);
    char *buf = malloc(chunk);
    memset(buf, 'a', chunk);
    double t0 = now_ns();
    for (size_t done = 0; done < total;) {
        ssize_t w = write(fds[1], buf, chunk);
        if (w <= 0) break;
        done += w;
    }
    close(fds[1]);
    waitpid(p, NULL, 0);
    double t = now_ns() - t0;
    free(buf);
    return t / (total / 1024.0);
}

/* First touch of anonymous memory, per 4 KiB. Both kernels back large anonymous areas with
   2 MiB pages by default (transparent huge pages); pagefault_4k turns that off with madvise. */
static double touch_pages(int small) {
    const size_t len = 64u << 20;
    char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) return -1;
    if (small) madvise(m, len, MADV_NOHUGEPAGE);
    double t0 = now_ns();
    for (size_t i = 0; i < len; i += 4096) m[i] = 1;
    double t = now_ns() - t0;
    munmap(m, len);
    return t / (len / 4096);
}

static double b_pagefault(void) { return touch_pages(0); }
static double b_pagefault_4k(void) { return touch_pages(1); }

static double b_open_close(void) {
    const int n = 100000;
    int fd = open("/tmp/bench.f", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    close(fd);
    double t0 = now_ns();
    for (int i = 0; i < n; i++) close(open("/tmp/bench.f", O_RDONLY));
    double t = now_ns() - t0;
    unlink("/tmp/bench.f");
    return t / n;
}

static double b_stat(void) {
    const int n = 100000;
    struct stat st;
    double t0 = now_ns();
    for (int i = 0; i < n; i++) stat("/bin/sh", &st);
    return (now_ns() - t0) / n;
}

static double b_file_write(void) {
    const size_t chunk = 4096, total = 64u << 20;
    int fd = open("/tmp/bench.w", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    char *buf = malloc(chunk);
    memset(buf, 'b', chunk);
    double t0 = now_ns();
    for (size_t done = 0; done < total; done += chunk) write(fd, buf, chunk);
    double t = now_ns() - t0;
    close(fd);
    unlink("/tmp/bench.w");
    free(buf);
    return t / (total / 1024.0);
}

static const struct {
    const char *name, *unit;
    double (*fn)(void);
} benches[] = {
    {"syscall", "ns", b_syscall},
    {"ctxswitch", "ns", b_ctxswitch},
    {"pipe_rtt_2cpu", "ns", b_pipe_rtt_2cpu},
    {"fork_exit_wait", "ns", b_fork},
    {"fork_exec_wait", "ns", b_exec},
    {"pipe_bw", "ns/KiB", b_pipe_bw},
    {"pagefault", "ns", b_pagefault},
    {"pagefault_4k", "ns", b_pagefault_4k},
    {"open_close", "ns", b_open_close},
    {"stat", "ns", b_stat},
    {"tmpfs_write", "ns/KiB", b_file_write},
};

static int cmp_double(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

int main(int argc, char **argv) {
    for (unsigned i = 0; i < sizeof benches / sizeof *benches; i++) {
        int want = argc < 2;
        for (int k = 1; k < argc; k++)
            if (!strcmp(argv[k], benches[i].name)) want = 1;
        if (!want) continue;
        double r[REPS];
        benches[i].fn(); /* warm-up */
        for (int k = 0; k < REPS; k++) r[k] = benches[i].fn();
        qsort(r, REPS, sizeof *r, cmp_double);
        printf("bench %-16s %10.1f %10.1f %10.1f %s\n", benches[i].name, r[REPS / 2], r[0], r[REPS - 1], benches[i].unit);
        fflush(stdout);
    }
    return 0;
}
