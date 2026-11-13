#ifndef _STDLIB_H
#define _STDLIB_H
#include <stddef.h>
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 2147483647
#define MB_CUR_MAX 1
typedef struct { int quot, rem; } div_t;
typedef struct { long quot, rem; } ldiv_t;
typedef struct { long long quot, rem; } lldiv_t;
void *malloc(size_t n);
void *calloc(size_t n, size_t sz);
void *realloc(void *p, size_t n);
void free(void *p);
void *aligned_alloc(size_t al, size_t n);
int posix_memalign(void **out, size_t al, size_t n);
_Noreturn void exit(int code);
_Noreturn void _Exit(int code);
_Noreturn void abort(void);
int atexit(void (*f)(void));
int atoi(const char *s);
long atol(const char *s);
long long atoll(const char *s);
double atof(const char *s);
long strtol(const char *restrict s, char **restrict end, int base);
unsigned long strtoul(const char *restrict s, char **restrict end, int base);
long long strtoll(const char *restrict s, char **restrict end, int base);
unsigned long long strtoull(const char *restrict s, char **restrict end, int base);
double strtod(const char *restrict s, char **restrict end);
float strtof(const char *restrict s, char **restrict end);
long double strtold(const char *restrict s, char **restrict end);
int abs(int x);
long labs(long x);
long long llabs(long long x);
div_t div(int a, int b);
ldiv_t ldiv(long a, long b);
void qsort(void *base, size_t n, size_t sz, int (*cmp)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t n, size_t sz, int (*cmp)(const void *, const void *));
int rand(void);
void srand(unsigned seed);
long random(void);
void srandom(unsigned seed);
char *getenv(const char *name);
int setenv(const char *name, const char *value, int overwrite);
int unsetenv(const char *name);
int putenv(char *s);
int system(const char *cmd);
char *realpath(const char *restrict path, char *restrict resolved);
int mkstemp(char *tmpl);
char *mktemp(char *tmpl);
#endif
