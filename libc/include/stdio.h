#ifndef _STDIO_H
#define _STDIO_H
#include <stddef.h>
#include <stdarg.h>
#include <sys/types.h>
#define EOF (-1)
#define BUFSIZ 4096
#define FILENAME_MAX 4096
#define FOPEN_MAX 64
#define L_tmpnam 32
#define TMP_MAX 10000
#define _IOFBF 0
#define _IOLBF 1
#define _IONBF 2
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
typedef struct FILE FILE;
typedef long fpos_t;
extern FILE *stdin, *stdout, *stderr;
FILE *fopen(const char *restrict path, const char *restrict mode);
FILE *fdopen(int fd, const char *mode);
FILE *freopen(const char *restrict path, const char *restrict mode, FILE *restrict f);
FILE *tmpfile(void);
char *tmpnam(char *s);
int fclose(FILE *f);
int fflush(FILE *f);
size_t fread(void *restrict p, size_t sz, size_t n, FILE *restrict f);
size_t fwrite(const void *restrict p, size_t sz, size_t n, FILE *restrict f);
int fgetc(FILE *f);
int getc(FILE *f);
int getchar(void);
int ungetc(int c, FILE *f);
char *fgets(char *restrict s, int n, FILE *restrict f);
ssize_t getline(char **restrict line, size_t *restrict cap, FILE *restrict f);
ssize_t getdelim(char **restrict line, size_t *restrict cap, int delim, FILE *restrict f);
int fputc(int c, FILE *f);
int putc(int c, FILE *f);
int putchar(int c);
int fputs(const char *restrict s, FILE *restrict f);
int puts(const char *s);
int fseek(FILE *f, long off, int whence);
int fseeko(FILE *f, off_t off, int whence);
long ftell(FILE *f);
off_t ftello(FILE *f);
void rewind(FILE *f);
int fgetpos(FILE *restrict f, fpos_t *restrict pos);
int fsetpos(FILE *f, const fpos_t *pos);
int feof(FILE *f);
int ferror(FILE *f);
void clearerr(FILE *f);
int fileno(FILE *f);
int setvbuf(FILE *restrict f, char *restrict buf, int mode, size_t size);
void setbuf(FILE *restrict f, char *restrict buf);
int printf(const char *restrict fmt, ...);
int fprintf(FILE *restrict f, const char *restrict fmt, ...);
int dprintf(int fd, const char *restrict fmt, ...);
int sprintf(char *restrict s, const char *restrict fmt, ...);
int snprintf(char *restrict s, size_t n, const char *restrict fmt, ...);
int vprintf(const char *restrict fmt, va_list ap);
int vfprintf(FILE *restrict f, const char *restrict fmt, va_list ap);
int vsprintf(char *restrict s, const char *restrict fmt, va_list ap);
int vsnprintf(char *restrict s, size_t n, const char *restrict fmt, va_list ap);
int vdprintf(int fd, const char *restrict fmt, va_list ap);
int scanf(const char *restrict fmt, ...);
int fscanf(FILE *restrict f, const char *restrict fmt, ...);
int sscanf(const char *restrict s, const char *restrict fmt, ...);
int vsscanf(const char *restrict s, const char *restrict fmt, va_list ap);
int vfscanf(FILE *restrict f, const char *restrict fmt, va_list ap);
void perror(const char *s);
int remove(const char *path);
int rename(const char *old, const char *new);
FILE *popen(const char *cmd, const char *mode);
int pclose(FILE *f);
#endif
