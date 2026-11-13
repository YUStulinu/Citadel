#ifndef _SYS_RESOURCE_H
#define _SYS_RESOURCE_H
#include <sys/time.h>
typedef unsigned long rlim_t;
struct rlimit { rlim_t rlim_cur, rlim_max; };
struct rusage { struct timeval ru_utime, ru_stime; long ru_maxrss, ru_ixrss, ru_idrss, ru_isrss, ru_minflt, ru_majflt, ru_nswap, ru_inblock, ru_oublock, ru_msgsnd, ru_msgrcv, ru_nsignals, ru_nvcsw, ru_nivcsw; };
#define RLIM_INFINITY (~0UL)
#define RLIMIT_STACK 3
#define RLIMIT_NOFILE 7
#define RLIMIT_CPU 0
#define RLIMIT_FSIZE 1
#define RLIMIT_DATA 2
#define RLIMIT_CORE 4
#define RLIMIT_AS 9
#define RUSAGE_SELF 0
#define RUSAGE_CHILDREN (-1)
int getrlimit(int r, struct rlimit *l);
int setrlimit(int r, const struct rlimit *l);
int getrusage(int who, struct rusage *u);
#endif
