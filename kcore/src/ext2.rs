//! ext2: reading and writing the second extended file system.
//!
//! Supported: revision 0 and 1, block sizes 1-64 KiB, the FILETYPE, SPARSE_SUPER and
//! LARGE_FILE features, direct/indirect/double/triple-indirect block maps, fast and slow
//! symbolic links. Directories with an htree index (DIR_INDEX) are read linearly, which the
//! format allows; a directory that is modified loses its index flag so that it stays valid.
//! File systems needing anything else (extents, journals with recovery, 64-bit) are refused.
//!
//! The superblock's free counts are kept in memory and written by [`Ext2::sync`]; bitmaps,
//! group descriptors, inodes and data are written as they change.
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::errno::*;

/// Byte-addressed storage holding the file system.
pub trait Disk {
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()>;
    fn write(&self, off: u64, buf: &[u8]) -> KResult<()>;
}

pub const ROOT_INO: u32 = 2;
const MAGIC: u16 = 0xef53;

const INCOMPAT_FILETYPE: u32 = 0x2;
const RO_COMPAT_SPARSE_SUPER: u32 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;
const INDEX_FL: u32 = 0x1000;

pub const S_IFMT: u16 = 0o170000;
pub const S_IFSOCK: u16 = 0o140000;
pub const S_IFLNK: u16 = 0o120000;
pub const S_IFREG: u16 = 0o100000;
pub const S_IFBLK: u16 = 0o060000;
pub const S_IFDIR: u16 = 0o040000;
pub const S_IFCHR: u16 = 0o020000;
pub const S_IFIFO: u16 = 0o010000;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// An inode as stored on disk (the first 128 bytes; larger on-disk inodes keep their tail).
#[derive(Clone)]
pub struct DInode {
    pub raw: [u8; 128],
}

impl DInode {
    pub fn mode(&self) -> u16 {
        le16(&self.raw, 0)
    }
    pub fn set_mode(&mut self, v: u16) {
        put16(&mut self.raw, 0, v)
    }
    pub fn uid(&self) -> u16 {
        le16(&self.raw, 2)
    }
    pub fn gid(&self) -> u16 {
        le16(&self.raw, 24)
    }
    pub fn set_owner(&mut self, uid: u16, gid: u16) {
        put16(&mut self.raw, 2, uid);
        put16(&mut self.raw, 24, gid);
    }
    pub fn kind(&self) -> u16 {
        self.mode() & S_IFMT
    }
    pub fn is_dir(&self) -> bool {
        self.kind() == S_IFDIR
    }
    pub fn size(&self) -> u64 {
        let lo = le32(&self.raw, 4) as u64;
        if self.kind() == S_IFREG { lo | (le32(&self.raw, 108) as u64) << 32 } else { lo }
    }
    fn set_size(&mut self, v: u64) {
        put32(&mut self.raw, 4, v as u32);
        if self.kind() == S_IFREG {
            put32(&mut self.raw, 108, (v >> 32) as u32);
        }
    }
    pub fn atime(&self) -> u32 {
        le32(&self.raw, 8)
    }
    pub fn ctime(&self) -> u32 {
        le32(&self.raw, 12)
    }
    pub fn mtime(&self) -> u32 {
        le32(&self.raw, 16)
    }
    pub fn set_times(&mut self, atime: u32, mtime: u32, ctime: u32) {
        put32(&mut self.raw, 8, atime);
        put32(&mut self.raw, 16, mtime);
        put32(&mut self.raw, 12, ctime);
    }
    fn set_dtime(&mut self, v: u32) {
        put32(&mut self.raw, 20, v)
    }
    pub fn links(&self) -> u16 {
        le16(&self.raw, 26)
    }
    fn set_links(&mut self, v: u16) {
        put16(&mut self.raw, 26, v)
    }
    /// Allocated space in 512-byte units.
    pub fn blocks512(&self) -> u32 {
        le32(&self.raw, 28)
    }
    fn set_blocks512(&mut self, v: u32) {
        put32(&mut self.raw, 28, v)
    }
    fn flags(&self) -> u32 {
        le32(&self.raw, 32)
    }
    fn set_flags(&mut self, v: u32) {
        put32(&mut self.raw, 32, v)
    }
    fn block(&self, i: usize) -> u32 {
        le32(&self.raw, 40 + 4 * i)
    }
    fn set_block(&mut self, i: usize, v: u32) {
        put32(&mut self.raw, 40 + 4 * i, v)
    }
    /// Device number of a character or block special file.
    pub fn rdev(&self) -> u32 {
        self.block(0)
    }
    /// A symbolic link whose target lives in the block-pointer area.
    fn is_fast_symlink(&self) -> bool {
        self.kind() == S_IFLNK && self.blocks512() == 0 && self.size() < 60
    }
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub ino: u32,
    /// The entry's type (DT_* values: 1 file, 2 dir, 7 symlink...); 0 if not recorded.
    pub file_type: u8,
}

fn file_type_of(mode: u16) -> u8 {
    match mode & S_IFMT {
        S_IFREG => 1,
        S_IFDIR => 2,
        S_IFCHR => 3,
        S_IFBLK => 4,
        S_IFIFO => 5,
        S_IFSOCK => 6,
        S_IFLNK => 7,
        _ => 0,
    }
}

pub struct Ext2<D: Disk> {
    pub dev: D,
    pub block_size: u32,
    sb: Vec<u8>,
    groups: Vec<[u8; 32]>,
    inodes_per_group: u32,
    blocks_per_group: u32,
    inode_size: u32,
    first_data_block: u32,
    blocks_count: u32,
    inodes_count: u32,
    first_ino: u32,
    filetype: bool,
    large_file: bool,
    sb_dirty: bool,
    read_only: bool,
    /// Seconds since 1970, for timestamps.
    pub clock: fn() -> u32,
}

fn zero_clock() -> u32 {
    0
}

impl<D: Disk> Ext2<D> {
    /// Reads the superblock and group descriptors. `read_only` refuses all changes.
    pub fn mount(dev: D, read_only: bool) -> KResult<Self> {
        let mut sb = vec![0u8; 1024];
        dev.read(1024, &mut sb)?;
        if le16(&sb, 56) != MAGIC {
            return Err(EINVAL);
        }
        let log = le32(&sb, 24);
        if log > 6 {
            return Err(EINVAL);
        }
        let block_size = 1024u32 << log;
        let rev = le32(&sb, 76);
        let (inode_size, first_ino, incompat, ro_compat) = if rev >= 1 { (le16(&sb, 88) as u32, le32(&sb, 84), le32(&sb, 96), le32(&sb, 100)) } else { (128, 11, 0, 0) };
        if incompat & !INCOMPAT_FILETYPE != 0 {
            return Err(EINVAL); // extents, journal recovery, 64-bit, ...: not ext2
        }
        let ro_unknown = ro_compat & !(RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE) != 0;
        let read_only = read_only || ro_unknown;
        let blocks_per_group = le32(&sb, 32);
        let inodes_per_group = le32(&sb, 40);
        let blocks_count = le32(&sb, 4);
        let first_data_block = le32(&sb, 20);
        if blocks_per_group == 0 || inodes_per_group == 0 || inode_size < 128 || !inode_size.is_power_of_two() {
            return Err(EINVAL);
        }
        let ngroups = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;
        let gdt_bytes = (ngroups * 32).div_ceil(block_size as usize) * block_size as usize;
        let mut gdt = vec![0u8; gdt_bytes];
        dev.read((first_data_block as u64 + 1) * block_size as u64, &mut gdt)?;
        let groups = (0..ngroups).map(|i| gdt[i * 32..i * 32 + 32].try_into().unwrap()).collect();
        Ok(Ext2 {
            dev,
            block_size,
            groups,
            inodes_per_group,
            blocks_per_group,
            inode_size,
            first_data_block,
            blocks_count,
            inodes_count: le32(&sb, 0),
            first_ino,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            large_file: ro_compat & RO_COMPAT_LARGE_FILE != 0,
            sb,
            sb_dirty: false,
            read_only,
            clock: zero_clock,
        })
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn writable(&self) -> KResult<()> {
        if self.read_only { Err(EROFS) } else { Ok(()) }
    }

    pub fn free_blocks(&self) -> u32 {
        le32(&self.sb, 12)
    }
    pub fn free_inodes(&self) -> u32 {
        le32(&self.sb, 16)
    }
    pub fn total_blocks(&self) -> u32 {
        self.blocks_count
    }
    pub fn total_inodes(&self) -> u32 {
        self.inodes_count
    }

    // ------------------------------------------------------------------ blocks

    fn bs(&self) -> u64 {
        self.block_size as u64
    }

    pub fn read_block(&self, b: u32, buf: &mut [u8]) -> KResult<()> {
        if b >= self.blocks_count {
            return Err(EIO);
        }
        self.dev.read(b as u64 * self.bs(), &mut buf[..self.block_size as usize])
    }

    fn write_block(&self, b: u32, buf: &[u8]) -> KResult<()> {
        if b >= self.blocks_count || b == 0 {
            return Err(EIO);
        }
        self.dev.write(b as u64 * self.bs(), &buf[..self.block_size as usize])
    }

    fn write_group(&self, g: usize) -> KResult<()> {
        // The descriptor's 32 bytes, within the table's block (sector-aligned write of a block).
        let per_block = self.block_size as usize / 32;
        let blk = self.first_data_block + 1 + (g / per_block) as u32;
        let mut buf = vec![0u8; self.block_size as usize];
        let start = (g / per_block) * per_block;
        for i in 0..per_block {
            if start + i < self.groups.len() {
                buf[i * 32..i * 32 + 32].copy_from_slice(&self.groups[start + i]);
            }
        }
        self.write_block(blk, &buf)
    }

    /// Writes the superblock (free counts, state) if it changed.
    pub fn sync(&mut self) -> KResult<()> {
        if self.sb_dirty && !self.read_only {
            let now = (self.clock)();
            put32(&mut self.sb, 48, now); // s_wtime
            self.dev.write(1024, &self.sb)?;
            self.sb_dirty = false;
        }
        Ok(())
    }

    fn adjust_counts(&mut self, g: usize, blocks: i32, inodes: i32, dirs: i32) {
        let gd = &mut self.groups[g];
        let (fb, fi, ud) = (le16(gd, 12) as i32 + blocks, le16(gd, 14) as i32 + inodes, le16(gd, 16) as i32 + dirs);
        put16(gd, 12, fb as u16);
        put16(gd, 14, fi as u16);
        put16(gd, 16, ud as u16);
        let fb = le32(&self.sb, 12) as i64 + blocks as i64;
        let fi = le32(&self.sb, 16) as i64 + inodes as i64;
        put32(&mut self.sb, 12, fb as u32);
        put32(&mut self.sb, 16, fi as u32);
        self.sb_dirty = true;
    }

    /// Finds a clear bit in a bitmap block, sets it and returns its index.
    fn take_bit(&self, bitmap_block: u32, limit: u32) -> KResult<Option<u32>> {
        let mut bm = vec![0u8; self.block_size as usize];
        self.read_block(bitmap_block, &mut bm)?;
        for (i, byte) in bm.iter_mut().enumerate() {
            if *byte == 0xff {
                continue;
            }
            for bit in 0..8 {
                let n = i as u32 * 8 + bit;
                if n >= limit {
                    return Ok(None);
                }
                if *byte & (1 << bit) == 0 {
                    *byte |= 1 << bit;
                    self.write_block(bitmap_block, &bm)?;
                    return Ok(Some(n));
                }
            }
        }
        Ok(None)
    }

    fn clear_bit(&self, bitmap_block: u32, n: u32) -> KResult<()> {
        let mut bm = vec![0u8; self.block_size as usize];
        self.read_block(bitmap_block, &mut bm)?;
        let (i, bit) = ((n / 8) as usize, n % 8);
        if bm[i] & (1 << bit) == 0 {
            return Err(EIO); // freeing something that is not allocated: corruption
        }
        bm[i] &= !(1 << bit);
        self.write_block(bitmap_block, &bm)
    }

    fn blocks_in_group(&self, g: usize) -> u32 {
        let start = self.first_data_block + g as u32 * self.blocks_per_group;
        (self.blocks_count - start).min(self.blocks_per_group)
    }

    /// Allocates a block, preferring group `goal`. The block's contents are not cleared.
    fn alloc_block(&mut self, goal: usize) -> KResult<u32> {
        let n = self.groups.len();
        for k in 0..n {
            let g = (goal + k) % n;
            if le16(&self.groups[g], 12) == 0 {
                continue;
            }
            let bitmap = le32(&self.groups[g], 0);
            if let Some(bit) = self.take_bit(bitmap, self.blocks_in_group(g))? {
                self.adjust_counts(g, -1, 0, 0);
                self.write_group(g)?;
                return Ok(self.first_data_block + g as u32 * self.blocks_per_group + bit);
            }
        }
        Err(ENOSPC)
    }

    fn free_block(&mut self, b: u32) -> KResult<()> {
        if b < self.first_data_block || b >= self.blocks_count {
            return Err(EIO);
        }
        let rel = b - self.first_data_block;
        let g = (rel / self.blocks_per_group) as usize;
        let bitmap = le32(&self.groups[g], 0);
        self.clear_bit(bitmap, rel % self.blocks_per_group)?;
        self.adjust_counts(g, 1, 0, 0);
        self.write_group(g)
    }

    fn zero_block(&self, b: u32) -> KResult<()> {
        self.write_block(b, &vec![0u8; self.block_size as usize])
    }

    // ------------------------------------------------------------------ inodes

    fn inode_location(&self, ino: u32) -> KResult<u64> {
        if ino == 0 || ino > self.inodes_count {
            return Err(EIO);
        }
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let idx = (ino - 1) % self.inodes_per_group;
        let table = le32(&self.groups[g], 8) as u64;
        Ok(table * self.bs() + idx as u64 * self.inode_size as u64)
    }

    pub fn read_inode(&self, ino: u32) -> KResult<DInode> {
        let at = self.inode_location(ino)?;
        // Read the sector holding the inode (devices work in 512-byte units).
        let sector = at & !511;
        let mut buf = vec![0u8; 512];
        self.dev.read(sector, &mut buf)?;
        let o = (at - sector) as usize;
        let mut raw = [0u8; 128];
        raw.copy_from_slice(&buf[o..o + 128]);
        Ok(DInode { raw })
    }

    pub fn write_inode(&self, ino: u32, n: &DInode) -> KResult<()> {
        self.writable()?;
        let at = self.inode_location(ino)?;
        let sector = at & !511;
        let mut buf = vec![0u8; 512];
        self.dev.read(sector, &mut buf)?;
        let o = (at - sector) as usize;
        buf[o..o + 128].copy_from_slice(&n.raw);
        self.dev.write(sector, &buf)
    }

    fn alloc_inode(&mut self, goal: usize, dir: bool) -> KResult<u32> {
        let n = self.groups.len();
        for k in 0..n {
            let g = (goal + k) % n;
            if le16(&self.groups[g], 14) == 0 {
                continue;
            }
            let bitmap = le32(&self.groups[g], 4);
            if let Some(bit) = self.take_bit(bitmap, self.inodes_per_group)? {
                let ino = g as u32 * self.inodes_per_group + bit + 1;
                if ino < self.first_ino {
                    continue; // reserved inodes are never handed out
                }
                self.adjust_counts(g, 0, -1, if dir { 1 } else { 0 });
                self.write_group(g)?;
                return Ok(ino);
            }
        }
        Err(ENOSPC)
    }

    fn free_inode(&mut self, ino: u32, dir: bool) -> KResult<()> {
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let bitmap = le32(&self.groups[g], 4);
        self.clear_bit(bitmap, (ino - 1) % self.inodes_per_group)?;
        self.adjust_counts(g, 0, 1, if dir { -1 } else { 0 });
        self.write_group(g)
    }

    fn group_of(&self, ino: u32) -> usize {
        ((ino - 1) / self.inodes_per_group) as usize
    }

    // ------------------------------------------------------------------ block maps

    fn ptrs_per_block(&self) -> u64 {
        self.bs() / 4
    }

    fn read_ptr(&self, block: u32, i: u64) -> KResult<u32> {
        let mut b = [0u8; 4];
        // Read the 512-byte sector holding the pointer.
        let at = block as u64 * self.bs() + i * 4;
        let sector = at & !511;
        let mut buf = vec![0u8; 512];
        self.dev.read(sector, &mut buf)?;
        let o = (at - sector) as usize;
        b.copy_from_slice(&buf[o..o + 4]);
        Ok(u32::from_le_bytes(b))
    }

    fn write_ptr(&self, block: u32, i: u64, v: u32) -> KResult<()> {
        let at = block as u64 * self.bs() + i * 4;
        let sector = at & !511;
        let mut buf = vec![0u8; 512];
        self.dev.read(sector, &mut buf)?;
        let o = (at - sector) as usize;
        buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self.dev.write(sector, &buf)
    }

    /// The path through the block map to logical block `idx`: (slot in i_block, indices in the
    /// indirect blocks below it).
    fn map_path(&self, idx: u64) -> KResult<(usize, Vec<u64>)> {
        let p = self.ptrs_per_block();
        if idx < 12 {
            return Ok((idx as usize, Vec::new()));
        }
        let mut i = idx - 12;
        if i < p {
            return Ok((12, vec![i]));
        }
        i -= p;
        if i < p * p {
            return Ok((13, vec![i / p, i % p]));
        }
        i -= p * p;
        if i < p * p * p {
            return Ok((14, vec![i / (p * p), (i / p) % p, i % p]));
        }
        Err(EFBIG)
    }

    /// Physical block of logical block `idx`, 0 for a hole. With `alloc`, holes are filled
    /// (new indirect blocks are zeroed; a new data block is zeroed unless `will_overwrite`).
    fn bmap(&mut self, ino: u32, n: &mut DInode, idx: u64, alloc: bool, will_overwrite: bool) -> KResult<u32> {
        let (slot, path) = self.map_path(idx)?;
        let goal = self.group_of(ino);
        let per_block = self.block_size / 512;
        let mut cur = n.block(slot);
        if cur == 0 {
            if !alloc {
                return Ok(0);
            }
            cur = self.alloc_block(goal)?;
            if !path.is_empty() || !will_overwrite {
                self.zero_block(cur)?;
            }
            n.set_block(slot, cur);
            n.set_blocks512(n.blocks512() + per_block);
        }
        for (depth, &i) in path.iter().enumerate() {
            let next = self.read_ptr(cur, i)?;
            if next != 0 {
                cur = next;
                continue;
            }
            if !alloc {
                return Ok(0);
            }
            let nb = self.alloc_block(goal)?;
            let is_data = depth + 1 == path.len();
            if !is_data || !will_overwrite {
                self.zero_block(nb)?;
            }
            self.write_ptr(cur, i, nb)?;
            n.set_blocks512(n.blocks512() + per_block);
            cur = nb;
        }
        Ok(cur)
    }

    /// Frees the blocks of logical index >= `keep` (and indirect blocks left empty).
    fn free_from(&mut self, n: &mut DInode, keep: u64) -> KResult<()> {
        let per_block = self.block_size / 512;
        let p = self.ptrs_per_block();
        // Direct blocks.
        for i in (keep.min(12) as usize)..12 {
            let b = n.block(i);
            if b != 0 {
                self.free_block(b)?;
                n.set_block(i, 0);
                n.set_blocks512(n.blocks512() - per_block);
            }
        }
        // Indirect trees: slot 12 covers [12, 12+p), 13 the next p^2, 14 the next p^3.
        let mut base = 12u64;
        for (slot, depth) in [(12usize, 1u32), (13, 2), (14, 3)] {
            let span = p.pow(depth);
            let root = n.block(slot);
            if root != 0 && keep < base + span {
                let first = keep.saturating_sub(base);
                let (freed, empty) = self.free_tree(root, depth, first)?;
                n.set_blocks512(n.blocks512() - freed * per_block);
                if empty {
                    self.free_block(root)?;
                    n.set_block(slot, 0);
                    n.set_blocks512(n.blocks512() - per_block);
                }
            }
            base += span;
        }
        Ok(())
    }

    /// Frees entries >= `first` (in data-block units) below indirect block `blk` of `depth`.
    /// Returns (blocks freed below blk, whether blk is now empty).
    fn free_tree(&mut self, blk: u32, depth: u32, first: u64) -> KResult<(u32, bool)> {
        let p = self.ptrs_per_block();
        let span = p.pow(depth - 1); // data blocks per entry
        let mut buf = vec![0u8; self.block_size as usize];
        self.read_block(blk, &mut buf)?;
        let mut freed = 0;
        let mut changed = false;
        for i in 0..p {
            let child = le32(&buf, i as usize * 4);
            if child == 0 {
                continue;
            }
            let child_first = i * span;
            if child_first + span <= first {
                continue; // entirely kept
            }
            if depth == 1 {
                self.free_block(child)?;
                freed += 1;
                put32(&mut buf, i as usize * 4, 0);
                changed = true;
            } else {
                let sub_first = first.saturating_sub(child_first);
                let (f, empty) = self.free_tree(child, depth - 1, sub_first)?;
                freed += f;
                if empty {
                    self.free_block(child)?;
                    freed += 1;
                    put32(&mut buf, i as usize * 4, 0);
                    changed = true;
                }
            }
        }
        if changed {
            self.write_block(blk, &buf)?;
        }
        let empty = (0..p).all(|i| le32(&buf, i as usize * 4) == 0);
        Ok((freed, empty))
    }

    // ------------------------------------------------------------------ file data

    pub fn read(&mut self, ino: u32, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let mut n = self.read_inode(ino)?;
        let size = n.size();
        if off >= size {
            return Ok(0);
        }
        let len = buf.len().min((size - off) as usize);
        let bs = self.bs();
        let mut done = 0;
        let mut blk = vec![0u8; self.block_size as usize];
        while done < len {
            let pos = off + done as u64;
            let idx = pos / bs;
            let within = (pos % bs) as usize;
            let take = (bs as usize - within).min(len - done);
            let b = self.bmap(ino, &mut n, idx, false, false)?;
            if b == 0 {
                buf[done..done + take].fill(0);
            } else if within == 0 && take == bs as usize {
                self.read_block(b, &mut buf[done..done + take])?;
            } else {
                self.read_block(b, &mut blk)?;
                buf[done..done + take].copy_from_slice(&blk[within..within + take]);
            }
            done += take;
        }
        Ok(len)
    }

    pub fn write(&mut self, ino: u32, off: u64, data: &[u8]) -> KResult<usize> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        let end = off.checked_add(data.len() as u64).ok_or(EFBIG)?;
        if end > u32::MAX as u64 && !(self.large_file && n.kind() == S_IFREG) {
            return Err(EFBIG);
        }
        let bs = self.bs();
        let mut done = 0;
        let mut blk = vec![0u8; self.block_size as usize];
        let result = (|| -> KResult<()> {
            while done < data.len() {
                let pos = off + done as u64;
                let idx = pos / bs;
                let within = (pos % bs) as usize;
                let take = (bs as usize - within).min(data.len() - done);
                let full = within == 0 && take == bs as usize;
                let b = self.bmap(ino, &mut n, idx, true, full)?;
                if full {
                    self.write_block(b, &data[done..done + take])?;
                } else {
                    self.read_block(b, &mut blk)?;
                    blk[within..within + take].copy_from_slice(&data[done..done + take]);
                    self.write_block(b, &blk)?;
                }
                done += take;
            }
            Ok(())
        })();
        if off + done as u64 > n.size() {
            n.set_size(off + done as u64);
        }
        let now = (self.clock)();
        n.set_times(n.atime(), now, now);
        self.write_inode(ino, &n)?;
        match result {
            Ok(()) => Ok(done),
            Err(e) if done > 0 && e == ENOSPC => Ok(done),
            Err(e) => Err(e),
        }
    }

    /// Changes a file's length; blocks past the end are freed, the tail of the last block cleared.
    pub fn truncate(&mut self, ino: u32, len: u64) -> KResult<()> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        let bs = self.bs();
        if len < n.size() {
            let keep = len.div_ceil(bs);
            self.free_from(&mut n, keep)?;
            // Zero the rest of the last kept block so a later extension reads zeros.
            if len % bs != 0 {
                let b = self.bmap(ino, &mut n, len / bs, false, false)?;
                if b != 0 {
                    let mut blk = vec![0u8; self.block_size as usize];
                    self.read_block(b, &mut blk)?;
                    blk[(len % bs) as usize..].fill(0);
                    self.write_block(b, &blk)?;
                }
            }
        }
        n.set_size(len);
        let now = (self.clock)();
        n.set_times(n.atime(), now, now);
        self.write_inode(ino, &n)
    }

    // ------------------------------------------------------------------ directories

    /// Calls `f(block_index, physical_block, block_bytes)` for every block of a directory.
    fn dir_blocks(&mut self, dir: u32, mut f: impl FnMut(&mut Self, u64, u32, &mut [u8]) -> KResult<bool>) -> KResult<()> {
        let mut n = self.read_inode(dir)?;
        if !n.is_dir() {
            return Err(ENOTDIR);
        }
        let count = n.size().div_ceil(self.bs());
        let mut buf = vec![0u8; self.block_size as usize];
        for idx in 0..count {
            let b = self.bmap(dir, &mut n, idx, false, false)?;
            if b == 0 {
                continue;
            }
            self.read_block(b, &mut buf)?;
            if !f(self, idx, b, &mut buf)? {
                break;
            }
        }
        Ok(())
    }

    fn parse_block(buf: &[u8], mut f: impl FnMut(usize, u32, u16, &[u8], u8) -> bool) -> KResult<()> {
        let mut o = 0;
        while o + 8 <= buf.len() {
            let ino = le32(buf, o);
            let rec = le16(buf, o + 4) as usize;
            let name_len = buf[o + 6] as usize;
            let ft = buf[o + 7];
            if rec < 8 || o + rec > buf.len() || 8 + name_len > rec {
                return Err(EIO);
            }
            if !f(o, ino, rec as u16, &buf[o + 8..o + 8 + name_len], ft) {
                break;
            }
            o += rec;
        }
        Ok(())
    }

    pub fn readdir(&mut self, dir: u32) -> KResult<Vec<Entry>> {
        let mut out = Vec::new();
        self.dir_blocks(dir, |_, _, _, buf| {
            Self::parse_block(buf, |_, ino, _, name, ft| {
                if ino != 0 {
                    out.push(Entry { name: String::from_utf8_lossy(name).into_owned(), ino, file_type: ft });
                }
                true
            })?;
            Ok(true)
        })?;
        Ok(out)
    }

    pub fn lookup(&mut self, dir: u32, name: &str) -> KResult<u32> {
        let mut found = 0;
        self.dir_blocks(dir, |_, _, _, buf| {
            Self::parse_block(buf, |_, ino, _, n, _| {
                if ino != 0 && n == name.as_bytes() {
                    found = ino;
                    return false;
                }
                true
            })?;
            Ok(found == 0)
        })?;
        if found == 0 { Err(ENOENT) } else { Ok(found) }
    }

    fn entry_len(name_len: usize) -> usize {
        (8 + name_len + 3) & !3
    }

    fn write_entry(&self, buf: &mut [u8], o: usize, ino: u32, rec: u16, name: &[u8], ft: u8) {
        put32(buf, o, ino);
        put16(buf, o + 4, rec);
        buf[o + 6] = name.len() as u8;
        buf[o + 7] = if self.filetype { ft } else { 0 };
        buf[o + 8..o + 8 + name.len()].copy_from_slice(name);
    }

    /// A directory being changed loses its htree index (it stays a valid linear directory).
    fn drop_index(&mut self, dir: u32) -> KResult<()> {
        let mut n = self.read_inode(dir)?;
        if n.flags() & INDEX_FL != 0 {
            n.set_flags(n.flags() & !INDEX_FL);
            self.write_inode(dir, &n)?;
        }
        Ok(())
    }

    fn add_entry(&mut self, dir: u32, name: &str, ino: u32, ft: u8) -> KResult<()> {
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(EINVAL);
        }
        self.drop_index(dir)?;
        let need = Self::entry_len(name.len());
        let mut placed = false;
        self.dir_blocks(dir, |fs, _, b, buf| {
            let mut slot = None;
            Self::parse_block(buf, |o, eino, rec, n, _| {
                let used = if eino == 0 { 0 } else { Self::entry_len(n.len()) };
                if rec as usize >= used + need {
                    slot = Some((o, eino, rec, used));
                    return false;
                }
                true
            })?;
            if let Some((o, eino, rec, used)) = slot {
                if eino == 0 {
                    fs.write_entry(buf, o, ino, rec, name.as_bytes(), ft);
                } else {
                    put16(buf, o + 4, used as u16);
                    fs.write_entry(buf, o + used, ino, rec - used as u16, name.as_bytes(), ft);
                }
                fs.write_block(b, buf)?;
                placed = true;
                return Ok(false);
            }
            Ok(true)
        })?;
        if placed {
            return Ok(());
        }
        // A new block at the end of the directory.
        let mut n = self.read_inode(dir)?;
        let idx = n.size() / self.bs();
        let b = self.bmap(dir, &mut n, idx, true, true)?;
        let mut buf = vec![0u8; self.block_size as usize];
        self.write_entry(&mut buf, 0, ino, self.block_size as u16, name.as_bytes(), ft);
        self.write_block(b, &buf)?;
        n.set_size(n.size() + self.bs());
        self.write_inode(dir, &n)
    }

    fn remove_entry(&mut self, dir: u32, name: &str) -> KResult<u32> {
        self.drop_index(dir)?;
        let mut removed = 0;
        self.dir_blocks(dir, |fs, _, b, buf| {
            let mut prev: Option<usize> = None;
            let mut hit = None;
            Self::parse_block(buf, |o, ino, rec, n, _| {
                if ino != 0 && n == name.as_bytes() {
                    hit = Some((o, ino, rec, prev));
                    return false;
                }
                prev = Some(o);
                true
            })?;
            if let Some((o, ino, rec, prev)) = hit {
                match prev {
                    Some(p) => {
                        let prec = le16(buf, p + 4);
                        put16(buf, p + 4, prec + rec);
                    }
                    None => put32(buf, o, 0),
                }
                fs.write_block(b, buf)?;
                removed = ino;
                return Ok(false);
            }
            Ok(true)
        })?;
        if removed == 0 { Err(ENOENT) } else { Ok(removed) }
    }

    fn dir_is_empty(&mut self, dir: u32) -> KResult<bool> {
        Ok(self.readdir(dir)?.iter().all(|e| e.name == "." || e.name == ".."))
    }

    fn set_dotdot(&mut self, dir: u32, parent: u32) -> KResult<()> {
        let mut done = false;
        self.dir_blocks(dir, |fs, _, b, buf| {
            let mut at = None;
            Self::parse_block(buf, |o, _, _, n, _| {
                if n == b".." {
                    at = Some(o);
                    return false;
                }
                true
            })?;
            if let Some(o) = at {
                put32(buf, o, parent);
                fs.write_block(b, buf)?;
                done = true;
                return Ok(false);
            }
            Ok(true)
        })?;
        if done { Ok(()) } else { Err(EIO) }
    }

    // ------------------------------------------------------------------ namespace operations

    fn new_inode(&mut self, dir: u32, mode: u16) -> KResult<(u32, DInode)> {
        let is_dir = mode & S_IFMT == S_IFDIR;
        let ino = self.alloc_inode(self.group_of(dir), is_dir)?;
        let now = (self.clock)();
        let mut n = DInode { raw: [0; 128] };
        n.set_mode(mode);
        n.set_links(1);
        n.set_times(now, now, now);
        Ok((ino, n))
    }

    /// Creates a file, FIFO, socket or device node (`rdev` for devices). Returns its inode.
    pub fn create(&mut self, dir: u32, name: &str, mode: u16, rdev: u32) -> KResult<u32> {
        self.writable()?;
        if mode & S_IFMT == S_IFDIR {
            return self.mkdir(dir, name, mode);
        }
        if self.lookup(dir, name).is_ok() {
            return Err(EEXIST);
        }
        let (ino, mut n) = self.new_inode(dir, mode)?;
        if matches!(mode & S_IFMT, S_IFCHR | S_IFBLK) {
            n.set_block(0, rdev);
        }
        self.write_inode(ino, &n)?;
        if let Err(e) = self.add_entry(dir, name, ino, file_type_of(mode)) {
            self.free_inode(ino, false)?;
            return Err(e);
        }
        Ok(ino)
    }

    pub fn mkdir(&mut self, dir: u32, name: &str, mode: u16) -> KResult<u32> {
        self.writable()?;
        if self.lookup(dir, name).is_ok() {
            return Err(EEXIST);
        }
        let mode = (mode & 0o7777) | S_IFDIR;
        let (ino, mut n) = self.new_inode(dir, mode)?;
        n.set_links(2);
        let b = self.bmap(ino, &mut n, 0, true, true)?;
        let mut buf = vec![0u8; self.block_size as usize];
        self.write_entry(&mut buf, 0, ino, 12, b".", 2);
        self.write_entry(&mut buf, 12, dir, self.block_size as u16 - 12, b"..", 2);
        self.write_block(b, &buf)?;
        n.set_size(self.bs());
        self.write_inode(ino, &n)?;
        self.add_entry(dir, name, ino, 2)?;
        let mut p = self.read_inode(dir)?;
        p.set_links(p.links() + 1);
        self.write_inode(dir, &p)?;
        Ok(ino)
    }

    pub fn symlink(&mut self, dir: u32, name: &str, target: &str) -> KResult<u32> {
        self.writable()?;
        if self.lookup(dir, name).is_ok() {
            return Err(EEXIST);
        }
        let (ino, mut n) = self.new_inode(dir, S_IFLNK | 0o777)?;
        if target.len() < 60 {
            n.raw[40..40 + target.len()].copy_from_slice(target.as_bytes());
            n.set_size(target.len() as u64);
            self.write_inode(ino, &n)?;
        } else {
            self.write_inode(ino, &n)?;
            self.write(ino, 0, target.as_bytes())?;
        }
        self.add_entry(dir, name, ino, 7)?;
        Ok(ino)
    }

    pub fn readlink(&mut self, ino: u32) -> KResult<String> {
        let n = self.read_inode(ino)?;
        if n.kind() != S_IFLNK {
            return Err(EINVAL);
        }
        if n.is_fast_symlink() {
            let len = n.size() as usize;
            return Ok(String::from_utf8_lossy(&n.raw[40..40 + len]).into_owned());
        }
        let mut buf = vec![0u8; n.size() as usize];
        let got = self.read(ino, 0, &mut buf)?;
        buf.truncate(got);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    pub fn link(&mut self, dir: u32, name: &str, ino: u32) -> KResult<()> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        if n.is_dir() {
            return Err(EPERM);
        }
        if self.lookup(dir, name).is_ok() {
            return Err(EEXIST);
        }
        if n.links() >= 65000 {
            return Err(EMLINK);
        }
        self.add_entry(dir, name, ino, file_type_of(n.mode()))?;
        n.set_links(n.links() + 1);
        n.set_times(n.atime(), n.mtime(), (self.clock)());
        self.write_inode(ino, &n)
    }

    /// Removes a name. Returns the inode and its remaining link count; at 0 the caller frees it
    /// with [`Ext2::release`] once no open file refers to it.
    pub fn unlink(&mut self, dir: u32, name: &str) -> KResult<(u32, u16)> {
        self.writable()?;
        let ino = self.lookup(dir, name)?;
        let mut n = self.read_inode(ino)?;
        if n.is_dir() {
            return Err(EISDIR);
        }
        self.remove_entry(dir, name)?;
        let links = n.links().saturating_sub(1);
        n.set_links(links);
        n.set_times(n.atime(), n.mtime(), (self.clock)());
        self.write_inode(ino, &n)?;
        Ok((ino, links))
    }

    /// Frees an inode with no links left: its blocks, then the inode itself.
    pub fn release(&mut self, ino: u32) -> KResult<()> {
        let mut n = self.read_inode(ino)?;
        if n.links() != 0 {
            return Ok(());
        }
        let dir = n.is_dir();
        if !n.is_fast_symlink() {
            self.free_from(&mut n, 0)?;
        }
        n.set_size(0);
        // A deletion time that could be read as an inode number would look like an orphan-list
        // link to e2fsck (ext3's orphan list reuses this field).
        n.set_dtime((self.clock)().max(self.inodes_count + 1));
        self.write_inode(ino, &n)?;
        self.free_inode(ino, dir)
    }

    pub fn rmdir(&mut self, dir: u32, name: &str) -> KResult<u32> {
        self.writable()?;
        if name == "." || name == ".." {
            return Err(EINVAL);
        }
        let ino = self.lookup(dir, name)?;
        let mut n = self.read_inode(ino)?;
        if !n.is_dir() {
            return Err(ENOTDIR);
        }
        if !self.dir_is_empty(ino)? {
            return Err(ENOTEMPTY);
        }
        self.remove_entry(dir, name)?;
        n.set_links(0);
        self.write_inode(ino, &n)?;
        let mut p = self.read_inode(dir)?;
        p.set_links(p.links().saturating_sub(1));
        self.write_inode(dir, &p)?;
        self.release(ino)?;
        Ok(ino)
    }

    /// Moves `old` in `odir` to `new` in `ndir`, replacing a file or an empty directory there.
    /// Returns an inode whose last link went away (to be released), if any.
    pub fn rename(&mut self, odir: u32, old: &str, ndir: u32, new: &str) -> KResult<Option<u32>> {
        self.writable()?;
        let ino = self.lookup(odir, old)?;
        let n = self.read_inode(ino)?;
        if odir == ndir && old == new {
            return Ok(None);
        }
        // A directory may not move below itself.
        if n.is_dir() {
            let mut p = ndir;
            for _ in 0..4096 {
                if p == ino {
                    return Err(EINVAL);
                }
                if p == ROOT_INO {
                    break;
                }
                p = self.lookup(p, "..")?;
            }
        }
        let mut orphan = None;
        if let Ok(target) = self.lookup(ndir, new) {
            let t = self.read_inode(target)?;
            if t.is_dir() {
                if !n.is_dir() {
                    return Err(EISDIR);
                }
                self.rmdir(ndir, new)?;
            } else {
                if n.is_dir() {
                    return Err(ENOTDIR);
                }
                let (t_ino, links) = self.unlink(ndir, new)?;
                if links == 0 {
                    orphan = Some(t_ino);
                }
            }
        }
        self.add_entry(ndir, new, ino, file_type_of(n.mode()))?;
        self.remove_entry(odir, old)?;
        if n.is_dir() && odir != ndir {
            self.set_dotdot(ino, ndir)?;
            let mut a = self.read_inode(odir)?;
            a.set_links(a.links().saturating_sub(1));
            self.write_inode(odir, &a)?;
            let mut b = self.read_inode(ndir)?;
            b.set_links(b.links() + 1);
            self.write_inode(ndir, &b)?;
        }
        Ok(orphan)
    }

    pub fn chmod(&mut self, ino: u32, mode: u16) -> KResult<()> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        n.set_mode((n.mode() & S_IFMT) | (mode & 0o7777));
        n.set_times(n.atime(), n.mtime(), (self.clock)());
        self.write_inode(ino, &n)
    }

    pub fn chown(&mut self, ino: u32, uid: u16, gid: u16) -> KResult<()> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        n.set_owner(uid, gid);
        self.write_inode(ino, &n)
    }

    pub fn utimes(&mut self, ino: u32, atime: u32, mtime: u32) -> KResult<()> {
        self.writable()?;
        let mut n = self.read_inode(ino)?;
        n.set_times(atime, mtime, (self.clock)());
        self.write_inode(ino, &n)
    }
}

#[cfg(test)]
mod tests;
