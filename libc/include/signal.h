#ifndef _SIGNAL_H
#define _SIGNAL_H
#include <stddef.h>
#include <sys/types.h>
#define SIGHUP 1
#define SIGINT 2
#define SIGQUIT 3
#define SIGILL 4
#define SIGTRAP 5
#define SIGABRT 6
#define SIGIOT 6
#define SIGBUS 7
#define SIGFPE 8
#define SIGKILL 9
#define SIGUSR1 10
#define SIGSEGV 11
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGTERM 15
#define SIGSTKFLT 16
#define SIGCHLD 17
#define SIGCONT 18
#define SIGSTOP 19
#define SIGTSTP 20
#define SIGTTIN 21
#define SIGTTOU 22
#define SIGURG 23
#define SIGXCPU 24
#define SIGXFSZ 25
#define SIGVTALRM 26
#define SIGPROF 27
#define SIGWINCH 28
#define SIGIO 29
#define SIGPWR 30
#define SIGSYS 31
#define NSIG 65
#define _NSIG 65
#define SIGRTMIN 34
#define SIGRTMAX 64
typedef void (*sighandler_t)(int);
#define SIG_DFL ((sighandler_t)0)
#define SIG_IGN ((sighandler_t)1)
#define SIG_ERR ((sighandler_t)-1)
#define SA_NOCLDSTOP 1
#define SA_SIGINFO 4
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_RESETHAND 0x80000000
#define SA_RESTORER 0x04000000
#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2
typedef unsigned long sigset_t;
typedef int sig_atomic_t;
union sigval { int sival_int; void *sival_ptr; };
typedef struct { int si_signo, si_errno, si_code; int __pad[29]; } siginfo_t;
struct sigaction {
    union {
        sighandler_t sa_handler;
        void (*sa_sigaction)(int, siginfo_t *, void *);
    };
    sigset_t sa_mask;
    int sa_flags;
    void (*sa_restorer)(void);
};
sighandler_t signal(int sig, sighandler_t h);
int sigaction(int sig, const struct sigaction *restrict act, struct sigaction *restrict old);
int sigprocmask(int how, const sigset_t *restrict set, sigset_t *restrict old);
int sigsuspend(const sigset_t *mask);
int sigpending(sigset_t *set);
int sigemptyset(sigset_t *s);
int sigfillset(sigset_t *s);
int sigaddset(sigset_t *s, int sig);
int sigdelset(sigset_t *s, int sig);
int sigismember(const sigset_t *s, int sig);
int kill(pid_t pid, int sig);
int killpg(pid_t pg, int sig);
int raise(int sig);
#define SIGPOLL SIGIO
#define SA_NOCLDWAIT 2
#define SS_ONSTACK 1
#define SS_DISABLE 2
#define MINSIGSTKSZ 2048
#define SIGSTKSZ 8192
#define CLD_EXITED 1
#define CLD_KILLED 2
#define CLD_DUMPED 3
#define CLD_TRAPPED 4
#define CLD_STOPPED 5
#define CLD_CONTINUED 6
#define SI_USER 0
#define SI_QUEUE -1
typedef struct {
    void *ss_sp;
    int ss_flags;
    size_t ss_size;
} stack_t;
int sigaltstack(const stack_t *restrict ss, stack_t *restrict old);
int sigpause(int sig);
int sighold(int sig);
int sigrelse(int sig);
int sigignore(int sig);
sighandler_t sigset(int sig, sighandler_t h);
int sigwait(const sigset_t *restrict set, int *restrict sig);
#endif
