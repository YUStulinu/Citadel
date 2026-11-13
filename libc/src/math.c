/* libm on the x87 FPU: transcendental functions are computed in 80-bit extended precision and
   rounded to double, which gives results within an ulp for ordinary arguments. */
#include <errno.h>
#include <math.h>
#include <stdint.h>

static inline long double x87_log2(long double x) { /* fyl2x: st1 * log2(st0) */
    long double r;
    __asm__("fld1; fxch; fyl2x" : "=t"(r) : "0"(x));
    return r;
}

/* 2^x for any finite x: split into integer and fraction, f2xm1 on the fraction, fscale. */
static long double x87_exp2(long double x) {
    if (x > 16384) return __builtin_infl();
    if (x < -16500) return 0;
    long double i, f, r;
    __asm__("frndint" : "=t"(i) : "0"(x));
    f = x - i;
    __asm__("f2xm1" : "=t"(r) : "0"(f));
    r += 1;
    __asm__("fscale" : "=t"(r) : "0"(r), "u"(i));
    return r;
}

double fabs(double x) { return __builtin_fabs(x); }
float fabsf(float x) { return __builtin_fabsf(x); }
long double fabsl(long double x) { return __builtin_fabsl(x); }
double copysign(double x, double y) { return __builtin_copysign(x, y); }
double sqrt(double x) {
    double r;
    __asm__("sqrtsd %1, %0" : "=x"(r) : "x"(x));
    return r;
}
float sqrtf(float x) {
    float r;
    __asm__("sqrtss %1, %0" : "=x"(r) : "x"(x));
    return r;
}

/* Rounding with SSE4.1 is not guaranteed on every CPU; use integer conversion when exact. */
double trunc(double x) {
    if (!(fabs(x) < 0x1p52)) return x;
    double t = (double)(long long)x;
    return copysign(t, x);
}
double floor(double x) {
    double t = trunc(x);
    return t > x ? t - 1 : t;
}
double ceil(double x) {
    double t = trunc(x);
    return t < x ? t + 1 : t;
}
double round(double x) {
    double t = trunc(x);
    if (fabs(x - t) >= 0.5) t += copysign(1.0, x);
    return t;
}
double rint(double x) {
    if (!(fabs(x) < 0x1p52)) return x;
    double t = x >= 0 ? (x + 0x1p52) - 0x1p52 : (x - 0x1p52) + 0x1p52;
    return copysign(t, x);
}
double nearbyint(double x) { return rint(x); }
long lround(double x) { return (long)round(x); }
long long llround(double x) { return (long long)round(x); }
long lrint(double x) { return (long)rint(x); }
float floorf(float x) { return (float)floor(x); }
float ceilf(float x) { return (float)ceil(x); }

double fmod(double x, double y) {
    long double a = x, b = y, r;
    if (y == 0 || __builtin_isinf(x) || __builtin_isnan(y)) return (x * y) / (x * y);
    __asm__("1: fprem; fnstsw %%ax; testb $4, %%ah; jnz 1b" : "=t"(r) : "0"(a), "u"(b) : "ax", "cc");
    return (double)r;
}
float fmodf(float x, float y) { return (float)fmod(x, y); }

double remainder(double x, double y) {
    long double a = x, b = y, r;
    __asm__("1: fprem1; fnstsw %%ax; testb $4, %%ah; jnz 1b" : "=t"(r) : "0"(a), "u"(b) : "ax", "cc");
    return (double)r;
}

double frexp(double x, int *e) {
    union { double d; uint64_t u; } v = {x};
    int be = (int)((v.u >> 52) & 0x7ff);
    if (!be) {
        if (x == 0) {
            *e = 0;
            return x;
        }
        x = frexp(x * 0x1p64, e);
        *e -= 64;
        return x;
    }
    if (be == 0x7ff) {
        *e = 0;
        return x;
    }
    *e = be - 1022;
    v.u = (v.u & ~(0x7ffULL << 52)) | (1022ULL << 52);
    return v.d;
}

long double frexpl(long double x, int *e) {
    if (x == 0 || !__builtin_isfinite(x)) {
        *e = 0;
        return x;
    }
    long double ex, m;
    __asm__("fxtract" : "=t"(m), "=u"(ex) : "0"(x));
    *e = (int)ex + 1;
    return m / 2;
}

double scalbn(double x, int n) {
    long double r = x, s = n;
    __asm__("fscale" : "=t"(r) : "0"(r), "u"(s));
    return (double)r;
}
double ldexp(double x, int n) { return scalbn(x, n); }
long double ldexpl(long double x, int n) {
    long double s = n;
    __asm__("fscale" : "=t"(x) : "0"(x), "u"(s));
    return x;
}

double modf(double x, double *ip) {
    double t = trunc(x);
    *ip = t;
    if (__builtin_isinf(x)) return copysign(0.0, x);
    return copysign(x - t, x);
}

double exp2(double x) { return (double)x87_exp2(x); }
double exp(double x) {
    if (__builtin_isnan(x)) return x;
    return (double)x87_exp2((long double)x * 1.44269504088896340735992468100189214L);
}
double expm1(double x) {
    if (fabs(x) < 1e-5) return x + x * x / 2 + x * x * x / 6;
    return (double)(x87_exp2((long double)x * 1.44269504088896340735992468100189214L) - 1);
}

static double log_domain(double x) {
    if (__builtin_isnan(x)) return x;
    if (x < 0) {
        errno = EDOM;
        return __builtin_nan("");
    }
    if (x == 0) {
        errno = ERANGE;
        return -__builtin_inf();
    }
    return 1; /* fine */
}

double log2(double x) {
    if (!(x > 0) || __builtin_isinf(x)) return __builtin_isinf(x) && x > 0 ? x : log_domain(x);
    return (double)x87_log2(x);
}
double log(double x) {
    if (!(x > 0) || __builtin_isinf(x)) return __builtin_isinf(x) && x > 0 ? x : log_domain(x);
    return (double)(x87_log2(x) * 0.693147180559945309417232121458176568L);
}
double log10(double x) {
    if (!(x > 0) || __builtin_isinf(x)) return __builtin_isinf(x) && x > 0 ? x : log_domain(x);
    return (double)(x87_log2(x) * 0.301029995663981195213738894724493027L);
}
double log1p(double x) {
    if (fabs(x) < 1e-5) return x - x * x / 2 + x * x * x / 3;
    return log(1 + x);
}

double pow(double x, double y) {
    if (y == 0) return 1;
    if (x == 1) return 1;
    if (__builtin_isnan(x) || __builtin_isnan(y)) return x + y;
    int yint = y == trunc(y);
    int yodd = yint && fabs(y) < 0x1p53 && ((long long)y & 1);
    if (x == 0) {
        if (y < 0) {
            errno = ERANGE;
            return yodd ? copysign(__builtin_inf(), x) : __builtin_inf();
        }
        return yodd ? x : 0.0;
    }
    if (__builtin_isinf(y)) {
        if (x == -1) return 1;
        return (fabs(x) > 1) == (y > 0) ? __builtin_inf() : 0.0;
    }
    if (__builtin_isinf(x)) {
        double r = y > 0 ? __builtin_inf() : 0.0;
        return x < 0 && yodd ? -r : r;
    }
    if (x < 0 && !yint) {
        errno = EDOM;
        return __builtin_nan("");
    }
    /* Small integer powers by repeated squaring are exact where possible. */
    if (yint && fabs(y) <= 64) {
        long double r = 1, b = x;
        long long n = (long long)fabs(y);
        while (n) {
            if (n & 1) r *= b;
            b *= b;
            n >>= 1;
        }
        return (double)(y < 0 ? 1 / r : r);
    }
    long double r = x87_exp2((long double)y * x87_log2(fabsl(x)));
    if (x < 0 && yodd) r = -r;
    double d = (double)r;
    if (__builtin_isinf(d) || d == 0) errno = ERANGE;
    return d;
}
float powf(float x, float y) { return (float)pow(x, y); }

double cbrt(double x) {
    if (x == 0 || !__builtin_isfinite(x)) return x;
    long double r = x87_exp2(x87_log2(fabsl(x)) / 3);
    r = r - (r * r * r - fabsl(x)) / (3 * r * r); /* one Newton step */
    return copysign((double)r, x);
}

double hypot(double x, double y) {
    if (__builtin_isinf(x) || __builtin_isinf(y)) return __builtin_inf();
    long double a = x, b = y;
    long double s = a * a + b * b, r;
    __asm__("fsqrt" : "=t"(r) : "0"(s));
    return (double)r;
}

/* Trigonometry: fprem1 reduces modulo 2*pi in extended precision; fsin/fcos then apply. */
static long double reduce(long double x) {
    if (fabsl(x) < 0x1p62L) return x;
    long double two_pi = 6.28318530717958647692528676655900577L, r;
    __asm__("1: fprem1; fnstsw %%ax; testb $4, %%ah; jnz 1b" : "=t"(r) : "0"(x), "u"(two_pi) : "ax", "cc");
    return r;
}
double sin(double x) {
    long double r = reduce(x);
    __asm__("fsin" : "+t"(r));
    return (double)r;
}
double cos(double x) {
    long double r = reduce(x);
    __asm__("fcos" : "+t"(r));
    return (double)r;
}
double tan(double x) {
    long double r = reduce(x), one;
    __asm__("fptan" : "=t"(one), "=u"(r) : "0"(r));
    return (double)r;
}
double atan2(double y, double x) {
    long double r;
    __asm__("fpatan" : "=t"(r) : "0"((long double)x), "u"((long double)y) : "st(1)");
    return (double)r;
}
double atan(double x) { return atan2(x, 1.0); }
double asin(double x) {
    if (fabs(x) > 1) {
        errno = EDOM;
        return __builtin_nan("");
    }
    long double s = 1.0L - (long double)x * x, c;
    __asm__("fsqrt" : "=t"(c) : "0"(s));
    long double r;
    __asm__("fpatan" : "=t"(r) : "0"(c), "u"((long double)x) : "st(1)");
    return (double)r;
}
double acos(double x) {
    if (fabs(x) > 1) {
        errno = EDOM;
        return __builtin_nan("");
    }
    long double s = 1.0L - (long double)x * x, c;
    __asm__("fsqrt" : "=t"(c) : "0"(s));
    long double r;
    __asm__("fpatan" : "=t"(r) : "0"((long double)x), "u"(c) : "st(1)");
    return (double)r;
}
double sinh(double x) {
    if (fabs(x) < 1e-5) return x;
    long double e = x87_exp2((long double)x * 1.44269504088896340736L);
    return (double)((e - 1 / e) / 2);
}
double cosh(double x) {
    long double e = x87_exp2((long double)x * 1.44269504088896340736L);
    return (double)((e + 1 / e) / 2);
}
double tanh(double x) {
    if (fabs(x) > 22) return copysign(1.0, x);
    if (fabs(x) < 1e-5) return x;
    long double e = x87_exp2(2.0L * x * 1.44269504088896340736L);
    return (double)((e - 1) / (e + 1));
}
double fmin(double a, double b) { return __builtin_isnan(a) ? b : __builtin_isnan(b) ? a : a < b ? a : b; }
double fmax(double a, double b) { return __builtin_isnan(a) ? b : __builtin_isnan(b) ? a : a > b ? a : b; }
