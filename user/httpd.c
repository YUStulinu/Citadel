/* httpd: a static-file HTTP/1.1 server. One process, non-blocking sockets and poll(), keep-alive.
   Usage: httpd [-p PORT] [-r DOCROOT] [-q]   (defaults: port 80, /mnt/www)
   The same static binary serves on Citadel and on Linux (the benchmark baseline). */
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#define MAX_CONN 1024
#define REQ_MAX 8192

struct conn {
    int fd;
    char req[REQ_MAX];
    size_t req_len;
    char *out;          /* response being sent */
    size_t out_len, out_off;
    int close_after;
};

static struct conn conns[MAX_CONN];
static struct pollfd pfds[MAX_CONN + 1];
static const char *root = "/mnt/www";
static int quiet;
static unsigned long served;

/* A small in-memory cache of files (the benchmark serves the same few files). */
struct cached {
    char path[256];
    char *data;
    size_t len;
    time_t mtime;
};
static struct cached cache[64];

static const char *mime(const char *p) {
    const char *e = strrchr(p, '.');
    if (!e) return "application/octet-stream";
    if (!strcmp(e, ".html") || !strcmp(e, ".htm")) return "text/html; charset=utf-8";
    if (!strcmp(e, ".txt") || !strcmp(e, ".lua") || !strcmp(e, ".c")) return "text/plain; charset=utf-8";
    if (!strcmp(e, ".css")) return "text/css";
    if (!strcmp(e, ".js")) return "text/javascript";
    if (!strcmp(e, ".json")) return "application/json";
    if (!strcmp(e, ".png")) return "image/png";
    if (!strcmp(e, ".svg")) return "image/svg+xml";
    return "application/octet-stream";
}

static struct cached *load(const char *path) {
    struct stat st;
    if (stat(path, &st) < 0 || !S_ISREG(st.st_mode)) return NULL;
    for (int i = 0; i < 64; i++)
        if (cache[i].data && !strcmp(cache[i].path, path) && cache[i].mtime == st.st_mtime && cache[i].len == (size_t)st.st_size)
            return &cache[i];
    int fd = open(path, O_RDONLY);
    if (fd < 0) return NULL;
    char *data = malloc(st.st_size + 1);
    size_t got = 0;
    while (got < (size_t)st.st_size) {
        ssize_t n = read(fd, data + got, st.st_size - got);
        if (n <= 0) break;
        got += n;
    }
    close(fd);
    static int next;
    struct cached *c = &cache[next++ % 64];
    free(c->data);
    snprintf(c->path, sizeof c->path, "%s", path);
    c->data = data;
    c->len = got;
    c->mtime = st.st_mtime;
    return c;
}

static void respond(struct conn *c, int code, const char *reason, const char *type, const char *body, size_t len, int head) {
    char hdr[512];
    int h = snprintf(hdr, sizeof hdr,
                     "HTTP/1.1 %d %s\r\nServer: citadel-httpd\r\nContent-Type: %s\r\nContent-Length: %zu\r\nConnection: %s\r\n\r\n",
                     code, reason, type, len, c->close_after ? "close" : "keep-alive");
    size_t total = h + (head ? 0 : len);
    c->out = malloc(total);
    memcpy(c->out, hdr, h);
    if (!head) memcpy(c->out + h, body, len);
    c->out_len = total;
    c->out_off = 0;
    served++;
}

/* Handles one complete request in c->req (ends with \r\n\r\n at `end`). */
static void handle(struct conn *c, size_t end) {
    char method[8] = "", target[1024] = "", version[16] = "";
    sscanf(c->req, "%7s %1023s %15s", method, target, version);
    /* Keep-alive is the HTTP/1.1 default; "Connection: close" or HTTP/1.0 end it. */
    c->close_after = strcmp(version, "HTTP/1.1") != 0;
    for (char *p = c->req; p < c->req + end; p++) {
        if (!strncasecmp(p, "\nConnection:", 12)) {
            char *v = p + 12;
            while (*v == ' ') v++;
            if (!strncasecmp(v, "close", 5)) c->close_after = 1;
            if (!strncasecmp(v, "keep-alive", 10)) c->close_after = 0;
        }
    }
    int head = !strcmp(method, "HEAD");
    if (strcmp(method, "GET") && !head) {
        const char *m = "method not allowed\n";
        respond(c, 405, "Method Not Allowed", "text/plain", m, strlen(m), 0);
        return;
    }
    char *q = strchr(target, '?');
    if (q) *q = 0;
    if (strstr(target, "..") || target[0] != '/') {
        const char *m = "bad request\n";
        respond(c, 400, "Bad Request", "text/plain", m, strlen(m), head);
        return;
    }
    if (!strcmp(target, "/status")) {
        char body[128];
        int n = snprintf(body, sizeof body, "{\"served\":%lu,\"pid\":%d}\n", served, getpid());
        respond(c, 200, "OK", "application/json", body, n, head);
        return;
    }
    char path[1300];
    snprintf(path, sizeof path, "%s%s%s", root, target, target[strlen(target) - 1] == '/' ? "index.html" : "");
    struct cached *f = load(path);
    if (!f) {
        const char *m = "<h1>404 Not Found</h1>\n";
        respond(c, 404, "Not Found", "text/html", m, strlen(m), head);
        return;
    }
    respond(c, 200, "OK", mime(path), f->data, f->len, head);
}

static void drop(int i) {
    close(conns[i].fd);
    free(conns[i].out);
    memset(&conns[i], 0, sizeof conns[i]);
    conns[i].fd = -1;
}

int main(int argc, char **argv) {
    int port = 80;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "-p") && i + 1 < argc) port = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-r") && i + 1 < argc) root = argv[++i];
        else if (!strcmp(argv[i], "-q")) quiet = 1;
    }
    signal(SIGPIPE, SIG_IGN);
    int ls = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(ls, (struct sockaddr *)&a, sizeof a) < 0 || listen(ls, 128) < 0) {
        perror("httpd: bind/listen");
        return 1;
    }
    if (!quiet) printf("httpd: serving %s on port %d\n", root, port);
    fflush(stdout);
    for (int i = 0; i < MAX_CONN; i++) conns[i].fd = -1;
    for (;;) {
        int n = 0;
        pfds[n].fd = ls;
        pfds[n].events = POLLIN;
        n++;
        int map[MAX_CONN];
        for (int i = 0; i < MAX_CONN; i++) {
            if (conns[i].fd < 0) continue;
            pfds[n].fd = conns[i].fd;
            pfds[n].events = conns[i].out ? POLLOUT : POLLIN;
            pfds[n].revents = 0;
            map[n] = i;
            n++;
        }
        if (poll(pfds, n, -1) < 0) {
            if (errno == EINTR) continue;
            perror("httpd: poll");
            return 1;
        }
        if (pfds[0].revents & POLLIN) {
            for (;;) {
                int fd = accept4(ls, NULL, NULL, SOCK_NONBLOCK);
                if (fd < 0) break;
                int slot = -1;
                for (int i = 0; i < MAX_CONN; i++)
                    if (conns[i].fd < 0) {
                        slot = i;
                        break;
                    }
                if (slot < 0) {
                    close(fd);
                    continue;
                }
                conns[slot].fd = fd;
            }
        }
        for (int k = 1; k < n; k++) {
            int i = map[k];
            struct conn *c = &conns[i];
            if (c->fd < 0 || !pfds[k].revents) continue;
            if (c->out) {
                ssize_t w = write(c->fd, c->out + c->out_off, c->out_len - c->out_off);
                if (w < 0 && errno != EAGAIN) {
                    drop(i);
                    continue;
                }
                if (w > 0) c->out_off += w;
                if (c->out_off == c->out_len) {
                    free(c->out);
                    c->out = NULL;
                    if (c->close_after) {
                        drop(i);
                        continue;
                    }
                }
            } else {
                ssize_t r = read(c->fd, c->req + c->req_len, REQ_MAX - 1 - c->req_len);
                if (r <= 0) {
                    if (r < 0 && errno == EAGAIN) continue;
                    drop(i);
                    continue;
                }
                c->req_len += r;
                c->req[c->req_len] = 0;
            }
            /* A complete request waiting (possibly pipelined)? */
            if (!c->out && c->fd >= 0) {
                char *e = strstr(c->req, "\r\n\r\n");
                if (e) {
                    size_t end = e - c->req + 4;
                    handle(c, end);
                    memmove(c->req, c->req + end, c->req_len - end + 1);
                    c->req_len -= end;
                    /* Try to send right away. */
                    ssize_t w = write(c->fd, c->out, c->out_len);
                    if (w > 0) c->out_off = w;
                    if (c->out_off == c->out_len) {
                        free(c->out);
                        c->out = NULL;
                        if (c->close_after) drop(i);
                    }
                } else if (c->req_len >= REQ_MAX - 1) {
                    drop(i);
                }
            }
        }
    }
}
