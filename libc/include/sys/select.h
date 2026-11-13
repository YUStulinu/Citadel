#ifndef _SYS_SELECT_H
#define _SYS_SELECT_H
#include <sys/types.h>
#define FD_SETSIZE 1024
typedef struct { unsigned long fds_bits[FD_SETSIZE / 64]; } fd_set;
#define FD_ZERO(s) __builtin_memset((s), 0, sizeof(fd_set))
#define FD_SET(fd, s) ((s)->fds_bits[(fd) / 64] |= (1UL << ((fd) % 64)))
#define FD_CLR(fd, s) ((s)->fds_bits[(fd) / 64] &= ~(1UL << ((fd) % 64)))
#define FD_ISSET(fd, s) (((s)->fds_bits[(fd) / 64] >> ((fd) % 64)) & 1)
struct timeval;
int select(int n, fd_set *restrict r, fd_set *restrict w, fd_set *restrict e, struct timeval *restrict tv);
#endif
