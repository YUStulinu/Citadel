//! File-system system calls.
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use kcore::errno::*;

use crate::fs::{self, File, InodeRef, Kind, O_ACCMODE, O_CLOEXEC, O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_NONBLOCK, O_TRUNC, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::proc;
use crate::user;

pub const AT_FDCWD: i64 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_EMPTY_PATH: u32 = 0x1000;

fn file(fd: i64) -> KResult<Arc<File>> {
    proc::current().fds.lock().get(fd)
}

/// The absolute path for (dirfd, path).
pub fn abs_path(dirfd: i64, path: &str) -> KResult<String> {
    if path.starts_with('/') {
        return Ok(if kcore::path::is_normal(path) { String::from(path) } else { kcore::path::normalize("/", path) });
    }
    let base = if dirfd == AT_FDCWD {
        proc::current().cwd.lock().clone()
    } else {
        let f = file(dirfd)?;
        if f.inode.kind() != Kind::Dir {
            return Err(ENOTDIR);
        }
        f.path.clone()
    };
    Ok(kcore::path::normalize(&base, path))
}

pub fn read(fd: i64, buf: u64, len: usize) -> KResult<u64> {
    let f = file(fd)?;
    // The user buffer is checked and faulted in, then filled directly (no bounce buffer).
    let dst = unsafe { user::slice_mut(buf, len.min(1 << 24))? };
    Ok(f.read(dst)? as u64)
}

pub fn write(fd: i64, buf: u64, len: usize) -> KResult<u64> {
    let f = file(fd)?;
    let src = unsafe { user::slice(buf, len.min(1 << 24))? };
    Ok(f.write(src)? as u64)
}

pub fn pread(fd: i64, buf: u64, len: usize, off: u64) -> KResult<u64> {
    let f = file(fd)?;
    if f.inode.kind() != Kind::File {
        return Err(ESPIPE);
    }
    let mut tmp = alloc::vec![0u8; len.min(1 << 20)];
    let n = f.inode.read_at(off, &mut tmp)?;
    user::copy_out(buf, &tmp[..n])?;
    Ok(n as u64)
}

pub fn pwrite(fd: i64, buf: u64, len: usize, off: u64) -> KResult<u64> {
    let f = file(fd)?;
    if f.inode.kind() != Kind::File {
        return Err(ESPIPE);
    }
    if !f.writable() {
        return Err(EBADF);
    }
    let data = user::read_vec(buf, len.min(1 << 20))?;
    Ok(f.inode.write_at(off, &data)? as u64)
}

fn iovecs(iov: u64, cnt: usize) -> KResult<Vec<(u64, usize)>> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    (0..cnt).map(|i| Ok((user::get_u64(iov + 16 * i as u64)?, user::get_u64(iov + 16 * i as u64 + 8)? as usize))).collect()
}

pub fn readv(fd: i64, iov: u64, cnt: usize) -> KResult<u64> {
    let mut total = 0;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let n = read(fd, base, len)?;
        total += n;
        if (n as usize) < len {
            break;
        }
    }
    Ok(total)
}

pub fn writev(fd: i64, iov: u64, cnt: usize) -> KResult<u64> {
    let mut total = 0;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let n = write(fd, base, len)?;
        total += n;
        if (n as usize) < len {
            break;
        }
    }
    Ok(total)
}

/// Opens (and creates) a file, returning the new descriptor.
pub fn open_path(abs: &str, flags: u32, mode: u32) -> KResult<Arc<File>> {
    let p = proc::current();
    let inode: InodeRef = if flags & O_CREAT != 0 {
        let (dir, name, _) = fs::resolve_parent(abs)?;
        match dir.lookup(&name) {
            Ok(existing) => {
                if flags & O_EXCL != 0 {
                    return Err(EEXIST);
                }
                if existing.kind() == Kind::Symlink && flags & O_NOFOLLOW == 0 {
                    fs::resolve(abs, true)?.1
                } else {
                    existing
                }
            }
            Err(ENOENT) => dir.create(&name, Kind::File, mode & !p.umask.load(Ordering::Relaxed) & 0o7777)?,
            Err(e) => return Err(e),
        }
    } else {
        let (_, i) = fs::resolve(abs, flags & O_NOFOLLOW == 0)?;
        i
    };
    if inode.kind() == Kind::Symlink {
        return Err(ELOOP);
    }
    if flags & O_DIRECTORY != 0 && inode.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    let acc = flags & O_ACCMODE;
    if inode.kind() == Kind::Dir && acc != fs::O_RDONLY {
        return Err(EISDIR);
    }
    // A named pipe: hand out one end of its pipe.
    let inode: InodeRef = match inode.as_any().downcast_ref::<fs::pipe::Fifo>() {
        Some(fifo) => {
            let end = fifo.open_end(acc != fs::O_RDONLY);
            end.nonblock.store(flags & O_NONBLOCK != 0, Ordering::Relaxed);
            end
        }
        None => inode,
    };
    if flags & O_TRUNC != 0 && inode.kind() == Kind::File && acc != fs::O_RDONLY {
        inode.truncate(0)?;
    }
    Ok(File::new(inode, flags, String::from(abs)))
}

pub fn openat(dirfd: i64, path: u64, flags: u32, mode: u32) -> KResult<u64> {
    let path = user::read_path(path)?;
    let abs = abs_path(dirfd, &path)?;
    let f = open_path(&abs, flags, mode)?;
    let fd = proc::current().fds.lock().alloc(f, flags & O_CLOEXEC != 0, 0)?;
    Ok(fd as u64)
}

pub fn close(fd: i64) -> KResult<u64> {
    let p = proc::current();
    let entry = {
        let mut t = p.fds.lock();
        let e = t.fds.get_mut(fd.max(0) as usize).and_then(|e| e.take());
        if fd < 0 || e.is_none() {
            return Err(EBADF);
        }
        e
    };
    drop(entry); // may release a pipe end or a socket (outside the table lock)
    Ok(0)
}

/// Linux's struct stat on x86-64.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LinuxStat {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    _pad0: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: u64,
    atime_ns: u64,
    mtime: u64,
    mtime_ns: u64,
    ctime: u64,
    ctime_ns: u64,
    _unused: [i64; 3],
}

fn put_stat(addr: u64, inode: &InodeRef) -> KResult<u64> {
    let s = inode.stat();
    let ls = LinuxStat {
        dev: s.dev,
        ino: s.ino,
        nlink: s.nlink as u64,
        mode: inode.kind().mode_bits() | s.mode,
        uid: s.uid,
        gid: s.gid,
        rdev: s.rdev,
        size: s.size as i64,
        blksize: 4096,
        blocks: s.blocks as i64,
        atime: s.atime,
        mtime: s.mtime,
        ctime: s.ctime,
        ..Default::default()
    };
    user::write_struct(addr, &ls)?;
    Ok(0)
}

pub fn stat(dirfd: i64, path: u64, buf: u64, flags: u32) -> KResult<u64> {
    let p = user::read_cstr(path, 4096)?;
    if p.is_empty() {
        if flags & AT_EMPTY_PATH != 0 {
            return fstat(dirfd, buf);
        }
        return Err(ENOENT);
    }
    let abs = abs_path(dirfd, &p)?;
    let (_, inode) = fs::resolve(&abs, flags & AT_SYMLINK_NOFOLLOW == 0)?;
    put_stat(buf, &inode)
}

pub fn fstat(fd: i64, buf: u64) -> KResult<u64> {
    put_stat(buf, &file(fd)?.inode)
}

pub fn lseek(fd: i64, off: i64, whence: u32) -> KResult<u64> {
    let f = file(fd)?;
    match f.inode.kind() {
        Kind::Fifo | Kind::Socket | Kind::CharDev => return Err(ESPIPE),
        _ => {}
    }
    let mut cur = f.offset.lock();
    let base = match whence {
        0 => 0,
        1 => *cur as i64,
        2 => f.inode.stat().size as i64,
        _ => return Err(EINVAL),
    };
    let n = base.checked_add(off).ok_or(EINVAL)?;
    if n < 0 {
        return Err(EINVAL);
    }
    *cur = n as u64;
    Ok(n as u64)
}

pub fn ioctl(fd: i64, cmd: u64, arg: u64) -> KResult<u64> {
    let f = file(fd)?;
    const FIONBIO: u64 = 0x5421;
    const FIOCLEX: u64 = 0x5451;
    if cmd == FIONBIO {
        let on = user::get_u32(arg)? != 0;
        if on {
            f.flags.fetch_or(O_NONBLOCK, Ordering::Relaxed);
        } else {
            f.flags.fetch_and(!O_NONBLOCK, Ordering::Relaxed);
        }
        set_nonblock_inode(&f);
        return Ok(0);
    }
    if cmd == FIOCLEX {
        return Ok(0);
    }
    f.inode.ioctl(cmd, arg)
}

fn set_nonblock_inode(f: &Arc<File>) {
    let nb = f.nonblocking();
    if let Some(p) = f.inode.as_any().downcast_ref::<fs::pipe::PipeEnd>() {
        p.nonblock.store(nb, Ordering::Relaxed);
    }
    if let Some(s) = f.inode.as_any().downcast_ref::<crate::net::socket::Socket>() {
        s.set_nonblock(nb);
    }
}

pub fn faccessat(dirfd: i64, path: u64, _mode: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    fs::resolve(&abs_path(dirfd, &p)?, true)?;
    Ok(0) // root may do anything
}

pub fn pipe2(fds: u64, flags: u32) -> KResult<u64> {
    let (r, w) = fs::pipe::pipe();
    let nb = flags & O_NONBLOCK != 0;
    r.nonblock.store(nb, Ordering::Relaxed);
    w.nonblock.store(nb, Ordering::Relaxed);
    let p = proc::current();
    let (a, b) = {
        let mut t = p.fds.lock();
        let a = t.alloc(File::new(r, fs::O_RDONLY | (flags & O_NONBLOCK), String::from("pipe:[r]")), flags & O_CLOEXEC != 0, 0)?;
        let b = match t.alloc(File::new(w, fs::O_WRONLY | (flags & O_NONBLOCK), String::from("pipe:[w]")), flags & O_CLOEXEC != 0, 0) {
            Ok(b) => b,
            Err(e) => {
                t.close(a)?;
                return Err(e);
            }
        };
        (a, b)
    };
    user::put_i32(fds, a as i32)?;
    user::put_i32(fds + 4, b as i32)?;
    Ok(0)
}

pub fn dup(fd: i64) -> KResult<u64> {
    let p = proc::current();
    let mut t = p.fds.lock();
    let f = t.get(fd)?;
    Ok(t.alloc(f, false, 0)? as u64)
}

pub fn dup3(old: i64, new: i64, flags: u32, dup2: bool) -> KResult<u64> {
    let p = proc::current();
    let mut t = p.fds.lock();
    let f = t.get(old)?;
    if old == new {
        return if dup2 { Ok(new as u64) } else { Err(EINVAL) };
    }
    if new < 0 {
        return Err(EBADF);
    }
    let prev = t.fds.get_mut(new as usize).and_then(|e| e.take());
    t.set(new as usize, f, flags & O_CLOEXEC != 0)?;
    drop(t);
    drop(prev);
    Ok(new as u64)
}

pub fn fcntl(fd: i64, cmd: u32, arg: u64) -> KResult<u64> {
    let p = proc::current();
    let mut t = p.fds.lock();
    let f = t.get(fd)?;
    match cmd {
        0 | 1030 => Ok(t.alloc(f, cmd == 1030, arg as usize)? as u64), // F_DUPFD(_CLOEXEC)
        1 => Ok(t.fds[fd as usize].as_ref().map(|e| e.cloexec as u64).unwrap_or(0)), // F_GETFD
        2 => {
            if let Some(e) = t.fds[fd as usize].as_mut() {
                e.cloexec = arg & 1 != 0;
            }
            Ok(0)
        }
        3 => Ok(f.flags.load(Ordering::Relaxed) as u64), // F_GETFL
        4 => {
            let keep = f.flags.load(Ordering::Relaxed) & !(O_NONBLOCK | fs::O_APPEND);
            f.flags.store(keep | (arg as u32 & (O_NONBLOCK | fs::O_APPEND)), Ordering::Relaxed);
            drop(t);
            set_nonblock_inode(&f);
            Ok(0)
        }
        5..=7 => Ok(0), // locks: always granted (single user)
        _ => Err(EINVAL),
    }
}

pub fn fsync(fd: i64) -> KResult<u64> {
    file(fd)?.inode.sync()?;
    Ok(0)
}

pub fn truncate(path: u64, len: u64) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (_, i) = fs::resolve(&abs_path(AT_FDCWD, &p)?, true)?;
    if i.kind() == Kind::Dir {
        return Err(EISDIR);
    }
    i.truncate(len)?;
    Ok(0)
}

pub fn ftruncate(fd: i64, len: u64) -> KResult<u64> {
    let f = file(fd)?;
    if !f.writable() {
        return Err(EINVAL);
    }
    f.inode.truncate(len)?;
    Ok(0)
}

pub fn getdents64(fd: i64, buf: u64, len: usize) -> KResult<u64> {
    let f = file(fd)?;
    if f.inode.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    let mut entries = alloc::vec![
        fs::DirEntry { name: String::from("."), ino: f.inode.stat().ino, kind: Kind::Dir },
        fs::DirEntry { name: String::from(".."), ino: 1, kind: Kind::Dir },
    ];
    entries.extend(f.inode.readdir()?);
    let mut off = f.offset.lock();
    let mut out = Vec::new();
    let mut i = *off as usize;
    while i < entries.len() {
        let e = &entries[i];
        let reclen = (19 + e.name.len() + 1 + 7) & !7;
        if out.len() + reclen > len {
            break;
        }
        let start = out.len();
        out.resize(start + reclen, 0);
        out[start..start + 8].copy_from_slice(&e.ino.to_le_bytes());
        out[start + 8..start + 16].copy_from_slice(&((i + 1) as u64).to_le_bytes());
        out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
        out[start + 18] = e.kind.dtype();
        out[start + 19..start + 19 + e.name.len()].copy_from_slice(e.name.as_bytes());
        i += 1;
    }
    if out.is_empty() && i < entries.len() {
        return Err(EINVAL);
    }
    user::copy_out(buf, &out)?;
    *off = i as u64;
    Ok(out.len() as u64)
}

pub fn getcwd(buf: u64, len: usize) -> KResult<u64> {
    let cwd = proc::current().cwd.lock().clone();
    if cwd.len() + 1 > len {
        return Err(ERANGE);
    }
    let mut v = Vec::from(cwd.as_bytes());
    v.push(0);
    user::copy_out(buf, &v)?;
    Ok(v.len() as u64)
}

pub fn chdir(path: u64) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (canon, i) = fs::resolve(&abs_path(AT_FDCWD, &p)?, true)?;
    if i.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    *proc::current().cwd.lock() = canon;
    Ok(0)
}

pub fn fchdir(fd: i64) -> KResult<u64> {
    let f = file(fd)?;
    if f.inode.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    *proc::current().cwd.lock() = f.path.clone();
    Ok(0)
}

pub fn mkdirat(dirfd: i64, path: u64, mode: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (dir, name, _) = fs::resolve_parent(&abs_path(dirfd, &p)?)?;
    if dir.lookup(&name).is_ok() {
        return Err(EEXIST);
    }
    let um = proc::current().umask.load(Ordering::Relaxed);
    dir.create(&name, Kind::Dir, mode & !um & 0o7777)?;
    Ok(0)
}

pub fn mknodat(dirfd: i64, path: u64, mode: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (dir, name, _) = fs::resolve_parent(&abs_path(dirfd, &p)?)?;
    let kind = match mode & 0o170000 {
        0o010000 => Kind::Fifo,
        0 | 0o100000 => Kind::File,
        _ => return Err(EPERM),
    };
    if dir.lookup(&name).is_ok() {
        return Err(EEXIST);
    }
    dir.create(&name, kind, mode & 0o7777)?;
    Ok(0)
}

pub fn unlinkat(dirfd: i64, path: u64, flags: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (dir, name, _) = fs::resolve_parent(&abs_path(dirfd, &p)?)?;
    if flags & AT_REMOVEDIR != 0 {
        if name == "." {
            return Err(EINVAL);
        }
        dir.rmdir(&name)?;
    } else {
        dir.unlink(&name)?;
    }
    Ok(0)
}

pub fn renameat(olddir: i64, old: u64, newdir: i64, new: u64) -> KResult<u64> {
    let o = user::read_path(old)?;
    let n = user::read_path(new)?;
    let oa = abs_path(olddir, &o)?;
    let na = abs_path(newdir, &n)?;
    if na.starts_with(&oa) && na.as_bytes().get(oa.len()) == Some(&b'/') {
        return Err(EINVAL); // into its own subdirectory
    }
    let (od, oname, _) = fs::resolve_parent(&oa)?;
    let (nd, nname, _) = fs::resolve_parent(&na)?;
    od.rename(&oname, &nd, &nname)?;
    Ok(0)
}

pub fn linkat(olddir: i64, old: u64, newdir: i64, new: u64) -> KResult<u64> {
    let o = user::read_path(old)?;
    let n = user::read_path(new)?;
    let (_, target) = fs::resolve(&abs_path(olddir, &o)?, false)?;
    let (dir, name, _) = fs::resolve_parent(&abs_path(newdir, &n)?)?;
    dir.link(&name, &target)?;
    Ok(0)
}

pub fn symlinkat(target: u64, dirfd: i64, path: u64) -> KResult<u64> {
    let t = user::read_path(target)?;
    let p = user::read_path(path)?;
    let (dir, name, _) = fs::resolve_parent(&abs_path(dirfd, &p)?)?;
    dir.symlink(&name, &t)?;
    Ok(0)
}

pub fn readlinkat(dirfd: i64, path: u64, buf: u64, len: usize) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (_, i) = fs::resolve(&abs_path(dirfd, &p)?, false)?;
    let s = i.readlink()?;
    let n = s.len().min(len);
    user::copy_out(buf, &s.as_bytes()[..n])?;
    Ok(n as u64)
}

pub fn chmod(dirfd: i64, path: u64, mode: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (_, i) = fs::resolve(&abs_path(dirfd, &p)?, true)?;
    i.chmod(mode)?;
    Ok(0)
}

pub fn fchmod(fd: i64, mode: u32) -> KResult<u64> {
    file(fd)?.inode.chmod(mode)?;
    Ok(0)
}

pub fn chown(dirfd: i64, path: u64, uid: u32, gid: u32) -> KResult<u64> {
    let p = user::read_path(path)?;
    let (_, i) = fs::resolve(&abs_path(dirfd, &p)?, true)?;
    i.chown(uid, gid)?;
    Ok(0)
}

pub fn umask(m: u32) -> KResult<u64> {
    Ok(proc::current().umask.swap(m & 0o777, Ordering::Relaxed) as u64)
}

pub fn utimensat(dirfd: i64, path: u64, times: u64) -> KResult<u64> {
    let inode = if path == 0 {
        file(dirfd)?.inode.clone()
    } else {
        let p = user::read_path(path)?;
        fs::resolve(&abs_path(dirfd, &p)?, true)?.1
    };
    let now = fs::now_secs();
    let (a, m) = if times == 0 { (now, now) } else { (user::get_u64(times)?, user::get_u64(times + 16)?) };
    inode.set_times(a, m)?;
    Ok(0)
}

pub fn statfs(buf: u64) -> KResult<u64> {
    let mut s = [0u64; 15];
    s[0] = 0x0102_1994; // f_type: tmpfs
    s[1] = 4096;
    s[2] = crate::mm::frame::total_bytes() / 4096;
    s[3] = crate::mm::frame::free_bytes() / 4096;
    s[4] = s[3];
    s[8] = 255; // f_namelen
    user::write_struct(buf, &s)?;
    Ok(0)
}

// ---------------------------------------------------------------- poll / select

fn ready(fd: i64, events: u16) -> u16 {
    match file(fd) {
        Ok(f) => {
            let r = f.inode.poll();
            r & (events | POLLERR | POLLHUP)
        }
        Err(_) => 0x20, // POLLNVAL
    }
}

/// Waits until one of `check` holds or the timeout (ms, negative = forever) passes.
fn wait_any(timeout_ms: i64, mut check: impl FnMut() -> bool) -> KResult<()> {
    let deadline = if timeout_ms < 0 { u64::MAX } else { crate::time::monotonic_ns() + timeout_ms as u64 * 1_000_000 };
    if check() || timeout_ms == 0 {
        return Ok(());
    }
    if deadline != u64::MAX {
        crate::sched::arm_timer(deadline - crate::time::monotonic_ns() + crate::arch::apic::now_ns());
    }
    let ok = crate::sched::POLL_WQ.wait_until(|| check() || crate::time::monotonic_ns() >= deadline);
    if !ok {
        return Err(EINTR);
    }
    Ok(())
}

pub fn poll(fds: u64, n: usize, timeout: i64) -> KResult<u64> {
    if n > 1024 {
        return Err(EINVAL);
    }
    let mut v: Vec<(i32, u16, u16)> = (0..n)
        .map(|i| {
            let a = fds + 8 * i as u64;
            Ok((user::get_u32(a)? as i32, (user::get_u32(a + 4)? & 0xffff) as u16, 0))
        })
        .collect::<KResult<_>>()?;
    let mut count = 0;
    wait_any(timeout, || {
        count = 0;
        for e in v.iter_mut() {
            e.2 = if e.0 < 0 { 0 } else { ready(e.0 as i64, e.1) };
            if e.2 != 0 {
                count += 1;
            }
        }
        count > 0
    })?;
    for (i, e) in v.iter().enumerate() {
        let a = fds + 8 * i as u64 + 6;
        user::write_struct(a, &e.2)?;
    }
    Ok(count)
}

pub fn select(n: usize, rfds: u64, wfds: u64, efds: u64, tv: u64) -> KResult<u64> {
    let n = n.min(1024);
    let words = n.div_ceil(64);
    let load = |a: u64| -> KResult<Vec<u64>> { if a == 0 { Ok(alloc::vec![0; words]) } else { (0..words).map(|i| user::get_u64(a + 8 * i as u64)).collect() } };
    let (r, w, _e) = (load(rfds)?, load(wfds)?, load(efds)?);
    let timeout = if tv == 0 { -1 } else { (user::get_u64(tv)? * 1000 + user::get_u64(tv + 8)? / 1000) as i64 };
    let (mut ro, mut wo) = (alloc::vec![0u64; words], alloc::vec![0u64; words]);
    let mut count = 0;
    wait_any(timeout, || {
        count = 0;
        ro.fill(0);
        wo.fill(0);
        for fd in 0..n {
            let (wi, bit) = (fd / 64, 1u64 << (fd % 64));
            let want_r = r[wi] & bit != 0;
            let want_w = w[wi] & bit != 0;
            if !want_r && !want_w {
                continue;
            }
            let rd = ready(fd as i64, POLLIN | POLLOUT);
            if want_r && rd & (POLLIN | POLLHUP | POLLERR) != 0 {
                ro[wi] |= bit;
                count += 1;
            }
            if want_w && rd & (POLLOUT | POLLERR) != 0 {
                wo[wi] |= bit;
                count += 1;
            }
        }
        count > 0
    })?;
    let store = |a: u64, v: &[u64]| -> KResult<()> {
        if a != 0 {
            for (i, x) in v.iter().enumerate() {
                user::put_u64(a + 8 * i as u64, *x)?;
            }
        }
        Ok(())
    };
    store(rfds, &ro)?;
    store(wfds, &wo)?;
    store(efds, &alloc::vec![0; words])?;
    Ok(count)
}
