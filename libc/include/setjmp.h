#ifndef _SETJMP_H
#define _SETJMP_H
typedef unsigned long jmp_buf[8];
typedef unsigned long sigjmp_buf[9];
int setjmp(jmp_buf env);
_Noreturn void longjmp(jmp_buf env, int v);
int _setjmp(jmp_buf env);
_Noreturn void _longjmp(jmp_buf env, int v);
#define sigsetjmp(env, save) setjmp(env)
#define siglongjmp(env, v) longjmp(env, v)
#endif
