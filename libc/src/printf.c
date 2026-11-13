/* printf family. Floating point is converted exactly: the double's binary value is expanded into
   a big decimal integer, then rounded half-to-even at the requested digit, so output matches glibc
   and musl digit for digit. long double arguments are formatted at double precision. */
#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

typedef struct {
    void (*put)(void *ctx, const char *s, size_t n);
    void *ctx;
    size_t total;
} sink;

static void emit(sink *k, const char *s, size_t n) {
    if (n) k->put(k->ctx, s, n);
    k->total += n;
}

static void pad(sink *k, char c, long n) {
    char buf[64];
    memset(buf, c, sizeof buf);
    while (n > 0) {
        size_t m = n > 64 ? 64 : (size_t)n;
        emit(k, buf, m);
        n -= m;
    }
}

enum { F_MINUS = 1, F_PLUS = 2, F_SPACE = 4, F_HASH = 8, F_ZERO = 16 };

/* Emit prefix + body with width/flags handling; `zeros` extra zeros go between them. */
static void field(sink *k, int flags, long width, const char *prefix, size_t plen, long zeros, const char *body,
                  size_t blen, const char *tail, size_t tlen) {
    long len = (long)(plen + blen + tlen) + (zeros > 0 ? zeros : 0);
    if (!(flags & F_MINUS) && !(flags & F_ZERO)) pad(k, ' ', width - len);
    emit(k, prefix, plen);
    if (!(flags & F_MINUS) && (flags & F_ZERO)) pad(k, '0', width - len);
    pad(k, '0', zeros);
    emit(k, body, blen);
    emit(k, tail, tlen);
    if (flags & F_MINUS) pad(k, ' ', width - len);
}

/* ------------------------------------------------------------ exact decimal expansion */

int __fp_digits(uint64_t m, int e, char *dig, int *dp);

/* Decimal digits of |x| (finite, non-zero): value = 0.DIGITS * 10^dp. Returns the digit count. */
static int decimal_digits(double x, char *dig, int *dp) {
    union { double d; uint64_t u; } v = {x};
    int bexp = (int)((v.u >> 52) & 0x7ff);
    uint64_t m = v.u & ((1ULL << 52) - 1);
    if (bexp) m |= 1ULL << 52;
    else bexp = 1;
    return __fp_digits(m, bexp - 1075, dig, dp);
}

/* Exact decimal digits of m * 2^e (m > 0, m < 2^60, -1100 < e < 1024). Shared with strtod. */
int __fp_digits(uint64_t m, int e, char *dig, int *dp) {
    while (!(m & 1)) {
        m >>= 1;
        e++;
    }
    /* Big integer in base 1e9, little-endian limbs. */
    uint32_t big[130];
    int n = 0;
    while (m) {
        big[n++] = (uint32_t)(m % 1000000000);
        m /= 1000000000;
    }
    int k = 0; /* digits after the decimal point */
    if (e > 0) {
        while (e > 0) {
            int s = e > 29 ? 29 : e;
            uint64_t carry = 0;
            for (int i = 0; i < n; i++) {
                uint64_t t = ((uint64_t)big[i] << s) + carry;
                big[i] = (uint32_t)(t % 1000000000);
                carry = t / 1000000000;
            }
            while (carry) {
                big[n++] = (uint32_t)(carry % 1000000000);
                carry /= 1000000000;
            }
            e -= s;
        }
    } else if (e < 0) {
        k = -e; /* multiply by 5^k, then the value is big / 10^k */
        int r = k;
        while (r > 0) {
            int s = r > 13 ? 13 : r;
            uint64_t mul = 1;
            for (int i = 0; i < s; i++) mul *= 5;
            uint64_t carry = 0;
            for (int i = 0; i < n; i++) {
                uint64_t t = (uint64_t)big[i] * mul + carry;
                big[i] = (uint32_t)(t % 1000000000);
                carry = t / 1000000000;
            }
            while (carry) {
                big[n++] = (uint32_t)(carry % 1000000000);
                carry /= 1000000000;
            }
            r -= s;
        }
    }
    /* Print limbs most significant first. */
    int len = 0;
    char tmp[10];
    for (int i = n - 1; i >= 0; i--) {
        uint32_t limb = big[i];
        for (int j = 8; j >= 0; j--) {
            tmp[j] = '0' + limb % 10;
            limb /= 10;
        }
        int start = 0;
        if (i == n - 1)
            while (start < 8 && tmp[start] == '0') start++;
        memcpy(dig + len, tmp + start, 9 - start);
        len += 9 - start;
    }
    /* Strip trailing zeros (they carry no information). */
    int total = len;
    while (len > 1 && dig[len - 1] == '0') len--;
    *dp = total - k;
    return len;
}

/* Round the digit string to `keep` significant digits (half to even). keep may be <= 0. */
static int round_digits(char *dig, int len, int *dp, int keep) {
    if (keep >= len) return len;
    if (keep < 0) {
        dig[0] = '0';
        *dp = 1;
        return 1; /* rounds to zero */
    }
    int up;
    char d = dig[keep];
    int rest = 0;
    for (int i = keep + 1; i < len; i++)
        if (dig[i] != '0') rest = 1;
    if (d > '5' || (d == '5' && rest)) up = 1;
    else if (d == '5') up = keep > 0 ? (dig[keep - 1] - '0') & 1 : 0;
    else up = 0;
    if (keep == 0) {
        if (up) {
            dig[0] = '1';
            (*dp)++;
            return 1;
        }
        dig[0] = '0';
        *dp = 1;
        return 1;
    }
    len = keep;
    if (up) {
        int i = len - 1;
        while (i >= 0 && dig[i] == '9') dig[i--] = '0';
        if (i < 0) {
            memmove(dig + 1, dig, len);
            dig[0] = '1';
            (*dp)++;
        } else {
            dig[i]++;
        }
    }
    while (len > 1 && dig[len - 1] == '0') len--;
    return len;
}

static void fmt_float(sink *k, double x, int flags, long width, int prec, char conv) {
    char prefix[2];
    size_t plen = 0;
    int upper = conv >= 'A' && conv <= 'Z';
    if (__builtin_signbit(x)) prefix[plen++] = '-';
    else if (flags & F_PLUS) prefix[plen++] = '+';
    else if (flags & F_SPACE) prefix[plen++] = ' ';
    if (!__builtin_isfinite(x)) {
        const char *s = __builtin_isnan(x) ? (upper ? "NAN" : "nan") : (upper ? "INF" : "inf");
        field(k, flags & ~F_ZERO, width, prefix, plen, 0, s, 3, "", 0);
        return;
    }
    if (x < 0) x = -x;
    char lc = conv | 32;
    if (lc == 'a') {
        /* Hexadecimal: 0x1.hhhhp+e */
        union { double d; uint64_t u; } v = {x};
        int bexp = (int)((v.u >> 52) & 0x7ff);
        uint64_t m = v.u & ((1ULL << 52) - 1);
        int lead = 1, e = bexp - 1023;
        if (!bexp) {
            lead = 0;
            e = m ? -1022 : 0;
        }
        if (prec >= 0 && prec < 13) {
            int drop = (13 - prec) * 4;
            uint64_t full = ((uint64_t)lead << 52) | m;
            uint64_t half = 1ULL << (drop - 1), rem = full & ((1ULL << drop) - 1);
            full >>= drop;
            if (rem > half || (rem == half && (full & 1))) full++;
            lead = (int)(full >> (prec * 4));
            m = (full & ((1ULL << (prec * 4)) - 1)) << drop;
        }
        char body[40], *p = body;
        const char *hex = upper ? "0123456789ABCDEF" : "0123456789abcdef";
        *p++ = '0' + lead;
        int nd = 13;
        if (prec < 0)
            while (nd > 0 && !((m >> ((13 - nd) * 4)) & 0xf)) nd--;
        else
            nd = prec;
        if (nd > 13) nd = 13;
        if (nd || (flags & F_HASH)) *p++ = '.';
        for (int i = 0; i < nd; i++) *p++ = hex[(m >> (48 - 4 * i)) & 0xf];
        int extra = prec > 13 ? prec - 13 : 0;
        char tail[16];
        int tl = 0;
        tail[tl++] = upper ? 'P' : 'p';
        tail[tl++] = e < 0 ? '-' : '+';
        unsigned ue = e < 0 ? -e : e;
        char eb[8];
        int el = 0;
        do eb[el++] = '0' + ue % 10; while (ue /= 10);
        while (el) tail[tl++] = eb[--el];
        char pre[4];
        memcpy(pre, prefix, plen);
        pre[plen] = '0';
        pre[plen + 1] = upper ? 'X' : 'x';
        /* Trailing zeros for precision beyond 13 go after the digits: emit body, zeros, tail. */
        long len = (long)(plen + 2 + (p - body) + extra + tl);
        if (!(flags & F_MINUS) && !(flags & F_ZERO)) pad(k, ' ', width - len);
        emit(k, pre, plen + 2);
        if (!(flags & F_MINUS) && (flags & F_ZERO)) pad(k, '0', width - len);
        emit(k, body, p - body);
        pad(k, '0', extra);
        emit(k, tail, tl);
        if (flags & F_MINUS) pad(k, ' ', width - len);
        return;
    }
    if (prec < 0) prec = 6;
    static char dig[1200];
    int dp, len;
    if (x == 0) {
        dig[0] = '0';
        len = 1;
        dp = 1;
    } else {
        len = decimal_digits(x, dig, &dp);
    }
    int strip = 0;
    if (lc == 'g') {
        if (prec == 0) prec = 1;
        int l2 = len, dp2 = dp;
        if (x != 0) l2 = round_digits(dig, len, &dp2, prec);
        int X = x == 0 ? 0 : dp2 - 1;
        strip = !(flags & F_HASH);
        if (prec > X && X >= -4) {
            lc = 'f';
            prec = prec - 1 - X;
        } else {
            lc = 'e';
            prec = prec - 1;
        }
        len = l2;
        dp = dp2;
    }
    static char body[1600];
    int bl = 0;
    char tail[8];
    int tl = 0;
    if (lc == 'f') {
        if (x != 0) len = round_digits(dig, len, &dp, dp + prec);
        if (len == 1 && dig[0] == '0') dp = 1;
        /* integer part */
        if (dp <= 0) body[bl++] = '0';
        else
            for (int i = 0; i < dp; i++) body[bl++] = i < len ? dig[i] : '0';
        int fd = prec;
        if (strip) {
            /* drop trailing zeros of the fraction */
            int last = len - dp; /* significant fraction digits */
            if (last < 0) last = 0;
            if (fd > last) fd = last;
        }
        if (fd > 0 || (flags & F_HASH)) body[bl++] = '.';
        /* Fraction digits past the last significant one are zeros: count them instead of storing. */
        int sig = len - dp; /* index past the last significant fraction digit */
        int stored = fd < sig ? fd : (sig > 0 ? sig : 0);
        for (int i = 0; i < stored; i++) {
            int idx = dp + i;
            body[bl++] = idx >= 0 ? dig[idx] : '0';
        }
        long trailing = fd - stored;
        long total = (long)plen + bl + trailing;
        if (!(flags & F_MINUS) && !(flags & F_ZERO)) pad(k, ' ', width - total);
        emit(k, prefix, plen);
        if (!(flags & F_MINUS) && (flags & F_ZERO)) pad(k, '0', width - total);
        emit(k, body, bl);
        pad(k, '0', trailing);
        if (flags & F_MINUS) pad(k, ' ', width - total);
        return;
    }
    /* 'e' */
    int e10 = 0;
    if (x != 0) {
        len = round_digits(dig, len, &dp, prec + 1);
        e10 = dp - 1;
    }
    body[bl++] = dig[0];
    int fd = prec;
    if (strip && fd > len - 1) fd = len - 1;
    if (fd > 0 || (flags & F_HASH)) body[bl++] = '.';
    int stored = fd < len - 1 ? fd : len - 1;
    for (int i = 1; i <= stored; i++) body[bl++] = dig[i];
    long trailing = fd - stored;
    tail[tl++] = upper ? 'E' : 'e';
    tail[tl++] = e10 < 0 ? '-' : '+';
    unsigned ue = e10 < 0 ? -e10 : e10;
    if (ue < 10) tail[tl++] = '0';
    char eb[8];
    int el = 0;
    do eb[el++] = '0' + ue % 10; while (ue /= 10);
    while (el) tail[tl++] = eb[--el];
    long total = (long)plen + bl + trailing + tl;
    if (!(flags & F_MINUS) && !(flags & F_ZERO)) pad(k, ' ', width - total);
    emit(k, prefix, plen);
    if (!(flags & F_MINUS) && (flags & F_ZERO)) pad(k, '0', width - total);
    emit(k, body, bl);
    pad(k, '0', trailing);
    emit(k, tail, tl);
    if (flags & F_MINUS) pad(k, ' ', width - total);
}

/* ------------------------------------------------------------ the formatter */

static int vformat(sink *k, const char *fmt, va_list ap) {
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            const char *q = p;
            while (*q && *q != '%') q++;
            emit(k, p, q - p);
            p = q - 1;
            continue;
        }
        p++;
        int flags = 0;
        for (;; p++) {
            if (*p == '-') flags |= F_MINUS;
            else if (*p == '+') flags |= F_PLUS;
            else if (*p == ' ') flags |= F_SPACE;
            else if (*p == '#') flags |= F_HASH;
            else if (*p == '0') flags |= F_ZERO;
            else break;
        }
        long width = 0;
        if (*p == '*') {
            width = va_arg(ap, int);
            if (width < 0) {
                flags |= F_MINUS;
                width = -width;
            }
            p++;
        } else {
            while (*p >= '0' && *p <= '9') width = width * 10 + (*p++ - '0');
        }
        int prec = -1;
        if (*p == '.') {
            p++;
            prec = 0;
            if (*p == '*') {
                prec = va_arg(ap, int);
                if (prec < 0) prec = -1;
                p++;
            } else {
                while (*p >= '0' && *p <= '9') prec = prec * 10 + (*p++ - '0');
            }
        }
        int len = 0; /* 1 = hh, 2 = h, 3 = l, 4 = ll, 5 = L */
        for (;; p++) {
            if (*p == 'h') len = len == 2 ? 1 : 2;
            else if (*p == 'l') len = len == 3 ? 4 : 3;
            else if (*p == 'L') len = 5;
            else if (*p == 'z' || *p == 'j' || *p == 't' || *p == 'q') len = 4;
            else break;
        }
        char c = *p;
        if (!c) break;
        if (flags & F_MINUS) flags &= ~F_ZERO;
        switch (c) {
        case '%':
            emit(k, "%", 1);
            break;
        case 'c': {
            char ch = (char)va_arg(ap, int);
            field(k, flags & ~F_ZERO, width, "", 0, 0, &ch, 1, "", 0);
            break;
        }
        case 'm':
        case 's': {
            const char *s = c == 'm' ? strerror(errno) : va_arg(ap, const char *);
            if (!s) s = "(null)";
            size_t n = prec >= 0 ? strnlen(s, prec) : strlen(s);
            field(k, flags & ~F_ZERO, width, "", 0, 0, s, n, "", 0);
            break;
        }
        case 'n': {
            void *dst = va_arg(ap, void *);
            if (len == 1) *(signed char *)dst = (signed char)k->total;
            else if (len == 2) *(short *)dst = (short)k->total;
            else if (len >= 3) *(long *)dst = (long)k->total;
            else *(int *)dst = (int)k->total;
            break;
        }
        case 'd':
        case 'i':
        case 'u':
        case 'o':
        case 'x':
        case 'X':
        case 'p': {
            unsigned long long v;
            int neg = 0;
            if (c == 'p') {
                v = (uintptr_t)va_arg(ap, void *);
                flags |= F_HASH;
                c = 'x';
                if (!v) {
                    field(k, flags & ~F_ZERO, width, "", 0, 0, "(nil)", 5, "", 0);
                    break;
                }
            } else if (c == 'd' || c == 'i') {
                long long s;
                if (len >= 3) s = va_arg(ap, long long);
                else s = va_arg(ap, int);
                if (len == 1) s = (signed char)s;
                else if (len == 2) s = (short)s;
                neg = s < 0;
                v = neg ? -(unsigned long long)s : (unsigned long long)s;
            } else {
                if (len >= 3) v = va_arg(ap, unsigned long long);
                else v = va_arg(ap, unsigned);
                if (len == 1) v = (unsigned char)v;
                else if (len == 2) v = (unsigned short)v;
            }
            unsigned base = c == 'o' ? 8 : (c == 'x' || c == 'X') ? 16 : 10;
            const char *digs = c == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
            char buf[24], *e = buf + sizeof buf, *b = e;
            for (unsigned long long t = v; t; t /= base) *--b = digs[t % base];
            char pre[3];
            size_t pl = 0;
            if (neg) pre[pl++] = '-';
            else if ((c == 'd' || c == 'i') && (flags & F_PLUS)) pre[pl++] = '+';
            else if ((c == 'd' || c == 'i') && (flags & F_SPACE)) pre[pl++] = ' ';
            if ((flags & F_HASH) && base == 16 && v) {
                pre[pl++] = '0';
                pre[pl++] = c;
            }
            long nd = e - b;
            long zeros = 0;
            if (prec >= 0) {
                flags &= ~F_ZERO;
                if (prec > nd) zeros = prec - nd;
            } else if (nd == 0) {
                zeros = 1; /* the value 0 */
            }
            if ((flags & F_HASH) && base == 8 && zeros == 0 && (nd == 0 || *b != '0')) zeros = 1;
            field(k, flags, width, pre, pl, zeros, b, nd, "", 0);
            break;
        }
        case 'f':
        case 'F':
        case 'e':
        case 'E':
        case 'g':
        case 'G':
        case 'a':
        case 'A': {
            double x = len == 5 ? (double)va_arg(ap, long double) : va_arg(ap, double);
            fmt_float(k, x, flags, width, prec, c);
            break;
        }
        default:
            emit(k, p - 1, 2);
        }
    }
    return k->total > 0x7fffffff ? -1 : (int)k->total;
}

/* ------------------------------------------------------------ front ends */

struct strbuf {
    char *s;
    size_t cap, len;
};

static void put_str(void *ctx, const char *s, size_t n) {
    struct strbuf *b = ctx;
    if (b->len < b->cap) {
        size_t m = b->cap - b->len < n ? b->cap - b->len : n;
        memcpy(b->s + b->len, s, m);
    }
    b->len += n;
}

int vsnprintf(char *restrict s, size_t n, const char *restrict fmt, va_list ap) {
    struct strbuf b = {s, n ? n - 1 : 0, 0};
    sink k = {put_str, &b, 0};
    int r = vformat(&k, fmt, ap);
    if (n) s[b.len < b.cap ? b.len : b.cap] = 0;
    return r;
}

int vsprintf(char *restrict s, const char *restrict fmt, va_list ap) { return vsnprintf(s, (size_t)-1 >> 1, fmt, ap); }

int snprintf(char *restrict s, size_t n, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(s, n, fmt, ap);
    va_end(ap);
    return r;
}

int sprintf(char *restrict s, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsprintf(s, fmt, ap);
    va_end(ap);
    return r;
}

static void put_file(void *ctx, const char *s, size_t n) { fwrite(s, 1, n, ctx); }

int vfprintf(FILE *restrict f, const char *restrict fmt, va_list ap) {
    sink k = {put_file, f, 0};
    int r = vformat(&k, fmt, ap);
    return ferror(f) ? -1 : r;
}

int fprintf(FILE *restrict f, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(f, fmt, ap);
    va_end(ap);
    return r;
}

int vprintf(const char *restrict fmt, va_list ap) { return vfprintf(stdout, fmt, ap); }

int printf(const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(stdout, fmt, ap);
    va_end(ap);
    return r;
}

struct fdbuf {
    int fd;
    char buf[512];
    size_t n;
};

static void put_fd(void *ctx, const char *s, size_t n) {
    struct fdbuf *b = ctx;
    while (n) {
        if (b->n == sizeof b->buf) {
            write(b->fd, b->buf, b->n);
            b->n = 0;
        }
        size_t m = sizeof b->buf - b->n < n ? sizeof b->buf - b->n : n;
        memcpy(b->buf + b->n, s, m);
        b->n += m;
        s += m;
        n -= m;
    }
}

int vdprintf(int fd, const char *restrict fmt, va_list ap) {
    struct fdbuf b = {fd, {0}, 0};
    sink k = {put_fd, &b, 0};
    int r = vformat(&k, fmt, ap);
    if (b.n) write(fd, b.buf, b.n);
    return r;
}

int dprintf(int fd, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vdprintf(fd, fmt, ap);
    va_end(ap);
    return r;
}
