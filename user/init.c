/* /bin/init: runs /etc/rc, then keeps a shell on the console and reaps orphans.
   With arguments (init=/bin/init cmdline: "init /bin/prog args"), runs that program once and
   powers the machine off when it exits, which is how automated test runs work. */
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define CITADEL_POWEROFF 1001

static long sys_poweroff(int code) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"((long)CITADEL_POWEROFF), "D"((long)code) : "rcx", "r11", "memory");
    return r;
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
