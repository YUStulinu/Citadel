#ifndef _TERMIOS_H
#define _TERMIOS_H
typedef unsigned char cc_t;
typedef unsigned int speed_t;
typedef unsigned int tcflag_t;
#define NCCS 32
/* The kernel's struct termios has 19 control characters; the extra bytes are ignored. */
struct termios {
    tcflag_t c_iflag, c_oflag, c_cflag, c_lflag;
    cc_t c_line;
    cc_t c_cc[NCCS];
    speed_t c_ispeed, c_ospeed;
};
#define VINTR 0
#define VQUIT 1
#define VERASE 2
#define VKILL 3
#define VEOF 4
#define VTIME 5
#define VMIN 6
#define BRKINT 02
#define INPCK 020
#define ISTRIP 040
#define ICRNL 0400
#define IXON 02000
#define OPOST 01
#define ONLCR 04
#define CS8 060
#define ISIG 01
#define ICANON 02
#define ECHO 010
#define ECHOE 020
#define ECHOK 040
#define ECHONL 0100
#define IEXTEN 0100000
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2
int tcgetattr(int fd, struct termios *t);
int tcsetattr(int fd, int act, const struct termios *t);
#endif
