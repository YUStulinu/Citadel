/* scanf family over a character source (string or stream). */
#include <ctype.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    FILE *f;
    const char *s;
    long count; /* characters consumed */
} src;

static int next(src *r) {
    int c;
    if (r->f) c = fgetc(r->f);
    else c = *r->s ? (unsigned char)*r->s++ : EOF;
    if (c != EOF) r->count++;
    return c;
}

static void back(src *r, int c) {
    if (c == EOF) return;
    r->count--;
    if (r->f) ungetc(c, r->f);
    else r->s--;
}

static void store_int(void *dst, int len, unsigned long long v) {
    switch (len) {
    case 1: *(char *)dst = (char)v; break;
    case 2: *(short *)dst = (short)v; break;
    case 3:
    case 4: *(long long *)dst = (long long)v; break;
    default: *(int *)dst = (int)v;
    }
}

static int vscan(src *r, const char *fmt, va_list ap) {
    int assigned = 0, c;
    for (const char *p = fmt; *p; p++) {
        if (isspace((unsigned char)*p)) {
            while (isspace(c = next(r))) {}
            back(r, c);
            continue;
        }
        if (*p != '%' || p[1] == '%') {
            if (*p == '%') p++;
            c = next(r);
            if (c != (unsigned char)*p) {
                back(r, c);
                return c == EOF && assigned == 0 ? EOF : assigned;
            }
            continue;
        }
        p++;
        int skip = 0;
        if (*p == '*') {
            skip = 1;
            p++;
        }
        long width = 0;
        while (isdigit((unsigned char)*p)) width = width * 10 + (*p++ - '0');
        int len = 0; /* 1 hh, 2 h, 3 l, 4 ll/z/j/t, 5 L */
        for (;; p++) {
            if (*p == 'h') len = len == 2 ? 1 : 2;
            else if (*p == 'l') len = len == 3 ? 4 : 3;
            else if (*p == 'L') len = 5;
            else if (*p == 'z' || *p == 'j' || *p == 't' || *p == 'q') len = 4;
            else break;
        }
        char conv = *p;
        if (!conv) break;
        if (conv == 'n') {
            if (!skip) store_int(va_arg(ap, void *), len, r->count);
            continue;
        }
        if (conv != 'c' && conv != '[') {
            while (isspace(c = next(r))) {}
            back(r, c);
        }
        if (!width) width = conv == 'c' ? 1 : -1;
        char buf[512];
        int n = 0;
        if (conv == 'c' || conv == 's' || conv == '[') {
            char set[256];
            int negate = 0;
            if (conv == '[') {
                memset(set, 0, sizeof set);
                p++;
                if (*p == '^') {
                    negate = 1;
                    p++;
                }
                if (*p == ']') {
                    set[']'] = 1;
                    p++;
                }
                for (; *p && *p != ']'; p++) {
                    if (p[1] == '-' && p[2] && p[2] != ']') {
                        for (int ch = (unsigned char)p[0]; ch <= (unsigned char)p[2]; ch++) set[ch] = 1;
                        p += 2;
                    } else {
                        set[(unsigned char)*p] = 1;
                    }
                }
            }
            char *dst = skip ? NULL : va_arg(ap, char *);
            long got = 0;
            while (width < 0 || got < width) {
                c = next(r);
                if (c == EOF) break;
                int ok = conv == 'c' ? 1 : conv == 's' ? !isspace(c) : (set[c] ^ negate);
                if (!ok) {
                    back(r, c);
                    break;
                }
                if (dst) dst[got] = (char)c;
                got++;
            }
            if (!got) return assigned == 0 && c == EOF ? EOF : assigned;
            if (dst && conv != 'c') dst[got] = 0;
            if (dst) assigned++;
            continue;
        }
        /* Numbers: collect a maximal plausible prefix, then convert with strto*. */
        long maxw = width < 0 || width > (long)sizeof buf - 1 ? (long)sizeof buf - 1 : width;
        int is_float = strchr("eEfFgGaA", conv) != NULL;
        int base = conv == 'o' ? 8 : (conv == 'x' || conv == 'X' || conv == 'p') ? 16 : conv == 'i' ? 0 : 10;
        while (n < maxw) {
            c = next(r);
            if (c == EOF) break;
            int ok;
            if (c == '+' || c == '-') ok = n == 0 || (is_float && (buf[n - 1] | 32) == (base == 16 ? 'p' : 'e'));
            else if (is_float) ok = isxdigit(c) || c == '.' || (c | 32) == 'x' || (c | 32) == 'p' || (c | 32) == 'n' ||
                                    (c | 32) == 'i' || (c | 32) == 'a' || (c | 32) == 't' || (c | 32) == 'y';
            else if ((c | 32) == 'x') ok = (base == 16 || base == 0) && n > 0 && buf[n - 1] == '0';
            else ok = isxdigit(c) && (base == 16 || base == 0 ? 1 : (c >= '0' && c < '0' + base));
            if (!ok) {
                back(r, c);
                break;
            }
            if (is_float && (c | 32) == 'x') base = 16;
            buf[n++] = (char)c;
        }
        buf[n] = 0;
        if (!n) return assigned == 0 && c == EOF ? EOF : assigned;
        char *end;
        if (is_float) {
            long double v = strtold(buf, &end);
            if (end == buf) return assigned;
            /* Give back characters strtold did not use (e.g. a trailing 'e'). */
            for (char *q = buf + n; q > end;) back(r, (unsigned char)*--q);
            if (!skip) {
                void *dst = va_arg(ap, void *);
                if (len == 5) *(long double *)dst = v;
                else if (len == 3) *(double *)dst = (double)v;
                else *(float *)dst = (float)v;
                assigned++;
            }
        } else {
            unsigned long long v;
            if (conv == 'd' || conv == 'i') v = (unsigned long long)strtoll(buf, &end, base);
            else v = strtoull(buf, &end, base);
            if (end == buf) return assigned;
            for (char *q = buf + n; q > end;) back(r, (unsigned char)*--q);
            if (!skip) {
                if (conv == 'p') *va_arg(ap, void **) = (void *)(uintptr_t)v;
                else store_int(va_arg(ap, void *), len, v);
                assigned++;
            }
        }
    }
    return assigned;
}

int vsscanf(const char *restrict s, const char *restrict fmt, va_list ap) {
    src r = {NULL, s, 0};
    return vscan(&r, fmt, ap);
}

int vfscanf(FILE *restrict f, const char *restrict fmt, va_list ap) {
    src r = {f, NULL, 0};
    return vscan(&r, fmt, ap);
}

int sscanf(const char *restrict s, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsscanf(s, fmt, ap);
    va_end(ap);
    return n;
}

int fscanf(FILE *restrict f, const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vfscanf(f, fmt, ap);
    va_end(ap);
    return n;
}

int scanf(const char *restrict fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vfscanf(stdin, fmt, ap);
    va_end(ap);
    return n;
}
