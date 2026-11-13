//! The virtual file system: inodes from several file systems (tmpfs, devfs, procfs, ext2, pipes,
//! sockets) behind one trait, open file descriptions, a mount table and path resolution.
pub mod devfs;
pub mod locks;
pub mod pipe;
pub mod ext2;
pub mod procfs;
pub mod tmpfs;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kcore::errno::*;

use crate::sync::SpinLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    CharDev,
    Fifo,
    Socket,
}

impl Kind {
    /// The S_IFMT bits.
    pub fn mode_bits(self) -> u32 {
        match self {
            Kind::File => 0o100000,
            Kind::Dir => 0o040000,
            Kind::Symlink => 0o120000,
            Kind::CharDev => 0o020000,
            Kind::Fifo => 0o010000,
            Kind::Socket => 0o140000,
        }
    }
    /// The d_type of getdents64.
    pub fn dtype(self) -> u8 {
        match self {
            Kind::File => 8,
            Kind::Dir => 4,
            Kind::Symlink => 10,
            Kind::CharDev => 2,
            Kind::Fifo => 1,
            Kind::Socket => 12,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32, // permission bits; the type comes from kind
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
}

pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: Kind,
}

pub const POLLIN: u16 = 1;
pub const POLLOUT: u16 = 4;
pub const POLLERR: u16 = 8;
pub const POLLHUP: u16 = 16;

pub type InodeRef = Arc<dyn Inode>;

pub trait Inode: Send + Sync + Any {
    fn kind(&self) -> Kind;
    fn stat(&self) -> Stat;
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write_at(&self, _off: u64, _buf: &[u8]) -> KResult<usize> {
        Err(EINVAL)
    }
    fn truncate(&self, _len: u64) -> KResult<()> {
        Err(EINVAL)
    }
    fn lookup(&self, _name: &str) -> KResult<InodeRef> {
        Err(ENOTDIR)
    }
    fn create(&self, _name: &str, _kind: Kind, _mode: u32) -> KResult<InodeRef> {
        Err(ENOTDIR)
    }
    fn link(&self, _name: &str, _target: &InodeRef) -> KResult<()> {
        Err(EPERM)
    }
    fn symlink(&self, _name: &str, _target: &str) -> KResult<InodeRef> {
        Err(EPERM)
    }
    fn unlink(&self, _name: &str) -> KResult<()> {
        Err(ENOTDIR)
    }
    fn rmdir(&self, _name: &str) -> KResult<()> {
        Err(ENOTDIR)
    }
    /// Moves `old` in this directory to `new` in `to` (a directory of the same file system).
    fn rename(&self, _old: &str, _to: &InodeRef, _new: &str) -> KResult<()> {
        Err(EXDEV)
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        Err(ENOTDIR)
    }
    fn readlink(&self) -> KResult<String> {
        Err(EINVAL)
    }
    fn chmod(&self, _mode: u32) -> KResult<()> {
        Ok(())
    }
    fn chown(&self, _uid: u32, _gid: u32) -> KResult<()> {
        Ok(())
    }
    fn set_times(&self, _atime: u64, _mtime: u64) -> KResult<()> {
        Ok(())
    }
    /// Readiness for poll: POLLIN / POLLOUT / POLLHUP / POLLERR.
    fn poll(&self) -> u16 {
        POLLIN | POLLOUT
    }
    /// The wait queue poll/select sleep on, for inodes whose readiness changes.
    fn wait_queue(&self) -> Option<&crate::sched::WaitQueue> {
        None
    }
    fn ioctl(&self, _cmd: u64, _arg: u64) -> KResult<u64> {
        Err(ENOTTY)
    }
    fn sync(&self) -> KResult<()> {
        Ok(())
    }
    /// Called when the last file description referring to this inode closes.
    fn release(&self) {}
    fn as_any(&self) -> &dyn Any;
}

static NEXT_INO: AtomicU64 = AtomicU64::new(1000);
pub fn new_ino() -> u64 {
    NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

pub fn now_secs() -> u64 {
    crate::time::realtime_ns() / 1_000_000_000
}

// ---------------------------------------------------------------- open files

pub const O_ACCMODE: u32 = 3;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;

/// An open file description (shared by dup'ed descriptors and across fork).
pub struct File {
    pub inode: InodeRef,
    /// The file position. A sleeping lock: reads and writes may wait (disk, pipes, sockets).
    pub offset: crate::sync::Mutex<u64>,
    pub flags: AtomicU32,
    pub path: String,
}

impl File {
    pub fn new(inode: InodeRef, flags: u32, path: String) -> Arc<File> {
        Arc::new(File { inode, offset: crate::sync::Mutex::new(0), flags: AtomicU32::new(flags & !O_CLOEXEC), path })
    }
    pub fn readable(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_WRONLY
    }
    pub fn writable(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_RDONLY
    }
    pub fn nonblocking(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0
    }

    pub fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if !self.readable() {
            return Err(EBADF);
        }
        let kind = self.inode.kind();
        if kind == Kind::Dir {
            return Err(EISDIR);
        }
        if !matches!(kind, Kind::File | Kind::Symlink) {
            // Streams (pipes, terminals, sockets, devices) have no position to protect.
            return self.inode.read_at(0, buf);
        }
        let mut off = self.offset.lock();
        let n = self.inode.read_at(*off, buf)?;
        *off += n as u64;
        Ok(n)
    }

    pub fn write(&self, buf: &[u8]) -> KResult<usize> {
        if !self.writable() {
            return Err(EBADF);
        }
        if self.inode.kind() != Kind::File {
            return self.inode.write_at(0, buf);
        }
        let mut off = self.offset.lock();
        if self.flags.load(Ordering::Relaxed) & O_APPEND != 0 {
            *off = self.inode.stat().size;
        }
        let n = self.inode.write_at(*off, buf)?;
        *off += n as u64;
        Ok(n)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        self.inode.release();
    }
}

// ---------------------------------------------------------------- mounts and paths

struct Mount {
    path: String,
    root: InodeRef,
    fs: &'static str,
}

static MOUNTS: SpinLock<Vec<Mount>> = SpinLock::new(Vec::new());

pub fn mount(path: &str, root: InodeRef, fs: &'static str) {
    let mut m = MOUNTS.lock();
    m.retain(|x| x.path != path);
    m.push(Mount { path: String::from(path), root, fs });
}

pub fn mounts() -> Vec<(String, &'static str)> {
    MOUNTS.lock().iter().map(|m| (m.path.clone(), m.fs)).collect()
}

fn mounted_at(path: &str) -> Option<InodeRef> {
    MOUNTS.lock().iter().find(|m| m.path == path).map(|m| m.root.clone())
}

pub fn root() -> InodeRef {
    mounted_at("/").expect("no root file system")
}

const MAX_SYMLINKS: usize = 40;

/// Resolves an absolute path to an inode. Symbolic links in the middle are always followed;
/// the last component only when `follow` is set. Returns the canonical path too.
pub fn resolve(abs: &str, follow: bool) -> KResult<(String, InodeRef)> {
    resolve_inner(abs, follow, 0)
}

fn resolve_inner(abs: &str, follow: bool, depth: usize) -> KResult<(String, InodeRef)> {
    if depth > MAX_SYMLINKS {
        return Err(ELOOP);
    }
    // Hot path (stat, open, exec): no allocation besides the resulting path string.
    let owned;
    let norm: &str = if kcore::path::is_normal(abs) {
        abs
    } else {
        owned = kcore::path::normalize("/", abs);
        &owned
    };
    let mut cur = root();
    let mut path = String::with_capacity(norm.len().max(1));
    let mut rest = &norm[1..];
    while !rest.is_empty() {
        let (c, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        if cur.kind() != Kind::Dir {
            return Err(ENOTDIR);
        }
        let next = cur.lookup(c)?;
        let parent_len = path.len();
        path.push('/');
        path.push_str(c);
        let kind = next.kind();
        if kind == Kind::Symlink && (!tail.is_empty() || follow) {
            let target = next.readlink()?;
            let base = if parent_len == 0 { "/" } else { &path[..parent_len] };
            let mut again = kcore::path::normalize(base, &target);
            if !tail.is_empty() {
                again.push('/');
                again.push_str(tail);
            }
            return resolve_inner(&again, follow, depth + 1);
        }
        // Only directories can be mount points.
        cur = if kind == Kind::Dir { mounted_at(&path).unwrap_or(next) } else { next };
        rest = tail;
    }
    if path.is_empty() {
        path.push('/');
    }
    Ok((path, cur))
}

/// The directory that would contain `abs`, and the last component.
pub fn resolve_parent(abs: &str) -> KResult<(InodeRef, String, String)> {
    let norm = kcore::path::normalize("/", abs);
    if norm == "/" {
        return Err(EEXIST);
    }
    let (dir, name) = kcore::path::split_last(&norm);
    let (dpath, d) = resolve(dir, true)?;
    if d.kind() != Kind::Dir {
        return Err(ENOTDIR);
    }
    if name.len() > 255 {
        return Err(ENAMETOOLONG);
    }
    Ok((d, String::from(name), dpath))
}

/// Reads a whole file (exec, the initrd-loaded configuration).
pub fn read_all(inode: &InodeRef) -> KResult<Vec<u8>> {
    let size = inode.stat().size as usize;
    let mut v = alloc::vec![0u8; size];
    let mut done = 0;
    while done < size {
        let n = inode.read_at(done as u64, &mut v[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    v.truncate(done);
    Ok(v)
}

/// Creates the directories along `path` that do not exist yet.
pub fn mkdir_p(path: &str) -> KResult<InodeRef> {
    let mut cur = root();
    for c in path.split('/').filter(|c| !c.is_empty()) {
        cur = match cur.lookup(c) {
            Ok(n) => n,
            Err(ENOENT) => cur.create(c, Kind::Dir, 0o755)?,
            Err(e) => return Err(e),
        };
    }
    Ok(cur)
}

/// Unpacks the initrd (a newc cpio archive) into the root file system.
pub fn unpack_initrd(data: &[u8]) -> KResult<usize> {
    let mut n = 0;
    for e in kcore::cpio::Reader::new(data) {
        let e = e.map_err(|_| EINVAL)?;
        if e.name.is_empty() || e.name == "." {
            continue;
        }
        let abs = alloc::format!("/{}", e.name);
        let (parent_path, name) = kcore::path::split_last(&abs);
        let parent = mkdir_p(parent_path)?;
        let perm = e.mode & 0o7777;
        match e.mode & kcore::cpio::S_IFMT {
            kcore::cpio::S_IFDIR => {
                if parent.lookup(name).is_err() {
                    parent.create(name, Kind::Dir, perm)?;
                }
            }
            kcore::cpio::S_IFLNK => {
                parent.symlink(name, core::str::from_utf8(e.data).map_err(|_| EINVAL)?)?;
            }
            _ => {
                let f = match parent.lookup(name) {
                    Ok(f) => f,
                    Err(_) => parent.create(name, Kind::File, perm)?,
                };
                f.truncate(0)?;
                f.write_at(0, e.data)?;
                f.chmod(perm)?;
            }
        }
        n += 1;
    }
    Ok(n)
}
