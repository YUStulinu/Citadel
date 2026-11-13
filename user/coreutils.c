/* A multi-call binary: /bin/ls, /bin/cat, ... are symlinks to it and argv[0] picks the tool. */
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <libgen.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static const char *prog;

static void warn(const char *what) { fprintf(stderr, "%s: %s: %s\n", prog, what, strerror(errno)); }

static int copy_fd(int in, int out) {
    static char buf[65536];
    ssize_t n;
    while ((n = read(in, buf, sizeof buf)) != 0) {
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        for (ssize_t off = 0; off < n;) {
            ssize_t w = write(out, buf + off, n - off);
            if (w < 0) {
                if (errno == EINTR) continue;
                return -1;
            }
            off += w;
        }
    }
    return 0;
}

static FILE *open_arg(const char *path) {
    if (!strcmp(path, "-")) return stdin;
    FILE *f = fopen(path, "r");
    if (!f) warn(path);
    return f;
}

/* ------------------------------------------------------------------ cat, tee, echo, yes */

static int cmd_cat(int argc, char **argv) {
    int rc = 0;
    if (argc < 2) return copy_fd(0, 1) ? 1 : 0;
    for (int i = 1; i < argc; i++) {
        int fd = strcmp(argv[i], "-") ? open(argv[i], O_RDONLY) : 0;
        if (fd < 0 || copy_fd(fd, 1) < 0) {
            warn(argv[i]);
            rc = 1;
        }
        if (fd > 0) close(fd);
    }
    return rc;
}

static int cmd_tee(int argc, char **argv) {
    int append = argc > 1 && !strcmp(argv[1], "-a");
    int fds[32], n = 0;
    for (int i = 1 + append; i < argc && n < 32; i++) {
        int fd = open(argv[i], O_WRONLY | O_CREAT | (append ? O_APPEND : O_TRUNC), 0644);
        if (fd < 0) warn(argv[i]);
        else fds[n++] = fd;
    }
    char buf[4096];
    ssize_t r;
    while ((r = read(0, buf, sizeof buf)) > 0) {
        write(1, buf, r);
        for (int i = 0; i < n; i++) write(fds[i], buf, r);
    }
    return 0;
}

static int cmd_echo(int argc, char **argv) {
    int i = 1, nl = 1;
    if (i < argc && !strcmp(argv[i], "-n")) {
        nl = 0;
        i++;
    }
    for (int first = 1; i < argc; i++, first = 0) printf("%s%s", first ? "" : " ", argv[i]);
    if (nl) putchar('\n');
    return 0;
}

static int cmd_yes(int argc, char **argv) {
    const char *s = argc > 1 ? argv[1] : "y";
    while (printf("%s\n", s) >= 0 && !ferror(stdout)) {}
    return 0;
}

/* ------------------------------------------------------------------ ls */

static int ls_long, ls_all, ls_one;

static void mode_string(mode_t m, char *s) {
    s[0] = S_ISDIR(m) ? 'd' : S_ISLNK(m) ? 'l' : S_ISCHR(m) ? 'c' : S_ISFIFO(m) ? 'p' : S_ISSOCK(m) ? 's' : '-';
    const char *rwx = "rwxrwxrwx";
    for (int i = 0; i < 9; i++) s[1 + i] = (m & (0400 >> i)) ? rwx[i] : '-';
    s[10] = 0;
}

static void ls_entry(const char *dir, const char *name) {
    if (!ls_long) {
        puts(name);
        return;
    }
    char path[4096];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    struct stat st;
    if (lstat(path, &st) < 0) {
        warn(path);
        return;
    }
    char ms[11], when[32];
    mode_string(st.st_mode, ms);
    time_t t = st.st_mtime;
    strftime(when, sizeof when, "%Y-%m-%d %H:%M", gmtime(&t));
    printf("%s %2lu %4u %4u %8lld %s %s", ms, (unsigned long)st.st_nlink, (unsigned)st.st_uid, (unsigned)st.st_gid,
           (long long)st.st_size, when, name);
    if (S_ISLNK(st.st_mode)) {
        char target[4096];
        ssize_t l = readlink(path, target, sizeof target - 1);
        if (l >= 0) {
            target[l] = 0;
            printf(" -> %s", target);
        }
    }
    putchar('\n');
}

static int cmp_names(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

static int ls_path(const char *path, int header) {
    struct stat st;
    if (lstat(path, &st) < 0) {
        warn(path);
        return 1;
    }
    if (!S_ISDIR(st.st_mode)) {
        char *copy = strdup(path), *copy2 = strdup(path);
        if (ls_long) ls_entry(dirname(copy), basename(copy2));
        else puts(path);
        free(copy);
        free(copy2);
        return 0;
    }
    DIR *d = opendir(path);
    if (!d) {
        warn(path);
        return 1;
    }
    char **names = NULL;
    size_t n = 0, cap = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.' && !ls_all) continue;
        if (n == cap) {
            cap = cap ? cap * 2 : 32;
            names = realloc(names, cap * sizeof *names);
        }
        names[n++] = strdup(e->d_name);
    }
    closedir(d);
    qsort(names, n, sizeof *names, cmp_names);
    if (header) printf("%s:\n", path);
    if (ls_long || ls_one || !isatty(1)) {
        for (size_t i = 0; i < n; i++) ls_entry(path, names[i]);
    } else {
        size_t w = 0;
        for (size_t i = 0; i < n; i++)
            if (strlen(names[i]) > w) w = strlen(names[i]);
        size_t cols = 80 / (w + 2);
        if (cols < 1) cols = 1;
        size_t rows = (n + cols - 1) / cols;
        for (size_t r = 0; r < rows; r++) {
            for (size_t c = 0; c < cols; c++) {
                size_t i = c * rows + r;
                if (i < n) printf("%-*s", (int)(w + 2), names[i]);
            }
            putchar('\n');
        }
    }
    for (size_t i = 0; i < n; i++) free(names[i]);
    free(names);
    return 0;
}

static int cmd_ls(int argc, char **argv) {
    int i = 1, rc = 0;
    for (; i < argc && argv[i][0] == '-' && argv[i][1]; i++)
        for (char *f = argv[i] + 1; *f; f++) {
            if (*f == 'l') ls_long = 1;
            else if (*f == 'a') ls_all = 1;
            else if (*f == '1') ls_one = 1;
        }
    if (i == argc) return ls_path(".", 0);
    int multi = argc - i > 1;
    for (; i < argc; i++) rc |= ls_path(argv[i], multi);
    return rc;
}

/* ------------------------------------------------------------------ files */

static int cmd_mkdir(int argc, char **argv) {
    int parents = 0, rc = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "-p")) {
            parents = 1;
            continue;
        }
        if (parents) {
            char *p = argv[i];
            for (char *s = p + 1; *s; s++)
                if (*s == '/') {
                    *s = 0;
                    mkdir(p, 0755);
                    *s = '/';
                }
            if (mkdir(p, 0755) < 0 && errno != EEXIST) {
                warn(p);
                rc = 1;
            }
        } else if (mkdir(argv[i], 0755) < 0) {
            warn(argv[i]);
            rc = 1;
        }
    }
    return rc;
}

static int cmd_rmdir(int argc, char **argv) {
    int rc = 0;
    for (int i = 1; i < argc; i++)
        if (rmdir(argv[i]) < 0) {
            warn(argv[i]);
            rc = 1;
        }
    return rc;
}

static int remove_tree(const char *path) {
    struct stat st;
    if (lstat(path, &st) < 0) return -1;
    if (S_ISDIR(st.st_mode)) {
        DIR *d = opendir(path);
        if (!d) return -1;
        struct dirent *e;
        char child[4096];
        while ((e = readdir(d))) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            snprintf(child, sizeof child, "%s/%s", path, e->d_name);
            if (remove_tree(child) < 0) warn(child);
        }
        closedir(d);
        return rmdir(path);
    }
    return unlink(path);
}

static int cmd_rm(int argc, char **argv) {
    int rec = 0, force = 0, rc = 0, i = 1;
    for (; i < argc && argv[i][0] == '-'; i++)
        for (char *f = argv[i] + 1; *f; f++) {
            if (*f == 'r' || *f == 'R') rec = 1;
            else if (*f == 'f') force = 1;
        }
    for (; i < argc; i++) {
        int r = rec ? remove_tree(argv[i]) : unlink(argv[i]);
        if (r < 0 && !(force && errno == ENOENT)) {
            warn(argv[i]);
            rc = 1;
        }
    }
    return rc;
}

static int copy_file(const char *src, const char *dst) {
    int in = open(src, O_RDONLY);
    if (in < 0) {
        warn(src);
        return 1;
    }
    struct stat st;
    fstat(in, &st);
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, st.st_mode & 0777);
    if (out < 0) {
        warn(dst);
        close(in);
        return 1;
    }
    int r = copy_fd(in, out);
    close(in);
    close(out);
    if (r < 0) warn(dst);
    return r < 0;
}

static const char *target_path(const char *src, const char *dst, char *buf, size_t n) {
    struct stat st;
    if (stat(dst, &st) == 0 && S_ISDIR(st.st_mode)) {
        char *copy = strdup(src);
        snprintf(buf, n, "%s/%s", dst, basename(copy));
        free(copy);
        return buf;
    }
    return dst;
}

static int cmd_cp(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: cp SRC... DST\n");
        return 2;
    }
    int rc = 0;
    char buf[4096];
    for (int i = 1; i < argc - 1; i++) rc |= copy_file(argv[i], target_path(argv[i], argv[argc - 1], buf, sizeof buf));
    return rc;
}

static int cmd_mv(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: mv SRC... DST\n");
        return 2;
    }
    int rc = 0;
    char buf[4096];
    for (int i = 1; i < argc - 1; i++)
        if (rename(argv[i], target_path(argv[i], argv[argc - 1], buf, sizeof buf)) < 0) {
            warn(argv[i]);
            rc = 1;
        }
    return rc;
}

static int cmd_ln(int argc, char **argv) {
    int sym = argc > 1 && !strcmp(argv[1], "-s");
    if (argc - sym < 3) {
        fprintf(stderr, "usage: ln [-s] TARGET NAME\n");
        return 2;
    }
    const char *a = argv[1 + sym], *b = argv[2 + sym];
    if ((sym ? symlink(a, b) : link(a, b)) < 0) {
        warn(b);
        return 1;
    }
    return 0;
}

static int cmd_touch(int argc, char **argv) {
    int rc = 0;
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_WRONLY | O_CREAT, 0644);
        if (fd < 0) {
            warn(argv[i]);
            rc = 1;
            continue;
        }
        close(fd);
        utimensat(AT_FDCWD, argv[i], NULL, 0);
    }
    return rc;
}

static int cmd_stat(int argc, char **argv) {
    int rc = 0;
    for (int i = 1; i < argc; i++) {
        struct stat st;
        if (lstat(argv[i], &st) < 0) {
            warn(argv[i]);
            rc = 1;
            continue;
        }
        char ms[11];
        mode_string(st.st_mode, ms);
        printf("  File: %s\n  Size: %lld\tBlocks: %lld\tInode: %llu\tLinks: %lu\nAccess: (%04o/%s)\tUid: %u\tGid: %u\n",
               argv[i], (long long)st.st_size, (long long)st.st_blocks, (unsigned long long)st.st_ino,
               (unsigned long)st.st_nlink, (unsigned)(st.st_mode & 07777), ms, (unsigned)st.st_uid, (unsigned)st.st_gid);
    }
    return rc;
}

/* ------------------------------------------------------------------ text */

static int cmd_wc(int argc, char **argv) {
    int fl = 0, fw = 0, fc = 0, i = 1;
    for (; i < argc && argv[i][0] == '-' && argv[i][1]; i++)
        for (char *f = argv[i] + 1; *f; f++) {
            if (*f == 'l') fl = 1;
            else if (*f == 'w') fw = 1;
            else if (*f == 'c') fc = 1;
        }
    if (!fl && !fw && !fc) fl = fw = fc = 1;
    long tl = 0, tw = 0, tc = 0;
    int nfiles = argc - i, rc = 0;
    for (int k = 0; k < (nfiles ? nfiles : 1); k++) {
        const char *name = nfiles ? argv[i + k] : "-";
        FILE *f = open_arg(name);
        if (!f) {
            rc = 1;
            continue;
        }
        long l = 0, w = 0, c = 0;
        int in = 0, ch;
        while ((ch = getc(f)) != EOF) {
            c++;
            if (ch == '\n') l++;
            if (isspace(ch)) in = 0;
            else if (!in) {
                in = 1;
                w++;
            }
        }
        if (f != stdin) fclose(f);
        if (!nfiles && fl + fw + fc == 1) {
            printf("%ld\n", fl ? l : fw ? w : c); /* a lone count from stdin is printed bare */
        } else {
            if (fl) printf("%7ld ", l);
            if (fw) printf("%7ld ", w);
            if (fc) printf("%7ld ", c);
            printf("%s\n", nfiles ? name : "");
        }
        tl += l, tw += w, tc += c;
    }
    if (nfiles > 1) {
        if (fl) printf("%7ld ", tl);
        if (fw) printf("%7ld ", tw);
        if (fc) printf("%7ld ", tc);
        printf("total\n");
    }
    return rc;
}

static int cmd_head(int argc, char **argv) {
    long n = 10;
    int i = 1;
    if (i < argc && !strcmp(argv[i], "-n") && i + 1 < argc) {
        n = atol(argv[i + 1]);
        i += 2;
    } else if (i < argc && argv[i][0] == '-' && isdigit((unsigned char)argv[i][1])) {
        n = atol(argv[i] + 1);
        i++;
    }
    FILE *f = i < argc ? open_arg(argv[i]) : stdin;
    if (!f) return 1;
    char *line = NULL;
    size_t cap = 0;
    for (long k = 0; k < n && getline(&line, &cap, f) >= 0; k++) fputs(line, stdout);
    return 0;
}

static int cmd_tail(int argc, char **argv) {
    long n = 10;
    int i = 1;
    if (i < argc && !strcmp(argv[i], "-n") && i + 1 < argc) {
        n = atol(argv[i + 1]);
        i += 2;
    } else if (i < argc && argv[i][0] == '-' && isdigit((unsigned char)argv[i][1])) {
        n = atol(argv[i] + 1);
        i++;
    }
    FILE *f = i < argc ? open_arg(argv[i]) : stdin;
    if (!f || n <= 0) return f ? 0 : 1;
    char **ring = calloc(n, sizeof *ring);
    long count = 0;
    char *line = NULL;
    size_t cap = 0;
    while (getline(&line, &cap, f) >= 0) {
        free(ring[count % n]);
        ring[count % n] = strdup(line);
        count++;
    }
    for (long k = count > n ? count - n : 0; k < count; k++) fputs(ring[k % n], stdout);
    return 0;
}

/* grep: fixed strings or a small regex subset (^ $ . * and character classes). */
static int re_match_here(const char *re, const char *s, int icase);

static int re_class(const char **rep, int c, int icase) {
    const char *re = *rep + 1;
    int neg = *re == '^', ok = 0;
    if (neg) re++;
    if (icase) c = tolower(c);
    do {
        int a = (unsigned char)*re, b = a;
        if (re[1] == '-' && re[2] && re[2] != ']') {
            b = (unsigned char)re[2];
            re += 2;
        }
        if (icase) a = tolower(a), b = tolower(b);
        if (c >= a && c <= b) ok = 1;
        re++;
    } while (*re && *re != ']');
    *rep = *re ? re + 1 : re;
    return ok != neg;
}

static int re_one(const char *re, const char **next, int c, int icase) {
    if (*re == '[') {
        int r = re_class(&re, c, icase);
        *next = re;
        return c && r;
    }
    if (*re == '\\' && re[1]) re++;
    *next = re + 1;
    if (!c) return 0;
    if (*re == '.' && re[-1] != '\\') return 1;
    return icase ? tolower((unsigned char)*re) == tolower(c) : *re == c;
}

static int re_match_here(const char *re, const char *s, int icase) {
    for (;;) {
        if (!*re) return 1;
        if (re[0] == '$' && !re[1]) return !*s;
        const char *after;
        re_one(re, &after, 'x', icase); /* find the extent of this atom */
        if (*after == '*') {
            do {
                if (re_match_here(after + 1, s, icase)) return 1;
            } while (*s && re_one(re, &after, (unsigned char)*s++, icase));
            return 0;
        }
        if (!re_one(re, &after, (unsigned char)*s, icase)) return 0;
        re = after;
        s++;
    }
}

static int re_match(const char *re, const char *s, int icase) {
    if (*re == '^') return re_match_here(re + 1, s, icase);
    do
        if (re_match_here(re, s, icase)) return 1;
    while (*s++);
    return 0;
}

static int strcasestr_simple(const char *h, const char *n) {
    size_t l = strlen(n);
    for (; *h; h++)
        if (!strncasecmp(h, n, l)) return 1;
    return !l;
}

static int cmd_grep(int argc, char **argv) {
    int invert = 0, count = 0, icase = 0, number = 0, quiet = 0, fixed = 0, i = 1;
    for (; i < argc && argv[i][0] == '-' && argv[i][1]; i++)
        for (char *f = argv[i] + 1; *f; f++) switch (*f) {
            case 'v': invert = 1; break;
            case 'c': count = 1; break;
            case 'i': icase = 1; break;
            case 'n': number = 1; break;
            case 'q': quiet = 1; break;
            case 'F': fixed = 1; break;
            }
    if (i >= argc) {
        fprintf(stderr, "usage: grep [-vcinqF] PATTERN [FILE...]\n");
        return 2;
    }
    const char *pat = argv[i++];
    int nfiles = argc - i, found = 0;
    for (int k = 0; k < (nfiles ? nfiles : 1); k++) {
        const char *name = nfiles ? argv[i + k] : "-";
        FILE *f = open_arg(name);
        if (!f) continue;
        char *line = NULL;
        size_t cap = 0;
        ssize_t len;
        long ln = 0, hits = 0;
        while ((len = getline(&line, &cap, f)) >= 0) {
            ln++;
            if (len && line[len - 1] == '\n') line[len - 1] = 0;
            int m = fixed ? (icase ? strcasestr_simple(line, pat) : strstr(line, pat) != NULL) : re_match(pat, line, icase);
            if (m == invert) continue;
            hits++;
            found = 1;
            if (quiet) return 0;
            if (count) continue;
            if (nfiles > 1) printf("%s:", name);
            if (number) printf("%ld:", ln);
            puts(line);
        }
        if (count) {
            if (nfiles > 1) printf("%s:", name);
            printf("%ld\n", hits);
        }
        if (f != stdin) fclose(f);
        free(line);
    }
    return found ? 0 : 1;
}

static int sort_numeric, sort_reverse;
static int sort_cmp(const void *a, const void *b) {
    const char *x = *(char *const *)a, *y = *(char *const *)b;
    int r;
    if (sort_numeric) {
        double dx = strtod(x, NULL), dy = strtod(y, NULL);
        r = dx < dy ? -1 : dx > dy;
        if (!r) r = strcmp(x, y);
    } else {
        r = strcmp(x, y);
    }
    return sort_reverse ? -r : r;
}

static int cmd_sort(int argc, char **argv) {
    int uniq = 0, i = 1;
    for (; i < argc && argv[i][0] == '-' && argv[i][1]; i++)
        for (char *f = argv[i] + 1; *f; f++) {
            if (*f == 'n') sort_numeric = 1;
            else if (*f == 'r') sort_reverse = 1;
            else if (*f == 'u') uniq = 1;
        }
    char **lines = NULL;
    size_t n = 0, cap = 0;
    int nfiles = argc - i;
    for (int k = 0; k < (nfiles ? nfiles : 1); k++) {
        FILE *f = open_arg(nfiles ? argv[i + k] : "-");
        if (!f) return 2;
        char *line = NULL;
        size_t lc = 0;
        ssize_t len;
        while ((len = getline(&line, &lc, f)) >= 0) {
            if (len && line[len - 1] == '\n') line[len - 1] = 0;
            if (n == cap) {
                cap = cap ? cap * 2 : 256;
                lines = realloc(lines, cap * sizeof *lines);
            }
            lines[n++] = strdup(line);
        }
        if (f != stdin) fclose(f);
        free(line);
    }
    qsort(lines, n, sizeof *lines, sort_cmp);
    for (size_t k = 0; k < n; k++) {
        if (uniq && k && !strcmp(lines[k], lines[k - 1])) continue;
        puts(lines[k]);
    }
    return 0;
}

static int cmd_uniq(int argc, char **argv) {
    int count = argc > 1 && !strcmp(argv[1], "-c");
    FILE *f = argc > 1 + count ? open_arg(argv[1 + count]) : stdin;
    if (!f) return 1;
    char *line = NULL, *prev = NULL;
    size_t cap = 0;
    long reps = 0;
    while (getline(&line, &cap, f) >= 0) {
        if (prev && !strcmp(prev, line)) {
            reps++;
            continue;
        }
        if (prev) {
            if (count) printf("%7ld %s", reps, prev);
            else fputs(prev, stdout);
            free(prev);
        }
        prev = strdup(line);
        reps = 1;
    }
    if (prev) {
        if (count) printf("%7ld %s", reps, prev);
        else fputs(prev, stdout);
    }
    return 0;
}

/* ------------------------------------------------------------------ system */

static int cmd_uname(int argc, char **argv) {
    struct utsname u;
    uname(&u);
    int all = argc > 1 && !strcmp(argv[1], "-a");
    if (all) printf("%s %s %s %s %s\n", u.sysname, u.nodename, u.release, u.version, u.machine);
    else if (argc > 1 && !strcmp(argv[1], "-r")) puts(u.release);
    else if (argc > 1 && !strcmp(argv[1], "-m")) puts(u.machine);
    else puts(u.sysname);
    return 0;
}

static int cmd_sleep(int argc, char **argv) {
    if (argc < 2) return 1;
    double s = strtod(argv[1], NULL);
    struct timespec ts = {(time_t)s, (long)((s - (time_t)s) * 1e9)};
    while (nanosleep(&ts, &ts) < 0 && errno == EINTR) {}
    return 0;
}

static int sig_by_name(const char *s) {
    static const char *const names[] = {"", "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "BUS", "FPE", "KILL", "USR1",
                                        "SEGV", "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CONT", "STOP", "TSTP"};
    if (isdigit((unsigned char)*s)) return atoi(s);
    if (!strncmp(s, "SIG", 3)) s += 3;
    for (int i = 1; i < (int)(sizeof names / sizeof *names); i++)
        if (!strcmp(s, names[i])) return i;
    return -1;
}

static int cmd_kill(int argc, char **argv) {
    int sig = SIGTERM, i = 1, rc = 0;
    if (i < argc && argv[i][0] == '-') {
        const char *s = argv[i] + 1;
        if (!strcmp(s, "s") && i + 1 < argc) s = argv[++i];
        sig = sig_by_name(s);
        if (sig < 0) {
            fprintf(stderr, "kill: unknown signal %s\n", s);
            return 2;
        }
        i++;
    }
    for (; i < argc; i++)
        if (kill(atoi(argv[i]), sig) < 0) {
            warn(argv[i]);
            rc = 1;
        }
    return rc;
}

static int cmd_env(int argc, char **argv) {
    int i = 1;
    for (; i < argc && strchr(argv[i], '='); i++) putenv(argv[i]);
    if (i < argc) {
        execvp(argv[i], argv + i);
        warn(argv[i]);
        return 127;
    }
    for (char **e = environ; *e; e++) puts(*e);
    return 0;
}

static int cmd_date(int argc, char **argv) {
    time_t t = time(NULL);
    char buf[128];
    const char *fmt = argc > 1 && argv[1][0] == '+' ? argv[1] + 1 : "%a %b %e %H:%M:%S %Z %Y";
    strftime(buf, sizeof buf, fmt, gmtime(&t));
    puts(buf);
    return 0;
}

static int cmd_seq(int argc, char **argv) {
    long a = 1, b, step = 1;
    if (argc == 2) b = atol(argv[1]);
    else if (argc == 3) a = atol(argv[1]), b = atol(argv[2]);
    else if (argc == 4) a = atol(argv[1]), step = atol(argv[2]), b = atol(argv[3]);
    else {
        fprintf(stderr, "usage: seq [FIRST [STEP]] LAST\n");
        return 2;
    }
    if (step == 0) return 1;
    for (long x = a; step > 0 ? x <= b : x >= b; x += step) printf("%ld\n", x);
    return 0;
}

static int cmd_ps(int argc, char **argv) {
    DIR *d = opendir("/proc");
    if (!d) {
        warn("/proc");
        return 1;
    }
    printf("  PID  PPID S CMD\n");
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!isdigit((unsigned char)e->d_name[0])) continue;
        char path[64], buf[512];
        snprintf(path, sizeof path, "/proc/%s/stat", e->d_name);
        int fd = open(path, O_RDONLY);
        if (fd < 0) continue;
        ssize_t n = read(fd, buf, sizeof buf - 1);
        close(fd);
        if (n <= 0) continue;
        buf[n] = 0;
        char *lp = strchr(buf, '('), *rp = strrchr(buf, ')');
        if (!lp || !rp) continue;
        *rp = 0;
        char state;
        int ppid;
        if (sscanf(rp + 2, "%c %d", &state, &ppid) != 2) continue;
        printf("%5s %5d %c %s\n", e->d_name, ppid, state, lp + 1);
    }
    closedir(d);
    return 0;
}

static int cmd_clear(int argc, char **argv) {
    fputs("\033[H\033[2J", stdout);
    return 0;
}

static int cmd_basename(int argc, char **argv) {
    if (argc < 2) return 1;
    char *b = basename(argv[1]);
    if (argc > 2) {
        size_t l = strlen(b), s = strlen(argv[2]);
        if (l > s && !strcmp(b + l - s, argv[2])) b[l - s] = 0;
    }
    puts(b);
    return 0;
}

static int cmd_dirname(int argc, char **argv) {
    if (argc < 2) return 1;
    puts(dirname(argv[1]));
    return 0;
}

static int cmd_poweroff(int argc, char **argv) {
    sync();
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(1001L), "D"(0L) : "rcx", "r11", "memory");
    fprintf(stderr, "poweroff: not supported on this kernel\n");
    return 1;
}

/* profctl start|stream|stop: Citadel's sampling profiler (syscall 1000); /proc/profile shows it. */
static int cmd_profctl(int argc, char **argv) {
    long cmd = argc > 1 && !strcmp(argv[1], "stop") ? 0 : argc > 1 && !strcmp(argv[1], "stream") ? 2 : 1;
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(1000L), "D"(cmd), "S"(0L) : "rcx", "r11", "memory");
    if (r < 0) {
        fprintf(stderr, "profctl: not supported here (error %ld)\n", -r);
        return 1;
    }
    return 0;
}

static int cmd_sync(int argc, char **argv) {
    sync();
    return 0;
}

static int cmd_true(int argc, char **argv) { return 0; }
static int cmd_false(int argc, char **argv) { return 1; }

static const struct {
    const char *name;
    int (*fn)(int, char **);
} tools[] = {
    {"basename", cmd_basename}, {"cat", cmd_cat},   {"clear", cmd_clear},       {"cp", cmd_cp},       {"date", cmd_date},
    {"dirname", cmd_dirname},   {"echo", cmd_echo}, {"env", cmd_env},           {"false", cmd_false}, {"grep", cmd_grep},
    {"head", cmd_head},         {"kill", cmd_kill}, {"ln", cmd_ln},             {"ls", cmd_ls},       {"mkdir", cmd_mkdir},
    {"mv", cmd_mv},             {"poweroff", cmd_poweroff}, {"profctl", cmd_profctl}, {"ps", cmd_ps},     {"rm", cmd_rm},       {"rmdir", cmd_rmdir},
    {"seq", cmd_seq},           {"sleep", cmd_sleep}, {"sort", cmd_sort},       {"stat", cmd_stat},   {"sync", cmd_sync},   {"tail", cmd_tail},
    {"tee", cmd_tee},           {"touch", cmd_touch}, {"true", cmd_true},       {"uname", cmd_uname}, {"uniq", cmd_uniq},
    {"wc", cmd_wc},             {"yes", cmd_yes},
};

int main(int argc, char **argv) {
    char *copy = strdup(argv[0]);
    prog = basename(copy);
    if (!strcmp(prog, "coreutils") && argc == 2 && !strcmp(argv[1], "--list")) {
        for (unsigned i = 0; i < sizeof tools / sizeof *tools; i++) puts(tools[i].name);
        return 0;
    }
    if (!strcmp(prog, "coreutils") && argc > 1) {
        argv++;
        argc--;
        free(copy);
        copy = strdup(argv[0]);
        prog = basename(copy);
    }
    for (unsigned i = 0; i < sizeof tools / sizeof *tools; i++)
        if (!strcmp(prog, tools[i].name)) {
            int r = tools[i].fn(argc, argv);
            fflush(stdout);
            return r;
        }
    fprintf(stderr, "coreutils: unknown tool '%s'\n", prog);
    return 127;
}
