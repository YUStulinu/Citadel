/* Prints a large deterministic set of printf/strtod/scanf results. The same source is built
   against Citadel's libc and against the host libc; the two outputs must be identical. */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint64_t state = 0x9e3779b97f4a7c15ULL;
static uint64_t next(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

static double random_double(void) {
    for (;;) {
        uint64_t u = next();
        double d;
        memcpy(&d, &u, sizeof d);
        if (isfinite(d)) return d;
    }
}

int main(void) {
    static const char *const ffmts[] = {"%.17g", "%g", "%.3g", "%.0g", "%#g", "%e", "%.0e", "%.12e", "%E",
                                        "%f", "%.0f", "%.2f", "%#.0f", "%+.5f", "% 12.3f", "%-12.3e|", "%012.4f",
                                        "%.14g", "%a", "%.3a", "%A"};
    static const double fixed[] = {0.0, -0.0, 1.0, -1.0, 0.5, 1.5, 2.5, 0.1, 0.125, 1e-5, 123456789.0, 1e21, 1e22,
                                   9.999999999999999e22, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308,
                                   3.141592653589793, 2.718281828459045, 0.3, 100.0, 1e-300, 4.35, 0.05, 1234.5};
    unsigned long mismatch_rt = 0;
    for (unsigned i = 0; i < sizeof fixed / sizeof *fixed + 3000; i++) {
        double d = i < sizeof fixed / sizeof *fixed ? fixed[i] : random_double();
        if (i >= sizeof fixed / sizeof *fixed && i % 3 == 0) d = (double)(next() % 2000000) / 1000.0 - 1000;
        for (unsigned k = 0; k < sizeof ffmts / sizeof *ffmts; k++) {
            /* %f of huge numbers is long but exact; keep it to moderate magnitudes */
            if (strchr(ffmts[k], 'f') && fabs(d) > 1e40) continue;
            char buf[512];
            snprintf(buf, sizeof buf, ffmts[k], d);
            printf("%s ", buf);
        }
        putchar('\n');
        char buf[64];
        snprintf(buf, sizeof buf, "%.17g", d);
        double back = strtod(buf, NULL);
        if (memcmp(&back, &d, sizeof d)) mismatch_rt++;
    }
    static const char *const ifmts[] = {"%d", "%5d", "%-5d|", "%05d", "%+d", "% d", "%x", "%#x", "%#o", "%o", "%X",
                                        "%.3d", "%8.3d", "%u", "%hhd", "%hd", "%c"};
    static const int ints[] = {0, 1, -1, 42, -42, 255, 256, 65535, 65536, 2147483647, -2147483647 - 1, 7};
    for (unsigned i = 0; i < sizeof ints / sizeof *ints; i++) {
        for (unsigned k = 0; k < sizeof ifmts / sizeof *ifmts; k++) {
            if (!strcmp(ifmts[k], "%c") && (ints[i] < 32 || ints[i] > 126)) continue;
            printf(ifmts[k], ints[i]);
            putchar(' ');
        }
        putchar('\n');
    }
    printf("%lld %llu %llx %ld %zu %s %.2s %5s %-5s| %%\n", -9223372036854775807LL - 1, 18446744073709551615ULL,
           0xdeadbeefcafeULL, -5L, (size_t)12345, "str", "abc", "ab", "ab");
    /* scanf */
    int a, n;
    unsigned x;
    double f;
    char word[32], set[32];
    int got = sscanf("  -17 0x1f 2.5e3 hello abc123 rest", "%d %x %lf %31s %31[a-c]%n", &a, &x, &f, word, set, &n);
    printf("sscanf=%d a=%d x=%u f=%g word=%s set=%s n=%d\n", got, a, x, f, word, set, n);
    /* strtod on decimal strings: count values not correctly rounded */
    unsigned long mismatch_dec = 0;
    for (int i = 0; i < 20000; i++) {
        char s[64];
        unsigned long long ip = next() % 100000000, fp = next() % 1000000;
        int ex = (int)(next() % 600) - 300; /* separate statements: argument order is unspecified */
        snprintf(s, sizeof s, "%llu.%06llue%d", ip, fp, ex);
        double v = strtod(s, NULL);
        char r[64];
        snprintf(r, sizeof r, "%.17g", v);
        printf("%s\n", r);
        (void)mismatch_dec;
    }
    fprintf(stderr, "round-trip mismatches: %lu\n", mismatch_rt);
    return 0;
}
