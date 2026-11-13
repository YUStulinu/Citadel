/* Buffered streams. One buffer per stream, used for either reading or writing at a time. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

struct FILE {
    int fd;
    int flags;       /* open flags (O_ACCMODE, O_APPEND) */
    int mode;        /* _IOFBF, _IOLBF, _IONBF */
    int reading;     /* buffer holds read-ahead data */
    int eof, err;
    char *buf;
    size_t size;
    size_t pos, end; /* read: [pos, end) unread; write: [0, pos) pending */
    int own_buf;
    int unget;       /* pushed-back character or -1 */
    pid_t pid;       /* popen child */
    struct FILE *next;
};

static char stdin_buf[BUFSIZ], stdout_buf[BUFSIZ];
static FILE f_in = {0, O_RDONLY, _IOLBF, 0, 0, 0, stdin_buf, BUFSIZ, 0, 0, 0, -1, 0, 0};
static FILE f_out = {1, O_WRONLY, _IOLBF, 0, 0, 0, stdout_buf, BUFSIZ, 0, 0, 0, -1, 0, 0};
static FILE f_err = {2, O_WRONLY, _IONBF, 0, 0, 0, NULL, 0, 0, 0, 0, -1, 0, 0};
FILE *stdin = &f_in, *stdout = &f_out, *stderr = &f_err;
static FILE *open_files;

void __stdio_init(void) {
    /* stdout is line buffered on a terminal, fully buffered otherwise. */
    if (!isatty(1)) f_out.mode = _IOFBF;
}

static int flush_write(FILE *f) {
    size_t off = 0;
    while (off < f->pos) {
        ssize_t w = write(f->fd, f->buf + off, f->pos - off);
        if (w < 0) {
            if (errno == EINTR) continue;
            f->err = 1;
            memmove(f->buf, f->buf + off, f->pos - off);
            f->pos -= off;
            return EOF;
        }
        off += w;
    }
    f->pos = 0;
    return 0;
}

int fflush(FILE *f) {
    if (!f) {
        int r = 0;
        if (fflush(stdout)) r = EOF;
        if (fflush(stderr)) r = EOF;
        for (FILE *g = open_files; g; g = g->next)
            if (fflush(g)) r = EOF;
        return r;
    }
    if (f->reading) {
        /* Give unread input back to the file position when seekable. */
        off_t back = (off_t)(f->end - f->pos) + (f->unget >= 0);
        if (back) lseek(f->fd, -back, SEEK_CUR);
        f->pos = f->end = 0;
        f->unget = -1;
        f->reading = 0;
        return 0;
    }
    return flush_write(f);
}

void __stdio_flush_all(void) { fflush(NULL); }

static int parse_mode(const char *m) {
    int fl;
    switch (*m) {
    case 'r': fl = O_RDONLY; break;
    case 'w': fl = O_WRONLY | O_CREAT | O_TRUNC; break;
    case 'a': fl = O_WRONLY | O_CREAT | O_APPEND; break;
    default: errno = EINVAL; return -1;
    }
    for (m++; *m; m++) {
        if (*m == '+') fl = (fl & ~O_ACCMODE) | O_RDWR;
        else if (*m == 'x') fl |= O_EXCL;
        else if (*m == 'e') fl |= O_CLOEXEC;
    }
    return fl;
}

static FILE *make(int fd, int fl) {
    FILE *f = calloc(1, sizeof *f);
    if (!f) return NULL;
    f->fd = fd;
    f->flags = fl;
    f->mode = isatty(fd) ? _IOLBF : _IOFBF;
    f->unget = -1;
    f->next = open_files;
    open_files = f;
    return f;
}

FILE *fopen(const char *restrict path, const char *restrict mode) {
    int fl = parse_mode(mode);
    if (fl < 0) return NULL;
    int fd = open(path, fl, 0666);
    if (fd < 0) return NULL;
    FILE *f = make(fd, fl);
    if (!f) close(fd);
    return f;
}

FILE *fdopen(int fd, const char *mode) {
    int fl = parse_mode(mode);
    if (fl < 0) return NULL;
    if (fl & O_APPEND) lseek(fd, 0, SEEK_END);
    return make(fd, fl);
}

FILE *freopen(const char *restrict path, const char *restrict mode, FILE *restrict f) {
    fflush(f);
    int fl = parse_mode(mode);
    if (fl < 0) return NULL;
    int fd = open(path, fl, 0666);
    if (fd < 0) return NULL;
    if (fd != f->fd) {
        dup2(fd, f->fd);
        close(fd);
    }
    f->flags = fl;
    f->eof = f->err = 0;
    f->unget = -1;
    return f;
}

static void unlink_file(FILE *f) {
    for (FILE **pp = &open_files; *pp; pp = &(*pp)->next)
        if (*pp == f) {
            *pp = f->next;
            break;
        }
}

int fclose(FILE *f) {
    int r = fflush(f);
    if (close(f->fd) < 0) r = EOF;
    if (f == stdin || f == stdout || f == stderr) return r;
    unlink_file(f);
    if (f->own_buf) free(f->buf);
    free(f);
    return r;
}

FILE *tmpfile(void) {
    char name[] = "/tmp/tmpfileXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return NULL;
    unlink(name);
    return make(fd, O_RDWR);
}

char *tmpnam(char *s) {
    static char buf[L_tmpnam];
    static unsigned seq;
    if (!s) s = buf;
    snprintf(s, L_tmpnam, "/tmp/tmp.%d.%u", getpid(), seq++);
    return s;
}

static int ensure_buf(FILE *f) {
    if (f->buf || f->mode == _IONBF) return 0;
    f->buf = malloc(BUFSIZ);
    if (!f->buf) {
        f->mode = _IONBF;
        return 0;
    }
    f->size = BUFSIZ;
    f->own_buf = 1;
    return 0;
}

int setvbuf(FILE *restrict f, char *restrict buf, int mode, size_t size) {
    if (f->own_buf) free(f->buf);
    f->own_buf = 0;
    f->mode = mode;
    if (mode == _IONBF) {
        f->buf = NULL;
        f->size = 0;
    } else if (buf) {
        f->buf = buf;
        f->size = size;
    } else {
        f->buf = NULL;
        f->size = 0;
    }
    return 0;
}

void setbuf(FILE *restrict f, char *restrict buf) { setvbuf(f, buf, buf ? _IOFBF : _IONBF, BUFSIZ); }

/* ------------------------------------------------------------ reading */

static int refill(FILE *f) {
    if (!f->reading) {
        if (f->pos && flush_write(f)) return EOF;
        f->reading = 1;
        f->pos = f->end = 0;
    }
    /* Reading from a line-buffered stream flushes stdout first (prompts appear). */
    if (f == stdin && f_out.pos && !f_out.reading) flush_write(&f_out);
    ensure_buf(f);
    char one;
    char *dst = f->buf ? f->buf : &one;
    size_t cap = f->buf ? f->size : 1;
    ssize_t r;
    do r = read(f->fd, dst, cap);
    while (r < 0 && errno == EINTR);
    if (r <= 0) {
        if (r == 0) f->eof = 1;
        else f->err = 1;
        return EOF;
    }
    if (!f->buf) {
        f->unget = (unsigned char)one;
        return 0;
    }
    f->pos = 0;
    f->end = r;
    return 0;
}

int fgetc(FILE *f) {
    if (f->unget >= 0) {
        int c = f->unget;
        f->unget = -1;
        return c;
    }
    if (!f->reading || f->pos == f->end) {
        if (refill(f)) return EOF;
        if (f->unget >= 0) {
            int c = f->unget;
            f->unget = -1;
            return c;
        }
    }
    return (unsigned char)f->buf[f->pos++];
}

int getc(FILE *f) { return fgetc(f); }
int getchar(void) { return fgetc(stdin); }

int ungetc(int c, FILE *f) {
    if (c == EOF) return EOF;
    if (f->reading && f->pos > 0 && f->unget < 0) {
        f->buf[--f->pos] = (char)c;
    } else {
        if (f->unget >= 0) return EOF;
        f->unget = (unsigned char)c;
    }
    if (!f->reading) {
        if (f->pos && flush_write(f)) return EOF;
        f->reading = 1;
        f->pos = f->end = 0;
    }
    f->eof = 0;
    return (unsigned char)c;
}

size_t fread(void *restrict p, size_t sz, size_t n, FILE *restrict f) {
    size_t want = sz * n, got = 0;
    char *d = p;
    if (!want) return 0;
    if (f->unget >= 0) {
        d[got++] = (char)f->unget;
        f->unget = -1;
    }
    while (got < want) {
        if (f->reading && f->pos < f->end) {
            size_t m = f->end - f->pos;
            if (m > want - got) m = want - got;
            memcpy(d + got, f->buf + f->pos, m);
            f->pos += m;
            got += m;
            continue;
        }
        if (want - got >= f->size && f->reading) {
            /* Large read: bypass the buffer. */
            ssize_t r = read(f->fd, d + got, want - got);
            if (r < 0 && errno == EINTR) continue;
            if (r <= 0) {
                if (r == 0) f->eof = 1;
                else f->err = 1;
                break;
            }
            got += r;
            continue;
        }
        if (refill(f)) break;
        if (f->unget >= 0) {
            d[got++] = (char)f->unget;
            f->unget = -1;
        }
    }
    return got / sz;
}

char *fgets(char *restrict s, int n, FILE *restrict f) {
    int i = 0;
    while (i < n - 1) {
        int c = fgetc(f);
        if (c == EOF) break;
        s[i++] = (char)c;
        if (c == '\n') break;
    }
    if (i == 0 && n > 1) return NULL;
    s[i] = 0;
    return s;
}

ssize_t getdelim(char **restrict line, size_t *restrict cap, int delim, FILE *restrict f) {
    size_t n = 0;
    if (!*line || !*cap) {
        *cap = 128;
        *line = malloc(*cap);
        if (!*line) return -1;
    }
    for (;;) {
        int c = fgetc(f);
        if (c == EOF) {
            if (!n) return -1;
            break;
        }
        if (n + 2 > *cap) {
            char *nl = realloc(*line, *cap * 2);
            if (!nl) return -1;
            *line = nl;
            *cap *= 2;
        }
        (*line)[n++] = (char)c;
        if (c == delim) break;
    }
    (*line)[n] = 0;
    return (ssize_t)n;
}

ssize_t getline(char **restrict line, size_t *restrict cap, FILE *restrict f) { return getdelim(line, cap, '\n', f); }

/* ------------------------------------------------------------ writing */

size_t fwrite(const void *restrict p, size_t sz, size_t n, FILE *restrict f) {
    size_t len = sz * n;
    const char *s = p;
    if (!len) return 0;
    if (f->reading) fflush(f);
    ensure_buf(f);
    if (f->mode == _IONBF || !f->buf) {
        size_t off = 0;
        while (off < len) {
            ssize_t w = write(f->fd, s + off, len - off);
            if (w < 0) {
                if (errno == EINTR) continue;
                f->err = 1;
                return off / sz;
            }
            off += w;
        }
        return n;
    }
    size_t off = 0;
    while (off < len) {
        size_t room = f->size - f->pos;
        if (!room) {
            if (flush_write(f)) return off / sz;
            continue;
        }
        size_t m = len - off < room ? len - off : room;
        memcpy(f->buf + f->pos, s + off, m);
        f->pos += m;
        off += m;
    }
    if (f->mode == _IOLBF && memchr(s, '\n', len)) {
        if (flush_write(f)) return n;
    }
    return n;
}

int fputc(int c, FILE *f) {
    unsigned char ch = (unsigned char)c;
    return fwrite(&ch, 1, 1, f) == 1 ? ch : EOF;
}

int putc(int c, FILE *f) { return fputc(c, f); }
int putchar(int c) { return fputc(c, stdout); }

int fputs(const char *restrict s, FILE *restrict f) {
    size_t l = strlen(s);
    return fwrite(s, 1, l, f) == l ? 0 : EOF;
}

int puts(const char *s) {
    if (fputs(s, stdout) == EOF) return EOF;
    return fputc('\n', stdout) == EOF ? EOF : 0;
}

void perror(const char *s) {
    int e = errno;
    if (s && *s) fprintf(stderr, "%s: %s\n", s, strerror(e));
    else fprintf(stderr, "%s\n", strerror(e));
}

/* ------------------------------------------------------------ positioning, status */

int fseeko(FILE *f, off_t off, int whence) {
    if (!f->reading && f->pos && flush_write(f)) return -1;
    if (f->reading && whence == SEEK_CUR) off -= (off_t)(f->end - f->pos) + (f->unget >= 0);
    f->reading = 0;
    f->pos = f->end = 0;
    f->unget = -1;
    if (lseek(f->fd, off, whence) < 0) return -1;
    f->eof = 0;
    return 0;
}

int fseek(FILE *f, long off, int whence) { return fseeko(f, off, whence); }

off_t ftello(FILE *f) {
    off_t p = lseek(f->fd, 0, SEEK_CUR);
    if (p < 0) return -1;
    if (f->reading) return p - (off_t)(f->end - f->pos) - (f->unget >= 0);
    return p + (off_t)f->pos;
}

long ftell(FILE *f) { return ftello(f); }
void rewind(FILE *f) {
    fseeko(f, 0, SEEK_SET);
    f->err = 0;
}
int fgetpos(FILE *restrict f, fpos_t *restrict pos) {
    off_t p = ftello(f);
    if (p < 0) return -1;
    *pos = p;
    return 0;
}
int fsetpos(FILE *f, const fpos_t *pos) { return fseeko(f, *pos, SEEK_SET); }
int feof(FILE *f) { return f->eof; }
int ferror(FILE *f) { return f->err; }
void clearerr(FILE *f) { f->eof = f->err = 0; }
int fileno(FILE *f) { return f->fd; }

int remove(const char *path) {
    if (unlink(path) == 0) return 0;
    if (errno == EISDIR) return rmdir(path);
    return -1;
}

/* ------------------------------------------------------------ popen */

FILE *popen(const char *cmd, const char *mode) {
    int fds[2];
    int rd = *mode == 'r';
    if (pipe(fds) < 0) return NULL;
    pid_t pid = fork();
    if (pid < 0) {
        close(fds[0]);
        close(fds[1]);
        return NULL;
    }
    if (pid == 0) {
        if (rd) {
            dup2(fds[1], 1);
        } else {
            dup2(fds[0], 0);
        }
        close(fds[0]);
        close(fds[1]);
        execl("/bin/sh", "sh", "-c", cmd, (char *)0);
        _exit(127);
    }
    close(rd ? fds[1] : fds[0]);
    FILE *f = make(rd ? fds[0] : fds[1], rd ? O_RDONLY : O_WRONLY);
    if (f) f->pid = pid;
    return f;
}

int pclose(FILE *f) {
    pid_t pid = f->pid;
    fclose(f);
    int st;
    while (waitpid(pid, &st, 0) < 0)
        if (errno != EINTR) return -1;
    return st;
}
