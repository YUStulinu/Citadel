//! tmpfs: files and directories in kernel memory (the root file system unpacked from the initrd,
//! and /tmp).
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use kcore::errno::*;

use super::*;
use crate::sync::SpinLock;

enum Data {
    File(Vec<u8>),
    Dir(BTreeMap<String, InodeRef>),
    Symlink(String),
}

struct Meta {
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    atime: u64,
    mtime: u64,
    ctime: u64,
}

pub struct TmpInode {
    ino: u64,
    kind: Kind,
    data: SpinLock<Data>,
    meta: SpinLock<Meta>,
}

pub const TMPFS_DEV: u64 = 0x10;

impl TmpInode {
    fn new(kind: Kind, mode: u32) -> Arc<TmpInode> {
        let now = now_secs();
        let data = match kind {
            Kind::Dir => Data::Dir(BTreeMap::new()),
            Kind::Symlink => Data::Symlink(String::new()),
            _ => Data::File(Vec::new()),
        };
        Arc::new(TmpInode {
            ino: new_ino(),
            kind,
            data: SpinLock::new(data),
            meta: SpinLock::new(Meta { mode: mode & 0o7777, uid: 0, gid: 0, nlink: if kind == Kind::Dir { 2 } else { 1 }, atime: now, mtime: now, ctime: now }),
        })
    }

    pub fn new_root() -> InodeRef {
        TmpInode::new(Kind::Dir, 0o755)
    }

    fn touch(&self) {
        let mut m = self.meta.lock();
        let now = now_secs();
        m.mtime = now;
        m.ctime = now;
    }
}

impl Inode for TmpInode {
    fn kind(&self) -> Kind {
        self.kind
    }

    fn stat(&self) -> Stat {
        let size = match &*self.data.lock() {
            Data::File(v) => v.len() as u64,
            Data::Dir(m) => (m.len() as u64 + 2) * 32,
            Data::Symlink(s) => s.len() as u64,
        };
        let m = self.meta.lock();
        Stat { dev: TMPFS_DEV, ino: self.ino, mode: m.mode, nlink: m.nlink, uid: m.uid, gid: m.gid, size, blocks: size.div_ceil(512), atime: m.atime, mtime: m.mtime, ctime: m.ctime, rdev: 0 }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        match &*self.data.lock() {
            Data::File(v) => {
                let off = off as usize;
                if off >= v.len() {
                    return Ok(0);
                }
                let n = buf.len().min(v.len() - off);
                buf[..n].copy_from_slice(&v[off..off + n]);
                Ok(n)
            }
            Data::Dir(_) => Err(EISDIR),
            Data::Symlink(_) => Err(EINVAL),
        }
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        let r = match &mut *self.data.lock() {
            Data::File(v) => {
                let off = off as usize;
                let end = off.checked_add(buf.len()).ok_or(EFBIG)?;
                if end > 1 << 31 {
                    return Err(EFBIG);
                }
                if v.len() < end {
                    v.try_reserve(end - v.len()).map_err(|_| ENOSPC)?;
                    v.resize(end, 0);
                }
                v[off..end].copy_from_slice(buf);
                Ok(buf.len())
            }
            Data::Dir(_) => Err(EISDIR),
            Data::Symlink(_) => Err(EINVAL),
        };
        self.touch();
        r
    }

    fn truncate(&self, len: u64) -> KResult<()> {
        match &mut *self.data.lock() {
            Data::File(v) => v.resize(len as usize, 0),
            Data::Dir(_) => return Err(EISDIR),
            Data::Symlink(_) => return Err(EINVAL),
        }
        self.touch();
        Ok(())
    }

    fn lookup(&self, name: &str) -> KResult<InodeRef> {
        match &*self.data.lock() {
            Data::Dir(m) => m.get(name).cloned().ok_or(ENOENT),
            _ => Err(ENOTDIR),
        }
    }

    fn create(&self, name: &str, kind: Kind, mode: u32) -> KResult<InodeRef> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(EINVAL);
        }
        let n: InodeRef = match kind {
            Kind::Fifo => super::pipe::Pipe::new_fifo(),
            _ => TmpInode::new(kind, mode),
        };
        match &mut *self.data.lock() {
            Data::Dir(m) => {
                if m.contains_key(name) {
                    return Err(EEXIST);
                }
                m.insert(String::from(name), n.clone());
            }
            _ => return Err(ENOTDIR),
        }
        if kind == Kind::Dir {
            self.meta.lock().nlink += 1;
        }
        self.touch();
        Ok(n)
    }

    fn link(&self, name: &str, target: &InodeRef) -> KResult<()> {
        let t = target.as_any().downcast_ref::<TmpInode>().ok_or(EXDEV)?;
        if t.kind == Kind::Dir {
            return Err(EPERM);
        }
        match &mut *self.data.lock() {
            Data::Dir(m) => {
                if m.contains_key(name) {
                    return Err(EEXIST);
                }
                m.insert(String::from(name), target.clone());
            }
            _ => return Err(ENOTDIR),
        }
        t.meta.lock().nlink += 1;
        Ok(())
    }

    fn symlink(&self, name: &str, target: &str) -> KResult<InodeRef> {
        let n = TmpInode::new(Kind::Symlink, 0o777);
        *n.data.lock() = Data::Symlink(String::from(target));
        match &mut *self.data.lock() {
            Data::Dir(m) => {
                if m.contains_key(name) {
                    return Err(EEXIST);
                }
                m.insert(String::from(name), n.clone());
            }
            _ => return Err(ENOTDIR),
        }
        Ok(n)
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        let removed = match &mut *self.data.lock() {
            Data::Dir(m) => {
                let n = m.get(name).ok_or(ENOENT)?;
                if n.kind() == Kind::Dir {
                    return Err(EISDIR);
                }
                m.remove(name).unwrap()
            }
            _ => return Err(ENOTDIR),
        };
        if let Some(t) = removed.as_any().downcast_ref::<TmpInode>() {
            t.meta.lock().nlink -= 1;
        }
        self.touch();
        Ok(())
    }

    fn rmdir(&self, name: &str) -> KResult<()> {
        match &mut *self.data.lock() {
            Data::Dir(m) => {
                let n = m.get(name).ok_or(ENOENT)?;
                if n.kind() != Kind::Dir {
                    return Err(ENOTDIR);
                }
                if !n.readdir()?.is_empty() {
                    return Err(ENOTEMPTY);
                }
                m.remove(name);
            }
            _ => return Err(ENOTDIR),
        }
        self.meta.lock().nlink -= 1;
        self.touch();
        Ok(())
    }

    fn rename(&self, old: &str, to: &InodeRef, new: &str) -> KResult<()> {
        let dst = to.as_any().downcast_ref::<TmpInode>().ok_or(EXDEV)?;
        let node = self.lookup(old)?;
        if let Ok(existing) = dst.lookup(new) {
            if Arc::ptr_eq(&existing, &node) {
                return Ok(());
            }
            match (node.kind(), existing.kind()) {
                (Kind::Dir, Kind::Dir) => {
                    if !existing.readdir()?.is_empty() {
                        return Err(ENOTEMPTY);
                    }
                }
                (Kind::Dir, _) => return Err(ENOTDIR),
                (_, Kind::Dir) => return Err(EISDIR),
                _ => {}
            }
        }
        if let Data::Dir(m) = &mut *self.data.lock() {
            m.remove(old);
        }
        if let Data::Dir(m) = &mut *dst.data.lock() {
            m.insert(String::from(new), node);
        }
        self.touch();
        dst.touch();
        Ok(())
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        match &*self.data.lock() {
            Data::Dir(m) => Ok(m.iter().map(|(k, v)| DirEntry { name: k.clone(), ino: v.stat().ino, kind: v.kind() }).collect()),
            _ => Err(ENOTDIR),
        }
    }

    fn readlink(&self) -> KResult<String> {
        match &*self.data.lock() {
            Data::Symlink(s) => Ok(s.clone()),
            _ => Err(EINVAL),
        }
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        let mut m = self.meta.lock();
        m.mode = mode & 0o7777;
        m.ctime = now_secs();
        Ok(())
    }

    fn chown(&self, uid: u32, gid: u32) -> KResult<()> {
        let mut m = self.meta.lock();
        if uid != u32::MAX {
            m.uid = uid;
        }
        if gid != u32::MAX {
            m.gid = gid;
        }
        Ok(())
    }

    fn set_times(&self, atime: u64, mtime: u64) -> KResult<()> {
        let mut m = self.meta.lock();
        m.atime = atime;
        m.mtime = mtime;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
