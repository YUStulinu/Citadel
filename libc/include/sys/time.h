#ifndef _SYS_TIME_H
#define _SYS_TIME_H
#include <sys/types.h>
#include <time.h>
#include <sys/select.h>
struct timeval { time_t tv_sec; suseconds_t tv_usec; };
struct timezone { int tz_minuteswest, tz_dsttime; };
int gettimeofday(struct timeval *restrict tv, void *restrict tz);
#endif
