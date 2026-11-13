/* malloc: power-of-two size classes carved from the brk heap, mmap for large blocks.
   Every block has a 16-byte header holding its usable size; free blocks of a class form a
   singly linked list. Not thread-safe (Citadel processes are single-threaded). */
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define HDR 16
#define MIN_SHIFT 4  /* 16 bytes */
#define MAX_SHIFT 17 /* 128 KiB; larger requests use mmap */
#define MMAP_FLAG 1UL

struct hdr {
    size_t size; /* usable size; low bit set for mmap blocks */
    size_t pad;
};

static void *bins[MAX_SHIFT + 1];
static char *arena_cur, *arena_end;

static int class_of(size_t n) {
    int s = MIN_SHIFT;
    while (((size_t)1 << s) < n) s++;
    return s;
}

static void *arena_take(size_t n) {
    if ((size_t)(arena_end - arena_cur) < n) {
        size_t grow = n > 256 * 1024 ? (n + 4095) & ~(size_t)4095 : 256 * 1024;
        char *p = sbrk(grow);
        if (p == (char *)-1) return NULL;
        if (p != arena_end) arena_cur = p; /* non-contiguous: abandon the tail */
        arena_end = p + grow;
    }
    void *r = arena_cur;
    arena_cur += n;
    return r;
}

void *malloc(size_t n) {
    if (n == 0) n = 1;
    if (n > ((size_t)1 << MAX_SHIFT)) {
        if (n > SIZE_MAX / 2) {
            errno = ENOMEM;
            return NULL;
        }
        size_t total = (n + HDR + 4095) & ~(size_t)4095;
        struct hdr *h = mmap(NULL, total, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (h == MAP_FAILED) {
            errno = ENOMEM;
            return NULL;
        }
        h->size = (total - HDR) | MMAP_FLAG;
        return h + 1;
    }
    int c = class_of(n);
    void *p = bins[c];
    if (p) {
        bins[c] = *(void **)p;
        return p;
    }
    struct hdr *h = arena_take(HDR + ((size_t)1 << c));
    if (!h) {
        errno = ENOMEM;
        return NULL;
    }
    h->size = (size_t)1 << c;
    return h + 1;
}

void free(void *p) {
    if (!p) return;
    struct hdr *h = (struct hdr *)p - 1;
    if (h->size & MMAP_FLAG) {
        uintptr_t start = (uintptr_t)h & ~(uintptr_t)4095, end = (uintptr_t)p + (h->size & ~MMAP_FLAG);
        munmap((void *)start, end - start);
        return;
    }
    int c = class_of(h->size);
    *(void **)p = bins[c];
    bins[c] = p;
}

void *calloc(size_t n, size_t sz) {
    if (sz && n > SIZE_MAX / sz) {
        errno = ENOMEM;
        return NULL;
    }
    void *p = malloc(n * sz);
    if (p) memset(p, 0, n * sz);
    return p;
}

void *realloc(void *p, size_t n) {
    if (!p) return malloc(n);
    if (!n) {
        free(p);
        return NULL;
    }
    size_t have = ((struct hdr *)p - 1)->size & ~MMAP_FLAG;
    if (n <= have) return p;
    void *q = malloc(n);
    if (!q) return NULL;
    memcpy(q, p, have);
    free(p);
    return q;
}

/* Alignment beyond 16 bytes: over-allocate from mmap and keep the whole mapping (rare). */
void *aligned_alloc(size_t al, size_t n) {
    if (al <= 16) return malloc(n);
    if (al & (al - 1)) {
        errno = EINVAL;
        return NULL;
    }
    size_t total = (n + al + HDR + 4095) & ~(size_t)4095;
    char *m = mmap(NULL, total, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) {
        errno = ENOMEM;
        return NULL;
    }
    uintptr_t u = ((uintptr_t)m + HDR + al - 1) & ~(uintptr_t)(al - 1);
    struct hdr *h = (struct hdr *)u - 1;
    /* Leaks the leading slack on free (munmap of the header page only covers from h). */
    h->size = (total - (u - (uintptr_t)m)) | MMAP_FLAG;
    return (void *)u;
}

int posix_memalign(void **out, size_t al, size_t n) {
    void *p = aligned_alloc(al, n);
    if (!p) return ENOMEM;
    *out = p;
    return 0;
}
