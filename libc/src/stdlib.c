/* Program start, exit, environment, number conversion, sorting, random numbers. */
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

char **environ;
extern int main(int, char **, char **);
extern void __stdio_init(void);
extern void __stdio_flush_all(void);

static void (*atexit_fns[32])(void);
static int atexit_n;

_Noreturn void __libc_start(long *sp) {
    int argc = (int)sp[0];
    char **argv = (char **)(sp + 1);
    environ = argv + argc + 1;
    __stdio_init();
    exit(main(argc, argv, environ));
}

int atexit(void (*f)(void)) {
    if (atexit_n == 32) return -1;
    atexit_fns[atexit_n++] = f;
    return 0;
}

_Noreturn void exit(int code) {
    while (atexit_n > 0) atexit_fns[--atexit_n]();
    __stdio_flush_all();
    _exit(code);
}

_Noreturn void _Exit(int code) { _exit(code); }

_Noreturn void abort(void) {
    __stdio_flush_all();
    raise(SIGABRT);
    _exit(127);
}

_Noreturn void __assert_fail(const char *e, const char *file, int line, const char *fn) {
    fprintf(stderr, "Assertion failed: %s (%s: %s: %d)\n", e, file, fn, line);
    abort();
}

/* ---------------------------------------------------------------- environment */

char *getenv(const char *name) {
    size_t l = strlen(name);
    if (!environ) return NULL;
    for (char **e = environ; *e; e++)
        if (!strncmp(*e, name, l) && (*e)[l] == '=') return *e + l + 1;
    return NULL;
}

static int env_owned;

static int env_set(char *entry, size_t namelen) {
    size_t n = 0;
    if (environ)
        for (; environ[n]; n++)
            if (!strncmp(environ[n], entry, namelen) && environ[n][namelen] == '=') {
                environ[n] = entry;
                return 0;
            }
    char **ne = malloc((n + 2) * sizeof *ne);
    if (!ne) return -1;
    if (environ) memcpy(ne, environ, n * sizeof *ne);
    ne[n] = entry;
    ne[n + 1] = NULL;
    if (env_owned) free(environ);
    environ = ne;
    env_owned = 1;
    return 0;
}

int setenv(const char *name, const char *value, int overwrite) {
    if (!name || !*name || strchr(name, '=')) {
        errno = EINVAL;
        return -1;
    }
    if (!overwrite && getenv(name)) return 0;
    size_t l = strlen(name), v = strlen(value);
    char *s = malloc(l + v + 2);
    if (!s) return -1;
    memcpy(s, name, l);
    s[l] = '=';
    memcpy(s + l + 1, value, v + 1);
    return env_set(s, l);
}

int putenv(char *s) {
    char *eq = strchr(s, '=');
    if (!eq) return -1;
    return env_set(s, eq - s);
}

int unsetenv(const char *name) {
    size_t l = strlen(name);
    if (!environ) return 0;
    char **w = environ;
    for (char **e = environ; *e; e++)
        if (!(strncmp(*e, name, l) == 0 && (*e)[l] == '=')) *w++ = *e;
    *w = NULL;
    return 0;
}

int system(const char *cmd) {
    if (!cmd) return 1;
    pid_t p = fork();
    if (p < 0) return -1;
    if (p == 0) {
        execl("/bin/sh", "sh", "-c", cmd, (char *)0);
        _exit(127);
    }
    int st;
    while (waitpid(p, &st, 0) < 0)
        if (errno != EINTR) return -1;
    return st;
}

/* ---------------------------------------------------------------- integers */

static unsigned long long parse_unsigned(const char *s, char **end, int base, int *neg, int *overflow) {
    const char *p = s;
    while (isspace((unsigned char)*p)) p++;
    *neg = 0;
    if (*p == '+' || *p == '-') *neg = *p++ == '-';
    if ((base == 0 || base == 16) && p[0] == '0' && (p[1] | 32) == 'x' && isxdigit((unsigned char)p[2])) {
        p += 2;
        base = 16;
    } else if (base == 0) {
        base = *p == '0' ? 8 : 10;
    }
    unsigned long long v = 0;
    const char *start = p;
    *overflow = 0;
    for (;; p++) {
        int d;
        if (isdigit((unsigned char)*p)) d = *p - '0';
        else if (isalpha((unsigned char)*p)) d = (*p | 32) - 'a' + 10;
        else break;
        if (d >= base) break;
        if (v > (ULLONG_MAX - d) / base) *overflow = 1;
        v = v * base + d;
    }
    if (end) *end = (char *)(p == start ? s : p);
    return v;
}

long long strtoll(const char *restrict s, char **restrict end, int base) {
    int neg, of;
    unsigned long long v = parse_unsigned(s, end, base, &neg, &of);
    if (of || v > (unsigned long long)LLONG_MAX + neg) {
        errno = ERANGE;
        return neg ? LLONG_MIN : LLONG_MAX;
    }
    return neg ? -(long long)v : (long long)v;
}

unsigned long long strtoull(const char *restrict s, char **restrict end, int base) {
    int neg, of;
    unsigned long long v = parse_unsigned(s, end, base, &neg, &of);
    if (of) {
        errno = ERANGE;
        return ULLONG_MAX;
    }
    return neg ? -v : v;
}

long strtol(const char *restrict s, char **restrict end, int base) { return strtoll(s, end, base); }
unsigned long strtoul(const char *restrict s, char **restrict end, int base) { return strtoull(s, end, base); }
int atoi(const char *s) { return (int)strtol(s, NULL, 10); }
long atol(const char *s) { return strtol(s, NULL, 10); }
long long atoll(const char *s) { return strtoll(s, NULL, 10); }
int abs(int x) { return x < 0 ? -x : x; }
long labs(long x) { return x < 0 ? -x : x; }
long long llabs(long long x) { return x < 0 ? -x : x; }
div_t div(int a, int b) { return (div_t){a / b, a % b}; }
ldiv_t ldiv(long a, long b) { return (ldiv_t){a / b, a % b}; }

/* ---------------------------------------------------------------- floating point */

/* strtod: decimal and hexadecimal, inf/nan. Digits are accumulated in a long double (64-bit
   mantissa) and scaled by powers of ten in long double, so results are within an ulp or so of
   the correctly rounded value. */
long double strtold(const char *restrict s, char **restrict end) {
    const char *p = s;
    while (isspace((unsigned char)*p)) p++;
    int neg = 0;
    if (*p == '+' || *p == '-') neg = *p++ == '-';
    if (!strncasecmp(p, "inf", 3)) {
        p += 3;
        if (!strncasecmp(p, "inity", 5)) p += 5;
        if (end) *end = (char *)p;
        return neg ? -__builtin_infl() : __builtin_infl();
    }
    if (!strncasecmp(p, "nan", 3)) {
        p += 3;
        if (*p == '(') {
            const char *q = p + 1;
            while (isalnum((unsigned char)*q) || *q == '_') q++;
            if (*q == ')') p = q + 1;
        }
        if (end) *end = (char *)p;
        return __builtin_nanl("");
    }
    long double v = 0;
    int any = 0;
    if (p[0] == '0' && (p[1] | 32) == 'x') {
        const char *q = p + 2;
        long exp2 = 0;
        int seen_dot = 0;
        for (;; q++) {
            int d;
            if (*q == '.' && !seen_dot) { seen_dot = 1; continue; }
            if (isdigit((unsigned char)*q)) d = *q - '0';
            else if (isxdigit((unsigned char)*q)) d = (*q | 32) - 'a' + 10;
            else break;
            any = 1;
            if (v < 0x1p62L) {
                v = v * 16 + d;
                if (seen_dot) exp2 -= 4;
            } else if (!seen_dot) {
                exp2 += 4;
            }
        }
        if (!any) {
            if (end) *end = (char *)(p + 1);
            return 0;
        }
        if ((*q | 32) == 'p') {
            const char *r = q + 1;
            int en = 0;
            if (*r == '+' || *r == '-') en = *r++ == '-';
            if (isdigit((unsigned char)*r)) {
                long e = 0;
                while (isdigit((unsigned char)*r)) { if (e < 100000) e = e * 10 + (*r - '0'); r++; }
                exp2 += en ? -e : e;
                q = r;
            }
        }
        if (end) *end = (char *)q;
        v = __builtin_ldexpl(v, (int)exp2);
        return neg ? -v : v;
    }
    long exp10 = 0;
    int digits = 0, seen_dot = 0;
    for (;; p++) {
        if (*p == '.' && !seen_dot) { seen_dot = 1; continue; }
        if (!isdigit((unsigned char)*p)) break;
        any = 1;
        if (digits < 19) {
            v = v * 10 + (*p - '0');
            if (v != 0) digits++;
            if (seen_dot) exp10--;
        } else if (!seen_dot) {
            exp10++;
        }
    }
    if (!any) {
        if (end) *end = (char *)s;
        return 0;
    }
    if ((*p | 32) == 'e') {
        const char *r = p + 1;
        int en = 0;
        if (*r == '+' || *r == '-') en = *r++ == '-';
        if (isdigit((unsigned char)*r)) {
            long e = 0;
            while (isdigit((unsigned char)*r)) { if (e < 100000) e = e * 10 + (*r - '0'); r++; }
            exp10 += en ? -e : e;
            p = r;
        }
    }
    if (end) *end = (char *)p;
    if (v != 0) {
        /* Scale by 10^|exp10| with repeated squaring. */
        long double scale = 1, base = 10;
        long e = exp10 < 0 ? -exp10 : exp10;
        if (e > 5000) e = 5000;
        while (e) {
            if (e & 1) scale *= base;
            base *= base;
            e >>= 1;
        }
        v = exp10 < 0 ? v / scale : v * scale;
        double d = (double)v;
        if (d == 0 || __builtin_isinf(d)) errno = ERANGE;
    }
    return neg ? -v : v;
}

int __fp_digits(uint64_t m, int e, char *dig, int *dp);

/* Compare the decimal 0.A * 10^adp with 0.B * 10^bdp (digit strings without leading zeros). */
static int cmp_decimal(const char *a, int alen, long adp, const char *b, int blen, long bdp) {
    if (adp != bdp) return adp < bdp ? -1 : 1;
    int n = alen > blen ? alen : blen;
    for (int i = 0; i < n; i++) {
        char x = i < alen ? a[i] : '0', y = i < blen ? b[i] : '0';
        if (x != y) return x < y ? -1 : 1;
    }
    return 0;
}

/* Compare the input with (m * 2^e) exactly. */
static int cmp_input(const char *dig, int len, long dp, uint64_t m, int e) {
    static char buf[1200];
    int bdp;
    int bl = __fp_digits(m, e, buf, &bdp);
    return cmp_decimal(dig, len, dp, buf, bl, bdp);
}

/* strtod is correctly rounded: the extended-precision estimate is checked against the exact
   midpoints to its neighbours (big-integer decimal expansion) and moved by an ulp if needed. */
double strtod(const char *restrict s, char **restrict end) {
    char *e;
    long double est = strtold(s, &e);
    if (end) *end = e;
    double d = (double)est;
    if (d == 0 || !__builtin_isfinite(d)) return d;
    /* Re-read the significant digits (decimal input only). */
    const char *p = s;
    while (isspace((unsigned char)*p)) p++;
    if (*p == '+' || *p == '-') p++;
    if (p[0] == '0' && (p[1] | 32) == 'x') return d;
    if (!isdigit((unsigned char)*p) && *p != '.') return d;
    static char dig[800];
    int len = 0, sticky = 0, seen_dot = 0;
    long dp = 0;
    for (; p < e; p++) {
        if (*p == '.') {
            seen_dot = 1;
            continue;
        }
        if (!isdigit((unsigned char)*p)) break;
        if (len == 0 && *p == '0') {
            if (seen_dot) dp--;
            continue;
        }
        if (len < (int)sizeof dig - 1) dig[len++] = *p;
        else if (*p != '0') sticky = 1;
        if (!seen_dot) dp++;
    }
    if (p < e && (*p | 32) == 'e') dp += strtol(p + 1, NULL, 10);
    if (sticky) dig[len++] = '1'; /* stands for the non-zero digits that were dropped */
    while (len > 1 && dig[len - 1] == '0') len--;
    if (dp > 400 || dp < -400) return d;
    int neg = d < 0;
    double a = neg ? -d : d;
    for (int iter = 0; iter < 4; iter++) {
        union { double d; uint64_t u; } v = {a};
        int bexp = (int)((v.u >> 52) & 0x7ff);
        uint64_t M = v.u & ((1ULL << 52) - 1);
        if (bexp) M |= 1ULL << 52;
        int E = (bexp ? bexp : 1) - 1075;
        /* upper midpoint: (2M+1) * 2^(E-1) */
        int c = cmp_input(dig, len, dp, 2 * M + 1, E - 1);
        if (c > 0 || (c == 0 && (M & 1))) {
            v.u++;
            a = v.d;
            if (!__builtin_isfinite(a)) break;
            continue;
        }
        /* lower midpoint: at a power of two the gap below is half as wide */
        int c2 = (M == (1ULL << 52) && bexp > 1) ? cmp_input(dig, len, dp, 4 * M - 1, E - 2)
                                                  : cmp_input(dig, len, dp, 2 * M - 1, E - 1);
        if (c2 < 0 || (c2 == 0 && (M & 1))) {
            v.u--;
            a = v.d;
            continue;
        }
        break;
    }
    if (a == 0 || !__builtin_isfinite(a)) errno = ERANGE;
    return neg ? -a : a;
}
float strtof(const char *restrict s, char **restrict end) { return (float)strtod(s, end); }
double atof(const char *s) { return strtod(s, NULL); }

/* ---------------------------------------------------------------- sorting, searching */

static void swap(char *a, char *b, size_t n) {
    while (n--) {
        char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

/* Introsort-lite: median-of-three quicksort, insertion sort for small ranges. */
static void qs(char *base, size_t n, size_t sz, int (*cmp)(const void *, const void *), int depth) {
    while (n > 12) {
        if (depth-- == 0) {
            /* Heap sort fallback would go here; fall back to insertion sort (rare). */
            break;
        }
        char *a = base, *m = base + (n / 2) * sz, *z = base + (n - 1) * sz;
        if (cmp(m, a) < 0) swap(m, a, sz);
        if (cmp(z, m) < 0) {
            swap(z, m, sz);
            if (cmp(m, a) < 0) swap(m, a, sz);
        }
        swap(m, z - sz, sz);
        char *pivot = z - sz;
        char *i = a, *j = pivot;
        for (;;) {
            do i += sz; while (cmp(i, pivot) < 0);
            do j -= sz; while (cmp(pivot, j) < 0);
            if (i >= j) break;
            swap(i, j, sz);
        }
        swap(i, pivot, sz);
        size_t left = (i - base) / sz, right = n - left - 1;
        if (left < right) {
            qs(base, left, sz, cmp, depth);
            base = i + sz;
            n = right;
        } else {
            qs(i + sz, right, sz, cmp, depth);
            n = left;
        }
    }
    for (size_t k = 1; k < n; k++)
        for (size_t j = k; j > 0 && cmp(base + (j - 1) * sz, base + j * sz) > 0; j--) swap(base + (j - 1) * sz, base + j * sz, sz);
}

void qsort(void *base, size_t n, size_t sz, int (*cmp)(const void *, const void *)) {
    if (n > 1) qs(base, n, sz, cmp, 64);
}

void *bsearch(const void *key, const void *base, size_t n, size_t sz, int (*cmp)(const void *, const void *)) {
    const char *b = base;
    while (n) {
        const char *m = b + (n / 2) * sz;
        int c = cmp(key, m);
        if (!c) return (void *)m;
        if (c > 0) {
            b = m + sz;
            n -= n / 2 + 1;
        } else {
            n /= 2;
        }
    }
    return NULL;
}

/* ---------------------------------------------------------------- random numbers */

static uint64_t rng = 0x853c49e6748fea9bULL;
void srand(unsigned s) { rng = s * 6364136223846793005ULL + 1442695040888963407ULL; }
int rand(void) {
    rng = rng * 6364136223846793005ULL + 1442695040888963407ULL;
    return (int)(rng >> 33);
}
long random(void) { return rand(); }
void srandom(unsigned s) { srand(s); }

/* ---------------------------------------------------------------- paths, temporary files */

char *realpath(const char *restrict path, char *restrict out) {
    char buf[4096];
    if (path[0] != '/') {
        if (!getcwd(buf, sizeof buf)) return NULL;
    } else {
        buf[0] = 0;
    }
    size_t l = strlen(buf);
    if (l + strlen(path) + 2 > sizeof buf) {
        errno = ENAMETOOLONG;
        return NULL;
    }
    if (path[0] != '/') buf[l++] = '/';
    strcpy(buf + l, path);
    /* Normalise . and .. textually. */
    char res[4096];
    size_t r = 0;
    char *save, *tok = strtok_r(buf, "/", &save);
    while (tok) {
        if (!strcmp(tok, "..")) {
            while (r > 0 && res[r - 1] != '/') r--;
            if (r > 0) r--;
        } else if (strcmp(tok, ".")) {
            res[r++] = '/';
            strcpy(res + r, tok);
            r += strlen(tok);
        }
        tok = strtok_r(NULL, "/", &save);
    }
    if (r == 0) res[r++] = '/';
    res[r] = 0;
    struct stat st;
    if (stat(res, &st) < 0) return NULL;
    if (!out) return strdup(res);
    strcpy(out, res);
    return out;
}

int mkstemp(char *t) {
    size_t l = strlen(t);
    if (l < 6 || strcmp(t + l - 6, "XXXXXX")) {
        errno = EINVAL;
        return -1;
    }
    for (int tries = 0; tries < 100; tries++) {
        unsigned v = rand() ^ getpid() ^ tries;
        for (int i = 0; i < 6; i++) {
            t[l - 6 + i] = "abcdefghijklmnopqrstuvwxyz0123456789"[v % 36];
            v /= 36;
        }
        int fd = open(t, O_RDWR | O_CREAT | O_EXCL, 0600);
        if (fd >= 0 || errno != EEXIST) return fd;
    }
    return -1;
}

char *mktemp(char *t) {
    int fd = mkstemp(t);
    if (fd < 0) return NULL;
    close(fd);
    unlink(t);
    return t;
}

intmax_t strtoimax(const char *restrict s, char **restrict end, int base) { return strtoll(s, end, base); }
uintmax_t strtoumax(const char *restrict s, char **restrict end, int base) { return strtoull(s, end, base); }
