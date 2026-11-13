/* Clang's limits.h covers the C types; POSIX adds these. */
#ifndef _CITADEL_LIMITS_H
#define _CITADEL_LIMITS_H
#include_next <limits.h>
#define PATH_MAX 4096
#define NAME_MAX 255
#define PIPE_BUF 4096
#define ARG_MAX 131072
#define OPEN_MAX 1024
#define LINE_MAX 2048
#define NGROUPS_MAX 32
#define _POSIX_PATH_MAX 256
#define _POSIX_NAME_MAX 14
#define _POSIX_OPEN_MAX 20
#define IOV_MAX 1024
#endif
