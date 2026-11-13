#ifndef _TIME_H
#define _TIME_H
#include <sys/types.h>
#define CLOCKS_PER_SEC 1000000L
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID 3
#define TIMER_ABSTIME 1
struct timespec { time_t tv_sec; long tv_nsec; };
struct tm { int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst; long tm_gmtoff; const char *tm_zone; };
time_t time(time_t *t);
clock_t clock(void);
double difftime(time_t a, time_t b);
time_t mktime(struct tm *t);
time_t timegm(struct tm *t);
struct tm *gmtime(const time_t *t);
struct tm *gmtime_r(const time_t *restrict t, struct tm *restrict out);
struct tm *localtime(const time_t *t);
struct tm *localtime_r(const time_t *restrict t, struct tm *restrict out);
size_t strftime(char *restrict s, size_t n, const char *restrict fmt, const struct tm *restrict t);
char *asctime(const struct tm *t);
char *ctime(const time_t *t);
int clock_gettime(clockid_t c, struct timespec *ts);
int clock_getres(clockid_t c, struct timespec *ts);
int nanosleep(const struct timespec *req, struct timespec *rem);
int clock_nanosleep(clockid_t c, int flags, const struct timespec *req, struct timespec *rem);
void tzset(void);
extern char *tzname[2];
extern long timezone;
extern int daylight;
#endif
