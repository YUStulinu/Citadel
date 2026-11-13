/* Directories, getopt, locale, libgen, byte order and IPv4 address helpers. */
#include <arpa/inet.h>
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <libgen.h>
#include <locale.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "syscall.h"

/* ------------------------------------------------------------ dirent */

struct DIR {
    int fd;
    size_t pos, end;
    struct dirent ent;
    char buf[4096];
};

struct linux_dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

DIR *fdopendir(int fd) {
    DIR *d = calloc(1, sizeof *d);
    if (!d) return NULL;
    d->fd = fd;
    return d;
}

DIR *opendir(const char *path) {
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd < 0) return NULL;
    DIR *d = fdopendir(fd);
    if (!d) close(fd);
    return d;
}

struct dirent *readdir(DIR *d) {
    if (d->pos >= d->end) {
        long n = __sc3(SYS_getdents64, d->fd, d->buf, sizeof d->buf);
        if (n <= 0) {
            if (n < 0) errno = (int)-n;
            return NULL;
        }
        d->pos = 0;
        d->end = (size_t)n;
    }
    struct linux_dirent64 *e = (struct linux_dirent64 *)(d->buf + d->pos);
    d->pos += e->d_reclen;
    d->ent.d_ino = e->d_ino;
    d->ent.d_off = e->d_off;
    d->ent.d_reclen = sizeof d->ent;
    d->ent.d_type = e->d_type;
    strncpy(d->ent.d_name, e->d_name, sizeof d->ent.d_name - 1);
    d->ent.d_name[sizeof d->ent.d_name - 1] = 0;
    return &d->ent;
}

int closedir(DIR *d) {
    int r = close(d->fd);
    free(d);
    return r;
}

void rewinddir(DIR *d) {
    lseek(d->fd, 0, SEEK_SET);
    d->pos = d->end = 0;
}

int dirfd(DIR *d) { return d->fd; }

/* ------------------------------------------------------------ getopt */

char *optarg;
int optind = 1, opterr = 1, optopt;
static int optpos = 1;

int getopt(int argc, char *const argv[], const char *opts) {
    if (optind == 0) {
        optind = 1;
        optpos = 1;
    }
    if (optind >= argc || !argv[optind] || argv[optind][0] != '-' || !argv[optind][1]) return -1;
    if (!strcmp(argv[optind], "--")) {
        optind++;
        return -1;
    }
    int c = (unsigned char)argv[optind][optpos];
    const char *spec = strchr(opts, c);
    int colon = opts[0] == ':';
    if (!spec || c == ':') {
        optopt = c;
        if (opterr && !colon) fprintf(stderr, "%s: unknown option -%c\n", argv[0], c);
        if (!argv[optind][++optpos]) {
            optind++;
            optpos = 1;
        }
        return '?';
    }
    if (spec[1] == ':') {
        if (argv[optind][optpos + 1]) {
            optarg = argv[optind] + optpos + 1;
        } else if (optind + 1 < argc) {
            optarg = argv[++optind];
        } else {
            optopt = c;
            optind++;
            optpos = 1;
            if (opterr && !colon) fprintf(stderr, "%s: option -%c requires an argument\n", argv[0], c);
            return colon ? ':' : '?';
        }
        optind++;
        optpos = 1;
        return c;
    }
    if (!argv[optind][++optpos]) {
        optind++;
        optpos = 1;
    }
    return c;
}

/* ------------------------------------------------------------ locale (C only) */

static struct lconv c_lconv = {".", "", "", "", "", "", "", "", "", "", 127, 127, 127, 127, 127, 127, 127, 127};

char *setlocale(int cat, const char *loc) {
    (void)cat;
    if (!loc || !*loc || !strcmp(loc, "C") || !strcmp(loc, "POSIX")) return "C";
    return NULL;
}

struct lconv *localeconv(void) { return &c_lconv; }

/* ------------------------------------------------------------ libgen */

char *basename(char *p) {
    if (!p || !*p) return ".";
    size_t l = strlen(p);
    while (l > 1 && p[l - 1] == '/') p[--l] = 0;
    char *s = strrchr(p, '/');
    return s && s[1] ? s + 1 : p;
}

char *dirname(char *p) {
    if (!p || !*p) return ".";
    size_t l = strlen(p);
    while (l > 1 && p[l - 1] == '/') l--;
    while (l > 0 && p[l - 1] != '/') l--;
    if (!l) return ".";
    while (l > 1 && p[l - 1] == '/') l--;
    p[l] = 0;
    return p;
}

/* ------------------------------------------------------------ byte order, inet */

uint16_t htons(uint16_t x) { return __builtin_bswap16(x); }
uint16_t ntohs(uint16_t x) { return __builtin_bswap16(x); }
uint32_t htonl(uint32_t x) { return __builtin_bswap32(x); }
uint32_t ntohl(uint32_t x) { return __builtin_bswap32(x); }

int inet_aton(const char *s, struct in_addr *a) {
    unsigned long parts[4];
    int n = 0;
    for (;;) {
        if (!isdigit((unsigned char)*s)) return 0;
        char *end;
        parts[n++] = strtoul(s, &end, 0);
        s = end;
        if (*s != '.' || n == 4) break;
        s++;
    }
    if (*s) return 0;
    unsigned long v;
    switch (n) {
    case 1: v = parts[0]; break;
    case 2: if (parts[0] > 255 || parts[1] > 0xffffff) return 0; v = parts[0] << 24 | parts[1]; break;
    case 3:
        if (parts[0] > 255 || parts[1] > 255 || parts[2] > 0xffff) return 0;
        v = parts[0] << 24 | parts[1] << 16 | parts[2];
        break;
    default:
        for (int i = 0; i < 4; i++)
            if (parts[i] > 255) return 0;
        v = parts[0] << 24 | parts[1] << 16 | parts[2] << 8 | parts[3];
    }
    if (v > 0xffffffffUL) return 0;
    a->s_addr = htonl((uint32_t)v);
    return 1;
}

in_addr_t inet_addr(const char *s) {
    struct in_addr a;
    return inet_aton(s, &a) ? a.s_addr : (in_addr_t)-1;
}

char *inet_ntoa(struct in_addr a) {
    static char buf[16];
    unsigned char *b = (unsigned char *)&a.s_addr;
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]);
    return buf;
}

int inet_pton(int af, const char *restrict s, void *restrict dst) {
    if (af != AF_INET) {
        errno = EAFNOSUPPORT;
        return -1;
    }
    unsigned char out[4];
    for (int i = 0; i < 4; i++) {
        if (!isdigit((unsigned char)*s)) return 0;
        int v = 0, digits = 0;
        while (isdigit((unsigned char)*s)) {
            v = v * 10 + (*s++ - '0');
            if (++digits > 3 || v > 255) return 0;
        }
        out[i] = (unsigned char)v;
        if (i < 3 && *s++ != '.') return 0;
    }
    if (*s) return 0;
    memcpy(dst, out, 4);
    return 1;
}

const char *inet_ntop(int af, const void *restrict src, char *restrict dst, socklen_t n) {
    if (af != AF_INET) {
        errno = EAFNOSUPPORT;
        return NULL;
    }
    const unsigned char *b = src;
    if (snprintf(dst, n, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]) >= (int)n) {
        errno = ENOSPC;
        return NULL;
    }
    return dst;
}
