/* /bin/init: runs /etc/rc, then keeps a shell on the console and reaps orphans.
   With arguments (init=/bin/init cmdline: "init /bin/prog args"), runs that program once and
   powers the machine off when it exits, which is how automated test runs work. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define CITADEL_POWEROFF 1001

static long sys6(long n, long a, long b, long c, long d, long e) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8) : "rcx", "r11", "memory");
    return r;
}

/* Citadel: its own power-off call (QEMU's debug-exit device carries the status out).
   Linux (the comparison baseline runs this same init): reboot(POWER_OFF). */
static long sys_poweroff(int code) {
    long r = sys6(CITADEL_POWEROFF, code, 0, 0, 0, 0);
    sync();
    sys6(169, 0xfee1dead, 672274793, 0x4321fedc, 0, 0);
    return r;
}

/* Linux interface set-up with the classic ioctls: address, netmask, up. */
struct ifreq_min {
    char name[16];
    union {
        struct {
            unsigned short family, port;
            unsigned addr;
            char zero[8];
        } sin;
        short flags;
        char pad[24];
    } u;
};

static void if_config(int s, const char *name, unsigned addr_be, unsigned mask_be) {
    struct ifreq_min r;
    memset(&r, 0, sizeof r);
    strncpy(r.name, name, 15);
    r.u.sin.family = 2;
    r.u.sin.addr = addr_be;
    sys6(16, s, 0x8916, (long)&r, 0, 0); /* SIOCSIFADDR */
    r.u.sin.addr = mask_be;
    sys6(16, s, 0x891c, (long)&r, 0, 0); /* SIOCSIFNETMASK */
    memset(&r.u, 0, sizeof r.u);
    r.u.flags = 0x1 | 0x40;              /* IFF_UP | IFF_RUNNING */
    sys6(16, s, 0x8914, (long)&r, 0, 0); /* SIOCSIFFLAGS */
}

/* Linux baseline only: load the virtio network modules shipped in the initrd (Alpine builds
   them as modules) and configure lo and eth0 like QEMU's user network expects. */
static void linux_net(void) {
    static const char *const mods[] = {"/lib/modules/failover.ko", "/lib/modules/net_failover.ko", "/lib/modules/virtio_net.ko"};
    for (unsigned i = 0; i < sizeof mods / sizeof *mods; i++) {
        int fd = open(mods[i], O_RDONLY);
        if (fd < 0) continue;
        sys6(313, fd, (long)"", 0, 0, 0); /* finit_module */
        close(fd);
    }
    int s = (int)sys6(41, 2, 2, 0, 0, 0);
    if (s < 0) return;
    if_config(s, "lo", 0x0100007f, 0x000000ff);
    if_config(s, "eth0", 0x0f02000a, 0x00ffffff); /* 10.0.2.15/24, little-endian bytes */
    close(s);
}

/* On Linux the kernel does not mount anything for us. */
static void linux_mounts(void) {
    if (access("/proc/self", F_OK) == 0) return;
    mkdir("/proc", 0555);
    mkdir("/dev", 0755);
    mkdir("/tmp", 01777);
    sys6(165, (long)"proc", (long)"/proc", (long)"proc", 0, 0);
    sys6(165, (long)"devtmpfs", (long)"/dev", (long)"devtmpfs", 0, 0);
    sys6(165, (long)"tmpfs", (long)"/tmp", (long)"tmpfs", 0, 0);
    mkdir("/sys", 0555);
    sys6(165, (long)"sysfs", (long)"/sys", (long)"sysfs", 0, 0);
    linux_net();
}

static pid_t spawn(char *const argv[], int fg) {
    pid_t p = fork();
    if (p < 0) {
        perror("init: fork");
        return -1;
    }
    if (p == 0) {
        setsid();
        setpgid(0, 0);
        if (fg) tcsetpgrp(0, getpid());
        signal(SIGINT, SIG_DFL);
        signal(SIGQUIT, SIG_DFL);
        signal(SIGTSTP, SIG_DFL);
        execv(argv[0], argv);
        fprintf(stderr, "init: cannot run %s: %s\n", argv[0], strerror(errno));
        _exit(127);
    }
    return p;
}

static int wait_for(pid_t want) {
    for (;;) {
        int st;
        pid_t p = wait(&st);
        if (p < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (p == want) return st;
        /* Anything else is an orphan reparented to us: just reap it. */
    }
}

int main(int argc, char **argv) {
    linux_mounts();
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);
    signal(SIGTSTP, SIG_IGN);
    signal(SIGTTOU, SIG_IGN);
    if (argc > 1) {
        pid_t p = spawn(argv + 1, 1);
        int st = wait_for(p);
        int code = WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
        tcsetpgrp(0, getpid());
        printf("init: %s exited with status %d\n", argv[1], code);
        fflush(stdout);
        sys_poweroff(code);
        return code;
    }
    printf("\nCitadel userland is up. Type 'help' for the built-in commands.\n");
    if (access("/etc/rc", R_OK) == 0) {
        char *rc[] = {"/bin/sh", "/etc/rc", NULL};
        wait_for(spawn(rc, 1));
    }
    for (;;) {
        char *sh[] = {"/bin/sh", "-i", NULL};
        pid_t p = spawn(sh, 1);
        if (p < 0) {
            sleep(1);
            continue;
        }
        int st = wait_for(p);
        tcsetpgrp(0, getpid());
        if (WIFEXITED(st) && WEXITSTATUS(st) == 0) {
            printf("init: shell exited; powering off\n");
            fflush(stdout);
            sys_poweroff(0);
        }
        printf("init: shell died (status %d); restarting\n", st);
    }
}
