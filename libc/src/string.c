/* string.h, strings.h, ctype.h */
#include <ctype.h>
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

void *memcpy(void *restrict d, const void *restrict s, size_t n) {
    void *ret = d;
    __asm__ volatile("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory");
    return ret;
}

void *memmove(void *d, const void *s, size_t n) {
    unsigned char *dp = d;
    const unsigned char *sp = s;
    if (dp == sp || n == 0) return d;
    if (dp < sp || dp >= sp + n) return memcpy(d, s, n);
    /* Overlap with the destination above the source: copy backwards. */
    dp += n - 1;
    sp += n - 1;
    __asm__ volatile("std; rep movsb; cld" : "+D"(dp), "+S"(sp), "+c"(n) : : "memory");
    return d;
}

void *memset(void *d, int c, size_t n) {
    void *ret = d;
    __asm__ volatile("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory");
    return ret;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; i++)
        if (x[i] != y[i]) return x[i] - y[i];
    return 0;
}

void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    for (size_t i = 0; i < n; i++)
        if (p[i] == (unsigned char)c) return (void *)(p + i);
    return NULL;
}

size_t strlen(const char *s) {
    const char *p = s;
    while (*p) p++;
    return p - s;
}

size_t strnlen(const char *s, size_t n) {
    size_t i = 0;
    while (i < n && s[i]) i++;
    return i;
}

char *strcpy(char *restrict d, const char *restrict s) {
    char *r = d;
    while ((*d++ = *s++)) {}
    return r;
}

char *strncpy(char *restrict d, const char *restrict s, size_t n) {
    size_t i = 0;
    for (; i < n && s[i]; i++) d[i] = s[i];
    for (; i < n; i++) d[i] = 0;
    return d;
}

char *strcat(char *restrict d, const char *restrict s) {
    strcpy(d + strlen(d), s);
    return d;
}

char *strncat(char *restrict d, const char *restrict s, size_t n) {
    char *e = d + strlen(d);
    size_t i = 0;
    for (; i < n && s[i]; i++) e[i] = s[i];
    e[i] = 0;
    return d;
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    for (; n; n--, a++, b++) {
        if (*a != *b) return (unsigned char)*a - (unsigned char)*b;
        if (!*a) return 0;
    }
    return 0;
}

int strcoll(const char *a, const char *b) { return strcmp(a, b); }

size_t strxfrm(char *restrict d, const char *restrict s, size_t n) {
    size_t l = strlen(s);
    if (n) strncpy(d, s, n);
    return l;
}

char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c) return (char *)s;
        if (!*s) return NULL;
    }
}

char *strrchr(const char *s, int c) {
    const char *r = NULL;
    for (;; s++) {
        if (*s == (char)c) r = s;
        if (!*s) return (char *)r;
    }
}

char *strstr(const char *h, const char *n) {
    size_t l = strlen(n);
    if (!l) return (char *)h;
    for (; *h; h++)
        if (*h == *n && !strncmp(h, n, l)) return (char *)h;
    return NULL;
}

size_t strspn(const char *s, const char *a) {
    size_t i = 0;
    while (s[i] && strchr(a, s[i])) i++;
    return i;
}

size_t strcspn(const char *s, const char *r) {
    size_t i = 0;
    while (s[i] && !strchr(r, s[i])) i++;
    return i;
}

char *strpbrk(const char *s, const char *a) {
    s += strcspn(s, a);
    return *s ? (char *)s : NULL;
}

char *strtok_r(char *restrict s, const char *restrict d, char **restrict save) {
    if (!s) s = *save;
    if (!s) return NULL;
    s += strspn(s, d);
    if (!*s) {
        *save = NULL;
        return NULL;
    }
    char *e = s + strcspn(s, d);
    if (*e) {
        *e = 0;
        *save = e + 1;
    } else {
        *save = NULL;
    }
    return s;
}

char *strtok(char *restrict s, const char *restrict d) {
    static char *save;
    return strtok_r(s, d, &save);
}

char *strdup(const char *s) {
    size_t l = strlen(s) + 1;
    char *d = malloc(l);
    if (d) memcpy(d, s, l);
    return d;
}

char *strndup(const char *s, size_t n) {
    size_t l = strnlen(s, n);
    char *d = malloc(l + 1);
    if (!d) return NULL;
    memcpy(d, s, l);
    d[l] = 0;
    return d;
}

int strcasecmp(const char *a, const char *b) {
    while (*a && tolower((unsigned char)*a) == tolower((unsigned char)*b)) a++, b++;
    return tolower((unsigned char)*a) - tolower((unsigned char)*b);
}

int strncasecmp(const char *a, const char *b, size_t n) {
    for (; n; n--, a++, b++) {
        int x = tolower((unsigned char)*a), y = tolower((unsigned char)*b);
        if (x != y) return x - y;
        if (!x) return 0;
    }
    return 0;
}

static const char *const errors[] = {
    [0] = "Success", [EPERM] = "Operation not permitted", [ENOENT] = "No such file or directory",
    [ESRCH] = "No such process", [EINTR] = "Interrupted system call", [EIO] = "I/O error",
    [ENXIO] = "No such device or address", [E2BIG] = "Argument list too long", [ENOEXEC] = "Exec format error",
    [EBADF] = "Bad file descriptor", [ECHILD] = "No child processes", [EAGAIN] = "Resource temporarily unavailable",
    [ENOMEM] = "Out of memory", [EACCES] = "Permission denied", [EFAULT] = "Bad address", [EBUSY] = "Resource busy",
    [EEXIST] = "File exists", [EXDEV] = "Cross-device link", [ENODEV] = "No such device", [ENOTDIR] = "Not a directory",
    [EISDIR] = "Is a directory", [EINVAL] = "Invalid argument", [ENFILE] = "Too many open files in system",
    [EMFILE] = "Too many open files", [ENOTTY] = "Not a tty", [EFBIG] = "File too large", [ENOSPC] = "No space left on device",
    [ESPIPE] = "Invalid seek", [EROFS] = "Read-only file system", [EMLINK] = "Too many links", [EPIPE] = "Broken pipe",
    [EDOM] = "Domain error", [ERANGE] = "Result not representable", [ENAMETOOLONG] = "Filename too long",
    [ENOSYS] = "Function not implemented", [ENOTEMPTY] = "Directory not empty", [ELOOP] = "Symbolic link loop",
    [ENOTSOCK] = "Not a socket", [EADDRINUSE] = "Address in use", [ECONNREFUSED] = "Connection refused",
    [ECONNRESET] = "Connection reset by peer", [ENOTCONN] = "Socket not connected", [ETIMEDOUT] = "Operation timed out",
    [EHOSTUNREACH] = "Host is unreachable", [ENETUNREACH] = "Network unreachable", [EINPROGRESS] = "Operation in progress",
};

char *strerror(int e) {
    static char buf[32];
    if (e >= 0 && e < (int)(sizeof errors / sizeof *errors) && errors[e]) return (char *)errors[e];
    char *p = buf + sizeof buf - 1;
    unsigned u = e < 0 ? -e : e;
    *p = 0;
    do *--p = '0' + u % 10; while (u /= 10);
    if (e < 0) *--p = '-';
    memcpy(buf, "Unknown error ", 14);
    memmove(buf + 14, p, strlen(p) + 1);
    return buf;
}

char *strsignal(int sig) {
    static const char *const names[] = {"Unknown signal", "Hangup", "Interrupt", "Quit", "Illegal instruction", "Trace trap",
                                        "Aborted", "Bus error", "Floating point exception", "Killed", "User signal 1",
                                        "Segmentation fault", "User signal 2", "Broken pipe", "Alarm clock", "Terminated"};
    return (char *)(sig > 0 && sig < 16 ? names[sig] : names[0]);
}

/* ctype (ASCII; the C locale is the only locale) */
int isdigit(int c) { return (unsigned)c - '0' < 10; }
int islower(int c) { return (unsigned)c - 'a' < 26; }
int isupper(int c) { return (unsigned)c - 'A' < 26; }
int isalpha(int c) { return islower(c) || isupper(c); }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isxdigit(int c) { return isdigit(c) || (unsigned)(c | 32) - 'a' < 6; }
int isspace(int c) { return c == ' ' || (unsigned)c - '\t' < 5; }
int isblank(int c) { return c == ' ' || c == '\t'; }
int iscntrl(int c) { return (unsigned)c < 32 || c == 127; }
int isprint(int c) { return (unsigned)c - 32 < 95; }
int isgraph(int c) { return (unsigned)c - 33 < 94; }
int ispunct(int c) { return isgraph(c) && !isalnum(c); }
int tolower(int c) { return isupper(c) ? c | 32 : c; }
int toupper(int c) { return islower(c) ? c & 0x5f : c; }
