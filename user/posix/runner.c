/* posixrun: runs Open POSIX Test Suite programs and reports what each returned.
   Exit codes are the suite's (posixtest.h): 0 PASS, 1 FAIL, 2 UNRESOLVED, 4 UNSUPPORTED,
   5 UNTESTED; a test killed by a signal is a CRASH, one running over the limit HUNG.
   "-buildonly" tests pass by having compiled.
     posixrun DIR [SECONDS]      prints "<result> <test>" lines and a summary */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int cmp(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    const char *dir = argc > 1 ? argv[1] : "/posix";
    int limit = argc > 2 ? atoi(argv[2]) : 20;
    DIR *d = opendir(dir);
    if (!d) {
        perror(dir);
        return 2;
    }
    char **names = NULL;
    size_t n = 0, cap = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.') continue;
        if (n == cap) {
            cap = cap ? cap * 2 : 256;
            names = realloc(names, cap * sizeof *names);
        }
        names[n++] = strdup(e->d_name);
    }
    closedir(d);
    qsort(names, n, sizeof *names, cmp);
    const char *labels[] = {"PASS", "FAIL", "UNRESOLVED", "UNINITIALIZED", "UNSUPPORTED", "UNTESTED"};
    int counts[8] = {0}; /* 0..5 as above, 6 CRASH, 7 HUNG */
    double t0 = now();
    for (size_t i = 0; i < n; i++) {
        char path[512];
        snprintf(path, sizeof path, "%s/%s", dir, names[i]);
        int result;
        if (strstr(names[i], "buildonly")) {
            result = 0;
        } else {
            fflush(stdout);
            pid_t p = fork();
            if (p == 0) {
                /* The test runs in its own process group so a stray kill(0, ...) stays inside. */
                setpgid(0, 0);
                int null = open("/dev/null", 1);
                if (null >= 0) {
                    dup2(null, 1);
                    dup2(null, 2);
                }
                char *av[] = {names[i], NULL};
                execv(path, av);
                _exit(127);
            }
            double start = now();
            int st = 0;
            result = -1;
            for (;;) {
                pid_t r = waitpid(p, &st, WNOHANG);
                if (r == p) break;
                if (now() - start > limit) {
                    kill(-p, SIGKILL);
                    kill(p, SIGKILL);
                    waitpid(p, &st, 0);
                    result = 7;
                    break;
                }
                struct timespec ts = {0, 2000000};
                nanosleep(&ts, NULL);
            }
            if (result < 0) {
                if (WIFSIGNALED(st)) result = 6;
                else {
                    int code = WEXITSTATUS(st);
                    result = code <= 5 ? code : 1;
                }
            }
        }
        counts[result]++;
        printf("%s %s\n", result == 6 ? "CRASH" : result == 7 ? "HUNG" : labels[result], names[i]);
    }
    printf("posixrun: %zu tests in %.1f s: %d pass, %d fail, %d unresolved, %d unsupported, %d untested, %d crash, %d hung\n", n,
           now() - t0, counts[0], counts[1], counts[2], counts[4], counts[5], counts[6], counts[7]);
    return 0;
}
