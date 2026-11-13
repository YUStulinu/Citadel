#ifndef _SCHED_H
#define _SCHED_H
#include <sys/types.h>
typedef struct { unsigned long bits[16]; } cpu_set_t;
#define CPU_ZERO(s) __builtin_memset((s), 0, sizeof(cpu_set_t))
#define CPU_SET(c, s) ((s)->bits[(c) / 64] |= 1UL << ((c) % 64))
#define CPU_CLR(c, s) ((s)->bits[(c) / 64] &= ~(1UL << ((c) % 64)))
#define CPU_ISSET(c, s) (((s)->bits[(c) / 64] >> ((c) % 64)) & 1)
int sched_yield(void);
int sched_setaffinity(pid_t pid, size_t size, const cpu_set_t *set);
int sched_getaffinity(pid_t pid, size_t size, cpu_set_t *set);
#endif
