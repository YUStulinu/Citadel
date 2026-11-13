/* httpload: an HTTP/1.1 load generator for the host (Linux, epoll). Keeps C keep-alive
   connections busy with GET requests for D seconds and reports requests per second and latency
   percentiles. Built natively on the host; used to compare Citadel with Linux.
     httpload HOST PORT PATH [CONNECTIONS=32] [SECONDS=10] */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

struct conn {
    int fd;
    double sent_at;
    char buf[65536];
    size_t len;
    long body_left; /* -1: still reading headers */
};

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static struct sockaddr_in target;
static char request[512];
static size_t request_len;
static double *lat;
static size_t nlat, caplat;
static long errors, connects;

static int open_conn(int ep, struct conn *c) {
    c->fd = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(c->fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    if (connect(c->fd, (struct sockaddr *)&target, sizeof target) < 0) {
        close(c->fd);
        return -1;
    }
    connects++;
    fcntl(c->fd, F_SETFL, O_NONBLOCK);
    struct epoll_event ev = {.events = EPOLLIN, .data.ptr = c};
    epoll_ctl(ep, EPOLL_CTL_ADD, c->fd, &ev);
    return 0;
}

static void send_req(struct conn *c) {
    c->len = 0;
    c->body_left = -1;
    c->sent_at = now();
    if (write(c->fd, request, request_len) != (ssize_t)request_len) errors++;
}

static int cmp(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: httpload HOST PORT PATH [CONNECTIONS] [SECONDS]\n");
        return 2;
    }
    int nconn = argc > 4 ? atoi(argv[4]) : 32;
    double secs = argc > 5 ? atof(argv[5]) : 10;
    target.sin_family = AF_INET;
    target.sin_port = htons(atoi(argv[2]));
    inet_pton(AF_INET, argv[1], &target.sin_addr);
    request_len = snprintf(request, sizeof request, "GET %s HTTP/1.1\r\nHost: %s\r\n\r\n", argv[3], argv[1]);
    int ep = epoll_create1(0);
    struct conn *cs = calloc(nconn, sizeof *cs);
    for (int i = 0; i < nconn; i++) {
        if (open_conn(ep, &cs[i]) < 0) {
            perror("connect");
            return 1;
        }
        send_req(&cs[i]);
    }
    double start = now(), end = start + secs;
    struct epoll_event evs[256];
    while (now() < end) {
        int n = epoll_wait(ep, evs, 256, 100);
        for (int k = 0; k < n; k++) {
            struct conn *c = evs[k].data.ptr;
            for (;;) {
                ssize_t r = read(c->fd, c->buf + c->len, sizeof c->buf - c->len - 1);
                if (r < 0 && errno == EAGAIN) break;
                if (r <= 0) {
                    /* Server closed: reconnect. */
                    epoll_ctl(ep, EPOLL_CTL_DEL, c->fd, NULL);
                    close(c->fd);
                    errors++;
                    if (open_conn(ep, c) == 0) send_req(c);
                    break;
                }
                c->len += r;
                c->buf[c->len] = 0;
                if (c->body_left < 0) {
                    char *h = strstr(c->buf, "\r\n\r\n");
                    if (!h) continue;
                    char *cl = strcasestr(c->buf, "Content-Length:");
                    long body = cl ? atol(cl + 15) : 0;
                    size_t hdr = h + 4 - c->buf;
                    c->body_left = body - (long)(c->len - hdr);
                } else {
                    c->body_left -= r;
                }
                if (c->body_left <= 0) {
                    if (nlat == caplat) {
                        caplat = caplat ? caplat * 2 : 1 << 16;
                        lat = realloc(lat, caplat * sizeof *lat);
                    }
                    lat[nlat++] = now() - c->sent_at;
                    send_req(c);
                }
            }
        }
    }
    double took = now() - start;
    qsort(lat, nlat, sizeof *lat, cmp);
    double p50 = nlat ? lat[nlat / 2] : 0, p99 = nlat ? lat[nlat * 99 / 100] : 0;
    printf("httpload: %zu requests in %.1f s = %.0f req/s, latency p50 %.0f us p99 %.0f us, %ld connections opened, %ld errors\n", nlat, took,
           nlat / took, p50 * 1e6, p99 * 1e6, connects, errors);
    return 0;
}
