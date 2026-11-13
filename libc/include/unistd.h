#ifndef _UNISTD_H
#define _UNISTD_H
#include <stddef.h>
#include <sys/types.h>
#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define _SC_PAGESIZE 30
#define _SC_PAGE_SIZE 30
#define _SC_CLK_TCK 2
#define _SC_OPEN_MAX 4
#define _SC_NPROCESSORS_ONLN 84
#define _SC_NPROCESSORS_CONF 83
#define _POSIX_VERSION 200809L
extern char **environ;
extern char *optarg;
extern int optind, opterr, optopt;
ssize_t read(int fd, void *buf, size_t n);
ssize_t write(int fd, const void *buf, size_t n);
ssize_t pread(int fd, void *buf, size_t n, off_t off);
ssize_t pwrite(int fd, const void *buf, size_t n, off_t off);
int close(int fd);
off_t lseek(int fd, off_t off, int whence);
int pipe(int fds[2]);
int pipe2(int fds[2], int flags);
int dup(int fd);
int dup2(int a, int b);
pid_t fork(void);
pid_t vfork(void);
int execve(const char *path, char *const argv[], char *const envp[]);
int execv(const char *path, char *const argv[]);
int execvp(const char *file, char *const argv[]);
int execl(const char *path, const char *arg, ...);
int execlp(const char *file, const char *arg, ...);
_Noreturn void _exit(int code);
pid_t getpid(void);
pid_t getppid(void);
pid_t getpgrp(void);
pid_t getpgid(pid_t pid);
int setpgid(pid_t pid, pid_t pgid);
pid_t setsid(void);
pid_t getsid(pid_t pid);
pid_t tcgetpgrp(int fd);
int tcsetpgrp(int fd, pid_t pg);
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
int setuid(uid_t u);
int setgid(gid_t g);
int chdir(const char *path);
int fchdir(int fd);
char *getcwd(char *buf, size_t n);
int access(const char *path, int mode);
int unlink(const char *path);
int rmdir(const char *path);
int link(const char *a, const char *b);
int symlink(const char *a, const char *b);
ssize_t readlink(const char *restrict path, char *restrict buf, size_t n);
int chown(const char *path, uid_t u, gid_t g);
int truncate(const char *path, off_t len);
int ftruncate(int fd, off_t len);
int fsync(int fd);
void sync(void);
unsigned sleep(unsigned s);
int usleep(useconds_t us);
unsigned alarm(unsigned s);
int pause(void);
int isatty(int fd);
char *ttyname(int fd);
long sysconf(int name);
int getpagesize(void);
int gethostname(char *name, size_t n);
int getopt(int argc, char *const argv[], const char *opts);
void *sbrk(long inc);
int brk(void *addr);
#endif
