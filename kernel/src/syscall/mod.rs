//! System calls, with Linux x86-64 numbers and conventions: number in rax, arguments in rdi,
//! rsi, rdx, r10, r8, r9, result (or -errno) in rax. Using Linux's ABI means a static program
//! built against Citadel's libc also runs unchanged on Linux, which is how the two kernels are
//! compared.
pub mod fs;
pub mod mm;
pub mod proc;
pub mod time;

use core::sync::atomic::{AtomicU64, Ordering};

use kcore::errno::*;

use crate::arch::{cpu, gdt, trap::{self, TrapFrame}};

pub fn init_msrs() {
    // SYSRET loads CS = base + 16 and SS = base + 8. The base carries RPL 3 (as in Linux): Intel
    // forces RPL 3 into the loaded selectors, but AMD does not, and a user SS of 0x18 (RPL 0)
    // would make the next IRET to user mode fault with #GP(0x18).
    cpu::wrmsr(cpu::MSR_STAR, (((gdt::KDATA as u64) | 3) << 48) | ((gdt::KCODE as u64) << 32));
    cpu::wrmsr(cpu::MSR_LSTAR, trap::syscall_entry_address());
    cpu::wrmsr(cpu::MSR_FMASK, 0x4_0700); // clear IF, DF, TF, AC on entry
}

/// Calls per system-call number (for /proc/syscalls and the profiler).
pub static COUNTS: [AtomicU64; 512] = [const { AtomicU64::new(0) }; 512];

pub extern "C" fn dispatch(f: &mut TrapFrame) {
    cpu::enable_irq();
    let nr = f.rax;
    if (nr as usize) < COUNTS.len() {
        // A statistic: a plain load and store (no locked instruction); rare lost updates are fine.
        let c = &COUNTS[nr as usize];
        c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
    let a = [f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9];
    let r: KResult<u64> = match nr {
        // files
        0 => fs::read(a[0] as i64, a[1], a[2] as usize),
        1 => fs::write(a[0] as i64, a[1], a[2] as usize),
        2 => fs::openat(fs::AT_FDCWD, a[0], a[1] as u32, a[2] as u32),
        3 => fs::close(a[0] as i64),
        4 => fs::stat(fs::AT_FDCWD, a[0], a[1], 0),
        5 => fs::fstat(a[0] as i64, a[1]),
        6 => fs::stat(fs::AT_FDCWD, a[0], a[1], fs::AT_SYMLINK_NOFOLLOW),
        7 => fs::poll(a[0], a[1] as usize, a[2] as i64),
        8 => fs::lseek(a[0] as i64, a[1] as i64, a[2] as u32),
        16 => fs::ioctl(a[0] as i64, a[1], a[2]),
        17 => fs::pread(a[0] as i64, a[1], a[2] as usize, a[3]),
        18 => fs::pwrite(a[0] as i64, a[1], a[2] as usize, a[3]),
        19 => fs::readv(a[0] as i64, a[1], a[2] as usize),
        20 => fs::writev(a[0] as i64, a[1], a[2] as usize),
        21 => fs::faccessat(fs::AT_FDCWD, a[0], a[1] as u32),
        22 => fs::pipe2(a[0], 0),
        23 => fs::select(a[0] as usize, a[1], a[2], a[3], a[4]),
        32 => fs::dup(a[0] as i64),
        33 => fs::dup3(a[0] as i64, a[1] as i64, 0, true),
        72 => fs::fcntl(a[0] as i64, a[1] as u32, a[2]),
        74 | 75 => fs::fsync(a[0] as i64),
        76 => fs::truncate(a[0], a[1]),
        77 => fs::ftruncate(a[0] as i64, a[1]),
        78 | 217 => fs::getdents64(a[0] as i64, a[1], a[2] as usize),
        79 => fs::getcwd(a[0], a[1] as usize),
        80 => fs::chdir(a[0]),
        81 => fs::fchdir(a[0] as i64),
        82 => fs::renameat(fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1]),
        83 => fs::mkdirat(fs::AT_FDCWD, a[0], a[1] as u32),
        84 => fs::unlinkat(fs::AT_FDCWD, a[0], fs::AT_REMOVEDIR),
        85 => fs::openat(fs::AT_FDCWD, a[0], 0o1101, a[1] as u32), // creat
        86 => fs::linkat(fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1]),
        87 => fs::unlinkat(fs::AT_FDCWD, a[0], 0),
        88 => fs::symlinkat(a[0], fs::AT_FDCWD, a[1]),
        89 => fs::readlinkat(fs::AT_FDCWD, a[0], a[1], a[2] as usize),
        90 => fs::chmod(fs::AT_FDCWD, a[0], a[1] as u32),
        91 => fs::fchmod(a[0] as i64, a[1] as u32),
        92 | 94 => fs::chown(fs::AT_FDCWD, a[0], a[1] as u32, a[2] as u32),
        93 => Ok(0), // fchown
        95 => fs::umask(a[0] as u32),
        133 => fs::mknodat(fs::AT_FDCWD, a[0], a[1] as u32),
        137 | 138 => fs::statfs(a[1]),
        161 => Ok(0), // chroot (not supported, accepted)
        162 => {
            crate::fs::ext2::sync_all();
            Ok(0)
        }
        257 => fs::openat(a[0] as i64, a[1], a[2] as u32, a[3] as u32),
        258 => fs::mkdirat(a[0] as i64, a[1], a[2] as u32),
        259 => fs::mknodat(a[0] as i64, a[1], a[2] as u32),
        262 => fs::stat(a[0] as i64, a[1], a[2], a[3] as u32),
        263 => fs::unlinkat(a[0] as i64, a[1], a[2] as u32),
        264 | 316 => fs::renameat(a[0] as i64, a[1], a[2] as i64, a[3]),
        265 => fs::linkat(a[0] as i64, a[1], a[2] as i64, a[3]),
        266 => fs::symlinkat(a[0], a[1] as i64, a[2]),
        267 => fs::readlinkat(a[0] as i64, a[1], a[2], a[3] as usize),
        268 => fs::chmod(a[0] as i64, a[1], a[2] as u32),
        269 | 439 => fs::faccessat(a[0] as i64, a[1], a[2] as u32),
        280 => fs::utimensat(a[0] as i64, a[1], a[2]),
        292 => fs::dup3(a[0] as i64, a[1] as i64, a[2] as u32, false),
        293 => fs::pipe2(a[0], a[1] as u32),
        // memory
        9 => mm::mmap(a[0], a[1], a[2] as u32, a[3] as u32, a[4] as i64, a[5]),
        10 => mm::mprotect(a[0], a[1], a[2] as u32),
        11 => mm::munmap(a[0], a[1]),
        12 => mm::brk(a[0]),
        25 => Err(ENOMEM), // mremap
        28 => mm::madvise(a[0], a[1], a[2] as u32),
        26 => mm::msync(a[0], a[1], a[2] as u32),
        // signals
        13 => proc::rt_sigaction(a[0] as u32, a[1], a[2]),
        14 => proc::rt_sigprocmask(a[0] as u32, a[1], a[2]),
        15 => crate::signal::sigreturn(f).map(|_| f.rax),
        62 => crate::signal::kill(a[0] as i64, a[1] as u32).map(|_| 0),
        127 => proc::rt_sigpending(a[0]),
        130 => proc::rt_sigsuspend(a[0]),
        131 => proc::sigaltstack(a[0], a[1], f.rsp),
        200 | 234 => crate::signal::kill(a[0] as i64, a[1] as u32).map(|_| 0), // tkill/tgkill
        // processes
        24 => {
            crate::sched::yield_now();
            Ok(0)
        }
        39 => Ok(crate::proc::current().pid as u64),
        203 => proc::sched_setaffinity(a[0] as u32, a[1] as usize, a[2]),
        204 => proc::sched_getaffinity(a[0] as u32, a[1] as usize, a[2]),
        56 => proc::clone(f, a[0]),
        57 | 58 => crate::proc::fork(f).map(|p| p as u64),
        59 => proc::execve(f, a[0], a[1], a[2]),
        60 | 231 => crate::proc::exit(((a[0] & 0xff) << 8) as i32),
        61 => proc::wait4(a[0] as i64, a[1], a[2], a[3]),
        63 => proc::uname(a[0]),
        97 | 302 => proc::getrlimit(a[0] as u32, if nr == 302 { a[3] } else { a[1] }),
        160 => Ok(0), // setrlimit
        98 => proc::getrusage(a[1]),
        99 => proc::sysinfo(a[0]),
        100 => time::times(a[0]),
        102 => Ok(crate::proc::current().uid.load(Ordering::Relaxed) as u64),
        107 => Ok(crate::proc::current().euid.load(Ordering::Relaxed) as u64),
        104 | 108 => Ok(0), // getgid, getegid
        105 => proc::setuid(a[0] as u32),
        106 | 116 | 117 | 119 => Ok(0), // setgid, setgroups, setresuid, setresgid
        115 => Ok(0),                         // getgroups: none
        109 => proc::setpgid(a[0] as u32, a[1] as u32),
        110 => Ok(crate::proc::current().ppid.load(Ordering::Relaxed) as u64),
        111 => Ok(crate::proc::current().pgid.load(Ordering::Relaxed) as u64),
        112 => proc::setsid(),
        121 => proc::getpgid(a[0] as u32),
        124 => proc::getsid(a[0] as u32),
        157 => Ok(0), // prctl
        158 => proc::arch_prctl(a[0], a[1]),
        186 => Ok(crate::sched::current().tid as u64),
        202 => proc::futex(a[0], a[1] as u32, a[2] as u32),
        218 => Ok(crate::sched::current().tid as u64), // set_tid_address
        273 => Ok(0), // set_robust_list
        318 => proc::getrandom(a[0], a[1] as usize),
        334 => Err(ENOSYS), // rseq
        // time
        35 => time::nanosleep(a[0], a[1]),
        37 => Ok(0), // alarm
        96 => time::gettimeofday(a[0]),
        201 => time::time(a[0]),
        228 => time::clock_gettime(a[0] as u32, a[1]),
        229 => time::clock_getres(a[0] as u32, a[1]),
        230 => time::clock_nanosleep(a[0] as u32, a[1] as u32, a[2], a[3]),
        // sockets
        41..=55 | 288 | 299 => crate::net::syscall(nr, &a),
        // Citadel-specific: 1000 = profiler control, 1001 = power off with an exit code
        1000 => crate::profile::control(a[0], a[1]),
        1001 => crate::poweroff(a[0] as u32),
        _ => Err(ENOSYS),
    };
    // rt_sigreturn restored rax itself.
    if nr != 15 || r.is_err() {
        f.rax = match r {
            Ok(v) => v,
            Err(e) => (-e) as u64,
        };
    }
    // sysret cannot return to a non-canonical address.
    if f.rip >= kcore::elf::USER_TOP {
        f.vector = 0;
    }
    if crate::arch::percpu::this().sched.need_resched.load(Ordering::Relaxed) {
        crate::sched::schedule();
    }
    crate::signal::deliver_pending(f);
}
