/* System-call wrappers: unistd.h, fcntl.h, sys/stat.h, sys/wait.h, sys/mman.h, sys/ioctl.h,
   sys/socket.h and friends. */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/time.h>
#include <sys/times.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <sched.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include "syscall.h"

int errno;

ssize_t read(int fd, void *buf, size_t n) { return __ret(__sc3(SYS_read, fd, buf, n)); }
ssize_t write(int fd, const void *buf, size_t n) { return __ret(__sc3(SYS_write, fd, buf, n)); }
ssize_t pread(int fd, void *buf, size_t n, off_t off) { return __ret(__sc4(SYS_pread64, fd, buf, n, off)); }
ssize_t pwrite(int fd, const void *buf, size_t n, off_t off) { return __ret(__sc4(SYS_pwrite64, fd, buf, n, off)); }
int close(int fd) { return __ret(__sc1(SYS_close, fd)); }
off_t lseek(int fd, off_t off, int whence) { return __ret(__sc3(SYS_lseek, fd, off, whence)); }

int open(const char *path, int flags, ...) {
    va_list ap;
    va_start(ap, flags);
    int mode = va_arg(ap, int);
    va_end(ap);
    return __ret(__sc4(SYS_openat, AT_FDCWD, path, flags, mode));
}

int openat(int dirfd, const char *path, int flags, ...) {
    va_list ap;
    va_start(ap, flags);
    int mode = va_arg(ap, int);
    va_end(ap);
    return __ret(__sc4(SYS_openat, dirfd, path, flags, mode));
}

int creat(const char *path, mode_t mode) { return open(path, O_WRONLY | O_CREAT | O_TRUNC, mode); }

int fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);
    long arg = va_arg(ap, long);
    va_end(ap);
    return __ret(__sc3(SYS_fcntl, fd, cmd, arg));
}

int ioctl(int fd, unsigned long req, ...) {
    va_list ap;
    va_start(ap, req);
    long arg = va_arg(ap, long);
    va_end(ap);
    return __ret(__sc3(SYS_ioctl, fd, req, arg));
}

int pipe(int fds[2]) { return __ret(__sc2(SYS_pipe2, fds, 0)); }
int pipe2(int fds[2], int flags) { return __ret(__sc2(SYS_pipe2, fds, flags)); }
int dup(int fd) { return __ret(__sc1(SYS_dup, fd)); }
int dup2(int a, int b) { return __ret(__sc2(SYS_dup2, a, b)); }
pid_t fork(void) { return __ret(__sc0(SYS_fork)); }
pid_t vfork(void) { return fork(); }
int execve(const char *path, char *const argv[], char *const envp[]) { return __ret(__sc3(SYS_execve, path, argv, envp)); }
int execv(const char *path, char *const argv[]) { return execve(path, argv, environ); }

int execvp(const char *file, char *const argv[]) {
    if (strchr(file, '/')) return execv(file, argv);
    const char *path = getenv("PATH");
    if (!path) path = "/bin:/usr/bin";
    char buf[512];
    int saved = ENOENT;
    while (*path) {
        const char *e = strchr(path, ':');
        size_t l = e ? (size_t)(e - path) : strlen(path);
        if (l + strlen(file) + 2 < sizeof buf) {
            memcpy(buf, path, l);
            buf[l] = '/';
            strcpy(buf + l + 1, file);
            execv(buf, argv);
            if (errno != ENOENT) saved = errno;
        }
        path += l;
        if (*path == ':') path++;
    }
    errno = saved;
    return -1;
}

static int execl_common(const char *path, const char *arg, va_list ap, int search) {
    char *argv[64];
    int n = 0;
    argv[n++] = (char *)arg;
    while (n < 63 && (argv[n] = va_arg(ap, char *))) n++;
    argv[n] = 0;
    return search ? execvp(path, argv) : execv(path, argv);
}

int execl(const char *path, const char *arg, ...) {
    va_list ap;
    va_start(ap, arg);
    int r = execl_common(path, arg, ap, 0);
    va_end(ap);
    return r;
}

int execlp(const char *file, const char *arg, ...) {
    va_list ap;
    va_start(ap, arg);
    int r = execl_common(file, arg, ap, 1);
    va_end(ap);
    return r;
}

_Noreturn void _exit(int code) {
    for (;;) __sc1(SYS_exit_group, code);
}

pid_t getpid(void) { return __sc0(SYS_getpid); }
pid_t getppid(void) { return __sc0(SYS_getppid); }
pid_t getpgrp(void) { return __sc0(SYS_getpgrp); }
pid_t getpgid(pid_t p) { return __ret(__sc1(SYS_getpgid, p)); }
int setpgid(pid_t p, pid_t g) { return __ret(__sc2(SYS_setpgid, p, g)); }
pid_t setsid(void) { return __ret(__sc0(SYS_setsid)); }
pid_t getsid(pid_t p) { return __ret(__sc1(SYS_getsid, p)); }
uid_t getuid(void) { return __sc0(SYS_getuid); }
uid_t geteuid(void) { return __sc0(SYS_geteuid); }
gid_t getgid(void) { return __sc0(SYS_getgid); }
gid_t getegid(void) { return __sc0(SYS_getegid); }
int setuid(uid_t u) { return __ret(__sc1(SYS_setuid, u)); }
int setgid(gid_t g) { return __ret(__sc1(SYS_setgid, g)); }

pid_t tcgetpgrp(int fd) {
    int pg;
    if (ioctl(fd, TIOCGPGRP, &pg) < 0) return -1;
    return pg;
}
int tcsetpgrp(int fd, pid_t pg) { return ioctl(fd, TIOCSPGRP, &pg); }
int tcgetattr(int fd, struct termios *t) { return ioctl(fd, TCGETS, t); }
int tcsetattr(int fd, int act, const struct termios *t) { return ioctl(fd, TCSETS + act, t); }
int isatty(int fd) {
    struct termios t;
    return tcgetattr(fd, &t) == 0;
}
char *ttyname(int fd) { return isatty(fd) ? "/dev/tty" : NULL; }

int chdir(const char *p) { return __ret(__sc1(SYS_chdir, p)); }
int fchdir(int fd) { return __ret(__sc1(SYS_fchdir, fd)); }
char *getcwd(char *buf, size_t n) {
    char tmp[4096];
    if (!buf) {
        buf = tmp;
        n = sizeof tmp;
    }
    if (__ret(__sc2(SYS_getcwd, buf, n)) < 0) return NULL;
    return buf == tmp ? strdup(tmp) : buf;
}
int access(const char *p, int m) { return __ret(__sc2(SYS_access, p, m)); }
int unlink(const char *p) { return __ret(__sc1(SYS_unlink, p)); }
int rmdir(const char *p) { return __ret(__sc1(SYS_rmdir, p)); }
int link(const char *a, const char *b) { return __ret(__sc2(SYS_link, a, b)); }
int symlink(const char *a, const char *b) { return __ret(__sc2(SYS_symlink, a, b)); }
ssize_t readlink(const char *restrict p, char *restrict b, size_t n) { return __ret(__sc3(SYS_readlink, p, b, n)); }
int chown(const char *p, uid_t u, gid_t g) { return __ret(__sc3(SYS_chown, p, u, g)); }
int truncate(const char *p, off_t l) { return __ret(__sc2(SYS_truncate, p, l)); }
int ftruncate(int fd, off_t l) { return __ret(__sc2(SYS_ftruncate, fd, l)); }
int fsync(int fd) { return __ret(__sc1(SYS_fsync, fd)); }
void sync(void) { __sc0(SYS_sync); }
int rename(const char *a, const char *b) { return __ret(__sc2(SYS_rename, a, b)); }
int mkdir(const char *p, mode_t m) { return __ret(__sc2(SYS_mkdir, p, m)); }
int mkfifo(const char *p, mode_t m) { return __ret(__sc3(SYS_mknod, p, S_IFIFO | (m & 07777), 0)); }
int mknod(const char *p, mode_t m, dev_t d) { return __ret(__sc3(SYS_mknod, p, m, d)); }
int chmod(const char *p, mode_t m) { return __ret(__sc2(SYS_chmod, p, m)); }
int fchmod(int fd, mode_t m) { return __ret(__sc2(SYS_fchmod, fd, m)); }
mode_t umask(mode_t m) { return __sc1(SYS_umask, m); }
int stat(const char *restrict p, struct stat *restrict s) { return __ret(__sc2(SYS_stat, p, s)); }
int lstat(const char *restrict p, struct stat *restrict s) { return __ret(__sc2(SYS_lstat, p, s)); }
int fstat(int fd, struct stat *s) { return __ret(__sc2(SYS_fstat, fd, s)); }
int fstatat(int d, const char *restrict p, struct stat *restrict s, int f) { return __ret(__sc4(SYS_newfstatat, d, p, s, f)); }
int utimensat(int d, const char *p, const struct timespec t[2], int f) { return __ret(__sc4(SYS_utimensat, d, p, t, f)); }
int statfs(const char *p, struct statfs *s) { return __ret(__sc2(SYS_statfs, p, s)); }

pid_t waitpid(pid_t pid, int *st, int opt) { return __ret(__sc4(SYS_wait4, pid, st, opt, 0)); }
pid_t wait(int *st) { return waitpid(-1, st, 0); }
int kill(pid_t pid, int sig) { return __ret(__sc2(SYS_kill, pid, sig)); }
int killpg(pid_t pg, int sig) { return kill(-pg, sig); }

void *mmap(void *a, size_t l, int prot, int fl, int fd, off_t off) {
    long r = __sc6(SYS_mmap, a, l, prot, fl, fd, off);
    if (r < 0 && r > -4096) {
        errno = -r;
        return MAP_FAILED;
    }
    return (void *)r;
}
int munmap(void *a, size_t l) { return __ret(__sc2(SYS_munmap, a, l)); }
int mprotect(void *a, size_t l, int p) { return __ret(__sc3(SYS_mprotect, a, l, p)); }

static char *cur_brk;
int brk(void *a) {
    long r = __sc1(SYS_brk, a);
    if ((void *)r != a) {
        errno = ENOMEM;
        return -1;
    }
    cur_brk = a;
    return 0;
}
int madvise(void *a, size_t n, int adv) { return __ret(__sc3(SYS_madvise, a, n, adv)); }
void *sbrk(long inc) {
    if (!cur_brk) cur_brk = (char *)__sc1(SYS_brk, 0);
    char *old = cur_brk;
    if (inc && brk(old + inc) < 0) return (void *)-1;
    return old;
}

int uname(struct utsname *u) { return __ret(__sc1(SYS_uname, u)); }
int gethostname(char *name, size_t n) {
    struct utsname u;
    if (uname(&u) < 0) return -1;
    strncpy(name, u.nodename, n);
    return 0;
}
int getrlimit(int r, struct rlimit *l) { return __ret(__sc2(SYS_getrlimit, r, l)); }
int setrlimit(int r, const struct rlimit *l) { return __ret(__sc2(SYS_setrlimit, r, l)); }
int getrusage(int w, struct rusage *u) { return __ret(__sc2(SYS_getrusage, w, u)); }
clock_t times(struct tms *t) { return __ret(__sc1(SYS_times, t)); }
int sched_yield(void) { return __ret(__sc0(SYS_sched_yield)); }
int sched_setaffinity(pid_t pid, size_t size, const cpu_set_t *set) { return __ret(__sc3(SYS_sched_setaffinity, pid, size, set)); }
int sched_getaffinity(pid_t pid, size_t size, cpu_set_t *set) {
    long r = __ret(__sc3(SYS_sched_getaffinity, pid, size, set));
    return r < 0 ? -1 : 0;
}
int getpagesize(void) { return 4096; }

long sysconf(int name) {
    switch (name) {
    case _SC_PAGESIZE: return 4096;
    case _SC_CLK_TCK: return 100;
    case _SC_OPEN_MAX: return 1024;
    case _SC_NPROCESSORS_ONLN:
    case _SC_NPROCESSORS_CONF: return 4;
    case _SC_ARG_MAX: return 131072;
    case _SC_CHILD_MAX: return 1024;
    case _SC_VERSION: return 200809L;
    case _SC_CPUTIME:
    case _SC_MONOTONIC_CLOCK:
    case _SC_TIMERS:
    case _SC_REALTIME_SIGNALS: return 200809L;
    case _SC_THREAD_CPUTIME: return -1;
    }
    errno = EINVAL;
    return -1;
}

int poll(struct pollfd *f, nfds_t n, int t) { return __ret(__sc3(SYS_poll, f, n, t)); }
int select(int n, fd_set *r, fd_set *w, fd_set *e, struct timeval *tv) { return __ret(__sc5(SYS_select, n, r, w, e, tv)); }

/* time */
int clock_gettime(clockid_t c, struct timespec *ts) { return __ret(__sc2(SYS_clock_gettime, c, ts)); }
int clock_getres(clockid_t c, struct timespec *ts) { return __ret(__sc2(SYS_clock_getres, c, ts)); }
int nanosleep(const struct timespec *req, struct timespec *rem) { return __ret(__sc2(SYS_nanosleep, req, rem)); }
int clock_nanosleep(clockid_t c, int f, const struct timespec *req, struct timespec *rem) {
    long r = __sc4(SYS_clock_nanosleep, c, f, req, rem);
    return r < 0 ? -r : 0;
}
int gettimeofday(struct timeval *restrict tv, void *restrict tz) {
    (void)tz;
    return __ret(__sc2(SYS_gettimeofday, tv, 0));
}
unsigned sleep(unsigned s) {
    struct timespec t = {s, 0}, rem = {0, 0};
    if (nanosleep(&t, &rem) < 0) return rem.tv_sec;
    return 0;
}
int usleep(useconds_t us) {
    struct timespec t = {us / 1000000, (us % 1000000) * 1000};
    return nanosleep(&t, NULL);
}
unsigned alarm(unsigned s) { return __sc1(SYS_alarm, s); }
int pause(void) {
    sigset_t m;
    sigprocmask(SIG_BLOCK, NULL, &m);
    return sigsuspend(&m);
}

/* sockets */
int socket(int d, int t, int p) { return __ret(__sc3(SYS_socket, d, t, p)); }
int bind(int fd, const struct sockaddr *a, socklen_t l) { return __ret(__sc3(SYS_bind, fd, a, l)); }
int listen(int fd, int b) { return __ret(__sc2(SYS_listen, fd, b)); }
int accept(int fd, struct sockaddr *restrict a, socklen_t *restrict l) { return __ret(__sc4(SYS_accept4, fd, a, l, 0)); }
int accept4(int fd, struct sockaddr *restrict a, socklen_t *restrict l, int f) { return __ret(__sc4(SYS_accept4, fd, a, l, f)); }
int connect(int fd, const struct sockaddr *a, socklen_t l) { return __ret(__sc3(SYS_connect, fd, a, l)); }
ssize_t sendto(int fd, const void *b, size_t n, int f, const struct sockaddr *a, socklen_t l) { return __ret(__sc6(SYS_sendto, fd, b, n, f, a, l)); }
ssize_t recvfrom(int fd, void *restrict b, size_t n, int f, struct sockaddr *restrict a, socklen_t *restrict l) { return __ret(__sc6(SYS_recvfrom, fd, b, n, f, a, l)); }
ssize_t send(int fd, const void *b, size_t n, int f) { return sendto(fd, b, n, f, NULL, 0); }
ssize_t recv(int fd, void *b, size_t n, int f) { return recvfrom(fd, b, n, f, NULL, NULL); }
int shutdown(int fd, int h) { return __ret(__sc2(SYS_shutdown, fd, h)); }
int setsockopt(int fd, int lv, int nm, const void *v, socklen_t l) { return __ret(__sc5(SYS_setsockopt, fd, lv, nm, v, l)); }
int getsockopt(int fd, int lv, int nm, void *restrict v, socklen_t *restrict l) { return __ret(__sc5(SYS_getsockopt, fd, lv, nm, v, l)); }
int getsockname(int fd, struct sockaddr *restrict a, socklen_t *restrict l) { return __ret(__sc3(SYS_getsockname, fd, a, l)); }
int getpeername(int fd, struct sockaddr *restrict a, socklen_t *restrict l) { return __ret(__sc3(SYS_getpeername, fd, a, l)); }

/* signals */
extern void __restore_rt(void);

int sigaction(int sig, const struct sigaction *restrict act, struct sigaction *restrict old) {
    struct { void *handler; unsigned long flags; void *restorer; unsigned long mask; } k, ko;
    if (act) {
        k.handler = (void *)act->sa_handler;
        k.flags = (unsigned)act->sa_flags | SA_RESTORER;
        k.restorer = (void *)__restore_rt;
        k.mask = act->sa_mask;
    }
    long r = __sc4(SYS_rt_sigaction, sig, act ? &k : NULL, old ? &ko : NULL, 8);
    if (r < 0) return __ret(r);
    if (old) {
        old->sa_handler = (sighandler_t)ko.handler;
        old->sa_flags = ko.flags & ~SA_RESTORER;
        old->sa_mask = ko.mask;
        old->sa_restorer = 0;
    }
    return 0;
}

sighandler_t signal(int sig, sighandler_t h) {
    struct sigaction a = {0}, old;
    a.sa_handler = h;
    a.sa_flags = SA_RESTART;
    if (sigaction(sig, &a, &old) < 0) return SIG_ERR;
    return old.sa_handler;
}

int sigprocmask(int how, const sigset_t *restrict s, sigset_t *restrict o) { return __ret(__sc4(SYS_rt_sigprocmask, how, s, o, 8)); }
int sigsuspend(const sigset_t *m) { return __ret(__sc2(SYS_rt_sigsuspend, m, 8)); }
int sigpending(sigset_t *s) { return __ret(__sc2(SYS_rt_sigpending, s, 8)); }
int sigemptyset(sigset_t *s) { *s = 0; return 0; }
int sigfillset(sigset_t *s) { *s = ~0UL; return 0; }
int sigaddset(sigset_t *s, int n) {
    if (n <= 0 || n >= NSIG) { errno = EINVAL; return -1; }
    *s |= 1UL << (n - 1);
    return 0;
}
int sigdelset(sigset_t *s, int n) {
    if (n <= 0 || n >= NSIG) { errno = EINVAL; return -1; }
    *s &= ~(1UL << (n - 1));
    return 0;
}
int sigismember(const sigset_t *s, int n) {
    if (n <= 0 || n >= NSIG) { errno = EINVAL; return -1; }
    return (*s >> (n - 1)) & 1;
}
int raise(int sig) { return kill(getpid(), sig); }

pid_t setpgrp(void) { return setpgid(0, 0); }
int msync(void *a, size_t n, int f) { return __ret(__sc3(26, a, n, f)); }
int mlock(const void *a, size_t n) { (void)a; (void)n; return 0; }
int munlock(const void *a, size_t n) { (void)a; (void)n; return 0; }

int sigaltstack(const stack_t *restrict ss, stack_t *restrict old) { return __ret(__sc2(131, ss, old)); }

/* The XSI signal calls, in terms of the POSIX ones. */
int sighold(int sig) {
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, sig);
    return sigprocmask(SIG_BLOCK, &s, NULL);
}
int sigrelse(int sig) {
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, sig);
    return sigprocmask(SIG_UNBLOCK, &s, NULL);
}
int sigignore(int sig) { return signal(sig, SIG_IGN) == SIG_ERR ? -1 : 0; }
int sigpause(int sig) {
    sigset_t s;
    sigprocmask(SIG_SETMASK, NULL, &s);
    sigdelset(&s, sig);
    return sigsuspend(&s);
}
sighandler_t sigset(int sig, sighandler_t h) {
    sigset_t s, old;
    sigemptyset(&s);
    sigaddset(&s, sig);
    if (h == (sighandler_t)2) { /* SIG_HOLD */
        sigprocmask(SIG_BLOCK, &s, &old);
        return sigismember(&old, sig) ? (sighandler_t)2 : SIG_DFL;
    }
    sigprocmask(SIG_UNBLOCK, &s, &old);
    sighandler_t prev = signal(sig, h);
    return sigismember(&old, sig) ? (sighandler_t)2 : prev;
}
/* sigwait: block in sigsuspend with only `set` allowed, collect the signal with a handler. */
static volatile int sigwait_got;
static void sigwait_catch(int sig) { sigwait_got = sig; }
int sigwait(const sigset_t *restrict set, int *restrict sig) {
    struct sigaction sa, old[65];
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = sigwait_catch;
    sigfillset(&sa.sa_mask);
    for (int i = 1; i < 65; i++)
        if (sigismember(set, i)) sigaction(i, &sa, &old[i]);
    sigset_t mask;
    sigfillset(&mask);
    for (int i = 1; i < 65; i++)
        if (sigismember(set, i)) sigdelset(&mask, i);
    sigwait_got = 0;
    while (!sigwait_got) sigsuspend(&mask);
    for (int i = 1; i < 65; i++)
        if (sigismember(set, i)) sigaction(i, &old[i], NULL);
    *sig = sigwait_got;
    return 0;
}
