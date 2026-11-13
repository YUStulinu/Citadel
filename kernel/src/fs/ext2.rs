//! ext2 file systems on block devices: the VFS side of kcore::ext2.
//!
//! The on-disk logic lives in kcore (tested against mke2fs/e2fsck on the host); here are the
//! block cache in front of the disk, the inode objects the VFS works with, and the rule that an
//! unlinked file keeps its data until the last open file closes.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kcore::errno::*;
use kcore::ext2::{self as e2, Ext2};

use super::*;
use crate::drivers::blk::BlockDevice;
use crate::sync::{Mutex, SpinLock};

const PAGE: u64 = 4096;
const CACHE_PAGES: usize = 4096; // 16 MiB per file system

/// A write-through cache of 4 KiB pages in front of a block device.
pub struct CachedDisk {
    dev: Arc<dyn BlockDevice>,
    pages: SpinLock<BTreeMap<u64, (Arc<SpinLock<Vec<u8>>>, u64)>>,
    tick: AtomicU64,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl CachedDisk {
    fn new(dev: Arc<dyn BlockDevice>) -> Self {
        CachedDisk { dev, pages: SpinLock::new(BTreeMap::new()), tick: AtomicU64::new(0), hits: AtomicU64::new(0), misses: AtomicU64::new(0) }
    }

    /// The cached page with index `p`, read from the device on a miss.
    fn page(&self, p: u64) -> KResult<Arc<SpinLock<Vec<u8>>>> {
        let t = self.tick.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.pages.lock().get_mut(&p) {
            e.1 = t;
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(e.0.clone());
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        // Read outside the lock (the request sleeps).
        let len = PAGE.min(self.dev.size() - p * PAGE) as usize;
        let mut buf = alloc::vec![0u8; PAGE as usize];
        self.dev.read_at(p * PAGE, &mut buf[..len])?;
        let page = Arc::new(SpinLock::new(buf));
        let mut map = self.pages.lock();
        if let Some(e) = map.get(&p) {
            return Ok(e.0.clone()); // someone else read it meanwhile
        }
        if map.len() >= CACHE_PAGES {
            // Evict the least recently used eighth.
            let mut ages: Vec<(u64, u64)> = map.iter().map(|(k, v)| (v.1, *k)).collect();
            ages.sort_unstable();
            for (_, k) in ages.iter().take(CACHE_PAGES / 8) {
                map.remove(k);
            }
        }
        map.insert(p, (page.clone(), t));
        Ok(page)
    }
}

impl e2::Disk for CachedDisk {
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let mut done = 0;
        while done < buf.len() {
            let pos = off + done as u64;
            let within = (pos % PAGE) as usize;
            let n = (PAGE as usize - within).min(buf.len() - done);
            let page = self.page(pos / PAGE)?;
            buf[done..done + n].copy_from_slice(&page.lock()[within..within + n]);
            done += n;
        }
        Ok(())
    }

    fn write(&self, off: u64, buf: &[u8]) -> KResult<()> {
        let mut done = 0;
        while done < buf.len() {
            let pos = off + done as u64;
            let within = (pos % PAGE) as usize;
            let n = (PAGE as usize - within).min(buf.len() - done);
            let p = pos / PAGE;
            let len = PAGE.min(self.dev.size() - p * PAGE) as usize;
            if within == 0 && n == len {
                // A whole page: no need to read it first.
                self.dev.write_at(pos, &buf[done..done + n])?;
                let page = Arc::new(SpinLock::new(buf[done..done + n].to_vec()));
                page.lock().resize(PAGE as usize, 0);
                let t = self.tick.fetch_add(1, Ordering::Relaxed);
                self.pages.lock().insert(p, (page, t));
            } else {
                let page = self.page(p)?;
                let copy = {
                    let mut g = page.lock();
                    g[within..within + n].copy_from_slice(&buf[done..done + n]);
                    g[..len].to_vec()
                };
                self.dev.write_at(p * PAGE, &copy)?;
            }
            done += n;
        }
        Ok(())
    }
}

pub struct Ext2Fs {
    fs: Mutex<Ext2<CachedDisk>>,
    inodes: SpinLock<BTreeMap<u32, Weak<Ext2Inode>>>,
    dev_id: u64,
    pub name: String,
}

pub struct Ext2Inode {
    fs: Arc<Ext2Fs>,
    ino: u32,
    /// Unlinked while still in use: freed when the last reference goes away.
    orphan: AtomicBool,
}

fn now32() -> u32 {
    now_secs() as u32
}

impl Ext2Fs {
    /// Mounts the ext2 file system on `dev`; returns its root inode.
    pub fn mount(dev: Arc<dyn BlockDevice>, name: &str, read_only: bool) -> KResult<InodeRef> {
        let mut fs = Ext2::mount(CachedDisk::new(dev), read_only)?;
        fs.clock = now32;
        static NEXT_DEV: AtomicU64 = AtomicU64::new(0x800);
        let me = Arc::new(Ext2Fs { fs: Mutex::new(fs), inodes: SpinLock::new(BTreeMap::new()), dev_id: NEXT_DEV.fetch_add(1, Ordering::Relaxed), name: String::from(name) });
        {
            let f = me.fs.lock();
            crate::kprintln!(
                "ext2: {name}: {} KiB blocks, {}/{} blocks free, {}/{} inodes free{}",
                f.block_size / 1024,
                f.free_blocks(),
                f.total_blocks(),
                f.free_inodes(),
                f.total_inodes(),
                if f.read_only() { ", read-only" } else { "" }
            );
        }
        MOUNTED.lock().push(me.clone());
        Ok(me.inode(e2::ROOT_INO))
    }

    fn inode(self: &Arc<Self>, ino: u32) -> InodeRef {
        let mut map = self.inodes.lock();
        if let Some(i) = map.get(&ino).and_then(|w| w.upgrade()) {
            return i;
        }
        let i = Arc::new(Ext2Inode { fs: self.clone(), ino, orphan: AtomicBool::new(false) });
        map.insert(ino, Arc::downgrade(&i));
        i
    }

    /// An inode that lost its last name: free it now, or when its last user lets go.
    fn orphaned(self: &Arc<Self>, ino: u32) -> KResult<()> {
        let live = self.inodes.lock().get(&ino).and_then(|w| w.upgrade());
        match live {
            Some(i) => {
                i.orphan.store(true, Ordering::Release);
                Ok(())
            }
            None => self.fs.lock().release(ino),
        }
    }

    pub fn sync(&self) -> KResult<()> {
        let mut f = self.fs.lock();
        f.sync()?;
        f.dev.dev.flush()
    }
}

static MOUNTED: SpinLock<Vec<Arc<Ext2Fs>>> = SpinLock::new(Vec::new());

/// Writes back every mounted ext2 file system (sync(2), power-off).
pub fn sync_all() {
    let list: Vec<Arc<Ext2Fs>> = MOUNTED.lock().clone();
    for fs in list {
        if let Err(e) = fs.sync() {
            crate::kprintln!("ext2: {}: sync failed (errno {e})", fs.name);
        }
    }
}

impl Drop for Ext2Inode {
    fn drop(&mut self) {
        {
            let mut map = self.fs.inodes.lock();
            if map.get(&self.ino).is_some_and(|w| w.strong_count() == 0) {
                map.remove(&self.ino);
            }
        }
        if self.orphan.load(Ordering::Acquire) {
            let _ = self.fs.fs.lock().release(self.ino);
        }
    }
}

fn kind_of(mode: u16) -> Kind {
    match mode & e2::S_IFMT {
        e2::S_IFDIR => Kind::Dir,
        e2::S_IFLNK => Kind::Symlink,
        e2::S_IFCHR | e2::S_IFBLK => Kind::CharDev,
        e2::S_IFIFO => Kind::Fifo,
        e2::S_IFSOCK => Kind::Socket,
        _ => Kind::File,
    }
}

impl Ext2Inode {
    fn other<'a>(&self, i: &'a InodeRef) -> KResult<&'a Ext2Inode> {
        let o = i.as_any().downcast_ref::<Ext2Inode>().ok_or(EXDEV)?;
        if !Arc::ptr_eq(&o.fs, &self.fs) {
            return Err(EXDEV);
        }
        Ok(o)
    }
}

impl Inode for Ext2Inode {
    fn kind(&self) -> Kind {
        self.fs.fs.lock().read_inode(self.ino).map(|n| kind_of(n.mode())).unwrap_or(Kind::File)
    }

    fn stat(&self) -> Stat {
        let f = self.fs.fs.lock();
        match f.read_inode(self.ino) {
            Ok(n) => Stat {
                dev: self.fs.dev_id,
                ino: self.ino as u64,
                mode: (n.mode() & 0o7777) as u32,
                nlink: n.links() as u32,
                uid: n.uid() as u32,
                gid: n.gid() as u32,
                rdev: if matches!(n.kind(), e2::S_IFCHR | e2::S_IFBLK) { n.rdev() as u64 } else { 0 },
                size: n.size(),
                blocks: n.blocks512() as u64,
                atime: n.atime() as u64,
                mtime: n.mtime() as u64,
                ctime: n.ctime() as u64,
            },
            Err(_) => Stat { dev: self.fs.dev_id, ino: self.ino as u64, ..Default::default() },
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        // Large reads go through a kernel buffer in pieces (the cache works in pages anyway).
        self.fs.fs.lock().read(self.ino, off, buf)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        self.fs.fs.lock().write(self.ino, off, buf)
    }

    fn truncate(&self, len: u64) -> KResult<()> {
        self.fs.fs.lock().truncate(self.ino, len)
    }

    fn lookup(&self, name: &str) -> KResult<InodeRef> {
        let ino = self.fs.fs.lock().lookup(self.ino, name)?;
        Ok(self.fs.inode(ino))
    }

    fn create(&self, name: &str, kind: Kind, mode: u32) -> KResult<InodeRef> {
        let perm = (mode & 0o7777) as u16;
        let ino = {
            let mut f = self.fs.fs.lock();
            match kind {
                Kind::Dir => f.mkdir(self.ino, name, perm)?,
                Kind::File => f.create(self.ino, name, e2::S_IFREG | perm, 0)?,
                Kind::Fifo => f.create(self.ino, name, e2::S_IFIFO | perm, 0)?,
                Kind::Socket => f.create(self.ino, name, e2::S_IFSOCK | perm, 0)?,
                _ => return Err(EPERM),
            }
        };
        Ok(self.fs.inode(ino))
    }

    fn link(&self, name: &str, target: &InodeRef) -> KResult<()> {
        let t = self.other(target)?;
        self.fs.fs.lock().link(self.ino, name, t.ino)
    }

    fn symlink(&self, name: &str, target: &str) -> KResult<InodeRef> {
        let ino = self.fs.fs.lock().symlink(self.ino, name, target)?;
        Ok(self.fs.inode(ino))
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        let (ino, links) = self.fs.fs.lock().unlink(self.ino, name)?;
        if links == 0 {
            self.fs.orphaned(ino)?;
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> KResult<()> {
        self.fs.fs.lock().rmdir(self.ino, name).map(|_| ())
    }

    fn rename(&self, old: &str, to: &InodeRef, new: &str) -> KResult<()> {
        let t = self.other(to)?;
        let orphan = self.fs.fs.lock().rename(self.ino, old, t.ino, new)?;
        if let Some(ino) = orphan {
            self.fs.orphaned(ino)?;
        }
        Ok(())
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let entries = self.fs.fs.lock().readdir(self.ino)?;
        Ok(entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| DirEntry {
                kind: match e.file_type {
                    2 => Kind::Dir,
                    7 => Kind::Symlink,
                    3 | 4 => Kind::CharDev,
                    5 => Kind::Fifo,
                    6 => Kind::Socket,
                    _ => Kind::File,
                },
                ino: e.ino as u64,
                name: e.name,
            })
            .collect())
    }

    fn readlink(&self) -> KResult<String> {
        self.fs.fs.lock().readlink(self.ino)
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        self.fs.fs.lock().chmod(self.ino, mode as u16)
    }

    fn chown(&self, uid: u32, gid: u32) -> KResult<()> {
        self.fs.fs.lock().chown(self.ino, uid as u16, gid as u16)
    }

    fn set_times(&self, atime: u64, mtime: u64) -> KResult<()> {
        self.fs.fs.lock().utimes(self.ino, atime as u32, mtime as u32)
    }

    fn sync(&self) -> KResult<()> {
        self.fs.sync()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Mounts the first ext2 partition (GPT "Linux filesystem" type, or a whole disk) at `path`.
pub fn mount_first(path: &str) -> Option<String> {
    use crate::drivers::blk;
    let disks = blk::disks();
    let candidates = disks.iter().filter(|d| d.type_guid == blk::LINUX_FS_GUID).chain(disks.iter().filter(|d| d.type_guid == [0; 16]));
    for d in candidates {
        let mut magic = alloc::vec![0u8; 512];
        if d.dev.read_at(1024, &mut magic).is_err() || u16::from_le_bytes([magic[56], magic[57]]) != 0xef53 {
            continue;
        }
        match Ext2Fs::mount(d.dev.clone(), &d.name, false) {
            Ok(root) => {
                let _ = mkdir_p(path);
                mount(path, root, "ext2");
                return Some(d.name.clone());
            }
            Err(e) => crate::kprintln!("ext2: {}: cannot mount (errno {e})", d.name),
        }
    }
    None
}
