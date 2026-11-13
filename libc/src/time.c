/* Calendar time. Citadel has no time zone database: local time is UTC. */
#include <stdio.h>
#include <string.h>
#include <time.h>

char *tzname[2] = {"UTC", "UTC"};
long timezone;
int daylight;
void tzset(void) {}

time_t time(time_t *t) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    if (t) *t = ts.tv_sec;
    return ts.tv_sec;
}

clock_t clock(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts) < 0) return (clock_t)-1;
    return ts.tv_sec * 1000000L + ts.tv_nsec / 1000;
}

double difftime(time_t a, time_t b) { return (double)a - (double)b; }

/* Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm). */
static long days_from_civil(long y, unsigned m, unsigned d) {
    y -= m <= 2;
    long era = (y >= 0 ? y : y - 399) / 400;
    unsigned yoe = (unsigned)(y - era * 400);
    unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + (long)doe - 719468;
}

/* ISO 8601 years have 53 weeks when they start on a Thursday, or on a Wednesday in a leap year. */
static int iso_weeks(long y) {
#define P(y) (((y) + (y) / 4 - (y) / 100 + (y) / 400) % 7)
    return 52 + (P(y) == 4 || P(y - 1) == 3);
#undef P
}

struct tm *gmtime_r(const time_t *restrict tp, struct tm *restrict t) {
    long long s = *tp;
    long days = (long)(s / 86400);
    long rem = (long)(s % 86400);
    if (rem < 0) {
        rem += 86400;
        days--;
    }
    t->tm_hour = rem / 3600;
    t->tm_min = rem / 60 % 60;
    t->tm_sec = rem % 60;
    t->tm_wday = (int)((days % 7 + 11) % 7); /* 1970-01-01 was a Thursday */
    long z = days + 719468;
    long era = (z >= 0 ? z : z - 146096) / 146097;
    unsigned doe = (unsigned)(z - era * 146097);
    unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    long y = (long)yoe + era * 400;
    unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    unsigned mp = (5 * doy + 2) / 153;
    unsigned d = doy - (153 * mp + 2) / 5 + 1;
    unsigned m = mp < 10 ? mp + 3 : mp - 9;
    y += m <= 2;
    t->tm_year = (int)(y - 1900);
    t->tm_mon = (int)m - 1;
    t->tm_mday = (int)d;
    t->tm_yday = (int)(days - days_from_civil(y, 1, 1));
    t->tm_isdst = 0;
    t->tm_gmtoff = 0;
    t->tm_zone = "UTC";
    return t;
}

struct tm *gmtime(const time_t *t) {
    static struct tm buf;
    return gmtime_r(t, &buf);
}
struct tm *localtime_r(const time_t *restrict t, struct tm *restrict out) { return gmtime_r(t, out); }
struct tm *localtime(const time_t *t) { return gmtime(t); }

time_t timegm(struct tm *t) {
    /* Normalise month overflow, then let the day count absorb everything else. */
    long y = t->tm_year + 1900L, mon = t->tm_mon;
    y += mon / 12;
    mon %= 12;
    if (mon < 0) {
        mon += 12;
        y--;
    }
    long long days = days_from_civil(y, (unsigned)mon + 1, 1) + (t->tm_mday - 1);
    time_t r = (time_t)(days * 86400 + t->tm_hour * 3600LL + t->tm_min * 60LL + t->tm_sec);
    gmtime_r(&r, t);
    return r;
}
time_t mktime(struct tm *t) { return timegm(t); }

static const char *const wday_name[] = {"Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"};
static const char *const mon_name[] = {"January", "February", "March",     "April",   "May",      "June",
                                       "July",    "August",   "September", "October", "November", "December"};

size_t strftime(char *restrict s, size_t n, const char *restrict fmt, const struct tm *restrict t) {
    size_t o = 0;
    char tmp[64];
    for (const char *p = fmt; *p; p++) {
        const char *add = tmp;
        if (*p != '%') {
            tmp[0] = *p;
            tmp[1] = 0;
        } else {
            p++;
            if (*p == 'E' || *p == 'O') p++;
            switch (*p) {
            case 'a': snprintf(tmp, sizeof tmp, "%.3s", wday_name[t->tm_wday % 7]); break;
            case 'A': add = wday_name[t->tm_wday % 7]; break;
            case 'b':
            case 'h': snprintf(tmp, sizeof tmp, "%.3s", mon_name[t->tm_mon % 12]); break;
            case 'B': add = mon_name[t->tm_mon % 12]; break;
            case 'c':
                snprintf(tmp, sizeof tmp, "%.3s %.3s %2d %02d:%02d:%02d %d", wday_name[t->tm_wday % 7],
                         mon_name[t->tm_mon % 12], t->tm_mday, t->tm_hour, t->tm_min, t->tm_sec, t->tm_year + 1900);
                break;
            case 'C': snprintf(tmp, sizeof tmp, "%02d", (t->tm_year + 1900) / 100); break;
            case 'd': snprintf(tmp, sizeof tmp, "%02d", t->tm_mday); break;
            case 'D': snprintf(tmp, sizeof tmp, "%02d/%02d/%02d", t->tm_mon + 1, t->tm_mday, t->tm_year % 100); break;
            case 'e': snprintf(tmp, sizeof tmp, "%2d", t->tm_mday); break;
            case 'F': snprintf(tmp, sizeof tmp, "%d-%02d-%02d", t->tm_year + 1900, t->tm_mon + 1, t->tm_mday); break;
            case 'H': snprintf(tmp, sizeof tmp, "%02d", t->tm_hour); break;
            case 'I': snprintf(tmp, sizeof tmp, "%02d", t->tm_hour % 12 ? t->tm_hour % 12 : 12); break;
            case 'j': snprintf(tmp, sizeof tmp, "%03d", t->tm_yday + 1); break;
            case 'm': snprintf(tmp, sizeof tmp, "%02d", t->tm_mon + 1); break;
            case 'M': snprintf(tmp, sizeof tmp, "%02d", t->tm_min); break;
            case 'n': add = "\n"; break;
            case 'p': add = t->tm_hour < 12 ? "AM" : "PM"; break;
            case 'r':
                snprintf(tmp, sizeof tmp, "%02d:%02d:%02d %s", t->tm_hour % 12 ? t->tm_hour % 12 : 12, t->tm_min,
                         t->tm_sec, t->tm_hour < 12 ? "AM" : "PM");
                break;
            case 'R': snprintf(tmp, sizeof tmp, "%02d:%02d", t->tm_hour, t->tm_min); break;
            case 's': {
                struct tm c = *t;
                snprintf(tmp, sizeof tmp, "%lld", (long long)timegm(&c));
                break;
            }
            case 'S': snprintf(tmp, sizeof tmp, "%02d", t->tm_sec); break;
            case 't': add = "\t"; break;
            case 'T': snprintf(tmp, sizeof tmp, "%02d:%02d:%02d", t->tm_hour, t->tm_min, t->tm_sec); break;
            case 'u': snprintf(tmp, sizeof tmp, "%d", t->tm_wday ? t->tm_wday : 7); break;
            case 'U': snprintf(tmp, sizeof tmp, "%02d", (t->tm_yday + 7 - t->tm_wday) / 7); break;
            case 'W': snprintf(tmp, sizeof tmp, "%02d", (t->tm_yday + 7 - (t->tm_wday + 6) % 7) / 7); break;
            case 'w': snprintf(tmp, sizeof tmp, "%d", t->tm_wday); break;
            case 'x': snprintf(tmp, sizeof tmp, "%02d/%02d/%02d", t->tm_mon + 1, t->tm_mday, t->tm_year % 100); break;
            case 'X': snprintf(tmp, sizeof tmp, "%02d:%02d:%02d", t->tm_hour, t->tm_min, t->tm_sec); break;
            case 'y': snprintf(tmp, sizeof tmp, "%02d", t->tm_year % 100); break;
            case 'Y': snprintf(tmp, sizeof tmp, "%d", t->tm_year + 1900); break;
            case 'z': add = "+0000"; break;
            case 'Z': add = "UTC"; break;
            case 'G': snprintf(tmp, sizeof tmp, "%d", t->tm_year + 1900); break;
            case 'g': snprintf(tmp, sizeof tmp, "%02d", t->tm_year % 100); break;
            case 'V': {
                int wd = (t->tm_wday + 6) % 7, wk = (t->tm_yday - wd + 10) / 7;
                long y = t->tm_year + 1900L;
                if (wk < 1) wk = iso_weeks(y - 1);
                else if (wk > iso_weeks(y)) wk = 1;
                snprintf(tmp, sizeof tmp, "%02d", wk);
                break;
            }
            case '%': add = "%"; break;
            case 0: p--; tmp[0] = 0; break;
            default: tmp[0] = '%'; tmp[1] = *p; tmp[2] = 0;
            }
        }
        size_t l = strlen(add);
        if (o + l >= n) return 0;
        memcpy(s + o, add, l);
        o += l;
    }
    s[o] = 0;
    return o;
}

char *asctime(const struct tm *t) {
    static char buf[64];
    snprintf(buf, sizeof buf, "%.3s %.3s%3d %02d:%02d:%02d %d\n", wday_name[t->tm_wday % 7], mon_name[t->tm_mon % 12],
             t->tm_mday, t->tm_hour, t->tm_min, t->tm_sec, t->tm_year + 1900);
    return buf;
}
char *ctime(const time_t *t) { return asctime(localtime(t)); }
