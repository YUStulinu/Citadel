/* Network tests over loopback: TCP echo of a large buffer, many short connections, refused
   connections, non-blocking connect + poll, UDP datagrams. Exit status = number of failures. */
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failures;
#define CHECK(c, ...)                                  \
    do {                                               \
        if (c) {                                       \
            printf("ok   " __VA_ARGS__);               \
        } else {                                       \
            printf("FAIL " __VA_ARGS__);               \
            printf("     (errno %d %s)\n", errno, strerror(errno)); \
            failures++;                                \
        }                                              \
        printf("\n");                                  \
        fflush(stdout);                                \
    } while (0)

static struct sockaddr_in addr(int port) {
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = htonl(0x7f000001);
    return a;
}

static int listener(int port) {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = addr(port);
    if (bind(s, (struct sockaddr *)&a, sizeof a) < 0 || listen(s, 16) < 0) return -1;
    return s;
}

static int dial(int port) {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = addr(port);
    if (connect(s, (struct sockaddr *)&a, sizeof a) < 0) {
        close(s);
        return -1;
    }
    return s;
}

static double secs(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int write_all(int fd, const char *p, size_t n) {
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w <= 0) return -1;
        p += w;
        n -= w;
    }
    return 0;
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    /* 1. Echo 4 MiB through a forked echo server. */
    int ls = listener(7001);
    CHECK(ls >= 0, "listen on 127.0.0.1:7001");
    pid_t p = fork();
    if (p == 0) {
        int c = accept(ls, NULL, NULL);
        char buf[16384];
        ssize_t n;
        while ((n = read(c, buf, sizeof buf)) > 0)
            if (write_all(c, buf, n) < 0) break;
        close(c);
        _exit(0);
    }
    const size_t total = 4 << 20;
    char *out = malloc(total), *in = malloc(total);
    for (size_t i = 0; i < total; i++) out[i] = (char)(i * 131 + (i >> 9));
    int c = dial(7001);
    CHECK(c >= 0, "connect");
    double t0 = secs();
    pid_t w = fork();
    if (w == 0) {
        write_all(c, out, total);
        shutdown(c, SHUT_WR);
        _exit(0);
    }
    size_t got = 0;
    ssize_t n;
    while (got < total && (n = read(c, in + got, total - got)) > 0) got += n;
    double t = secs() - t0;
    waitpid(w, NULL, 0);
    CHECK(got == total && !memcmp(in, out, total), "echo 4 MiB intact (%.1f MB/s each way)", total / t / 1e6);
    n = read(c, in, 1);
    CHECK(n == 0, "end of stream after the server closes");
    close(c);
    waitpid(p, NULL, 0);
    close(ls);

    /* 2. Many short connections (connect, request, response, close). */
    ls = listener(7002);
    p = fork();
    if (p == 0) {
        for (int i = 0; i < 200; i++) {
            int a = accept(ls, NULL, NULL);
            char b[64];
            ssize_t r = read(a, b, sizeof b);
            if (r > 0) write(a, b, r);
            close(a);
        }
        _exit(0);
    }
    int ok = 0;
    t0 = secs();
    for (int i = 0; i < 200; i++) {
        int s = dial(7002);
        if (s < 0) continue;
        char msg[32], back[32] = {0};
        int len = snprintf(msg, sizeof msg, "hello %d", i);
        write(s, msg, len);
        if (read(s, back, sizeof back) == len && !memcmp(back, msg, len)) ok++;
        close(s);
    }
    t = secs() - t0;
    waitpid(p, NULL, 0);
    close(ls);
    CHECK(ok == 200, "200 short connections (%d ok, %.0f us each)", ok, t / 200 * 1e6);

    /* 3. Nobody listening: refused. */
    int s = dial(7999);
    CHECK(s < 0 && errno == ECONNREFUSED, "connect to a closed port is refused");

    /* 4. Non-blocking connect, then poll for writability. */
    ls = listener(7003);
    s = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    struct sockaddr_in a3 = addr(7003);
    int r = connect(s, (struct sockaddr *)&a3, sizeof a3);
    CHECK(r < 0 && errno == EINPROGRESS, "non-blocking connect returns EINPROGRESS");
    struct pollfd pf = {s, POLLOUT, 0};
    r = poll(&pf, 1, 2000);
    int err = -1;
    socklen_t el = sizeof err;
    getsockopt(s, SOL_SOCKET, SO_ERROR, &err, &el);
    CHECK(r == 1 && (pf.revents & POLLOUT) && err == 0, "poll reports the connection writable");
    int acc = accept(ls, NULL, NULL);
    struct sockaddr_in peer;
    socklen_t pl = sizeof peer;
    getpeername(acc, (struct sockaddr *)&peer, &pl);
    struct sockaddr_in me;
    socklen_t ml = sizeof me;
    getsockname(s, (struct sockaddr *)&me, &ml);
    CHECK(peer.sin_port == me.sin_port, "accept/getpeername see the client's port %d", ntohs(me.sin_port));
    char ch;
    CHECK(read(s, &ch, 1) < 0 && errno == EAGAIN, "non-blocking read with no data: EAGAIN");
    close(acc);
    close(s);
    close(ls);

    /* 5. UDP. */
    int u1 = socket(AF_INET, SOCK_DGRAM, 0), u2 = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in ua = addr(7004);
    CHECK(bind(u1, (struct sockaddr *)&ua, sizeof ua) == 0, "bind UDP port");
    for (int i = 0; i < 5; i++) {
        char d[16];
        int l = snprintf(d, sizeof d, "dgram %d", i);
        sendto(u2, d, l, 0, (struct sockaddr *)&ua, sizeof ua);
    }
    int good = 0;
    for (int i = 0; i < 5; i++) {
        char d[16] = {0};
        struct sockaddr_in from;
        socklen_t fl = sizeof from;
        ssize_t l = recvfrom(u1, d, sizeof d, 0, (struct sockaddr *)&from, &fl);
        char want[16];
        snprintf(want, sizeof want, "dgram %d", i);
        if (l == (ssize_t)strlen(want) && !strcmp(d, want) && from.sin_addr.s_addr == htonl(0x7f000001)) good++;
    }
    CHECK(good == 5, "5 UDP datagrams in order with the sender's address");
    close(u1);
    close(u2);
    printf("nettest: %d failure(s)\n", failures);
    return failures;
}
