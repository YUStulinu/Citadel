//! The "newc" cpio format of the initial RAM disk (what `cpio -H newc` and Linux use).
//!
//! Each entry: a 110-byte ASCII header ("070701" + 13 eight-digit hex fields), the name
//! (NUL-terminated, padded to 4 bytes with the header), then the data (padded to 4 bytes).
//! The archive ends with an entry named "TRAILER!!!".

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub mode: u32,
    pub data: &'a [u8],
    pub mtime: u32,
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

fn hex(b: &[u8]) -> Option<u32> {
    let s = core::str::from_utf8(b).ok()?;
    u32::from_str_radix(s, 16).ok()
}

fn align4(x: usize) -> usize {
    (x + 3) & !3
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<Entry<'a>, &'static str>;

    fn next(&mut self) -> Option<Self::Item> {
        let b = self.buf;
        let p = self.pos;
        if p + 110 > b.len() {
            return None;
        }
        if &b[p..p + 6] != b"070701" && &b[p..p + 6] != b"070702" {
            self.pos = b.len();
            return Some(Err("bad cpio magic"));
        }
        let field = |i: usize| hex(&b[p + 6 + 8 * i..p + 14 + 8 * i]);
        let (Some(mode), Some(mtime), Some(filesize), Some(namesize)) = (field(1), field(5), field(6), field(11)) else {
            self.pos = b.len();
            return Some(Err("bad cpio header"));
        };
        let name_start = p + 110;
        let name_end = name_start + namesize as usize;
        if namesize == 0 || name_end > b.len() {
            self.pos = b.len();
            return Some(Err("truncated cpio name"));
        }
        let name = match core::str::from_utf8(&b[name_start..name_end - 1]) {
            Ok(n) => n,
            Err(_) => return Some(Err("cpio name is not UTF-8")),
        };
        let data_start = align4(name_end);
        let data_end = data_start + filesize as usize;
        if data_end > b.len() {
            self.pos = b.len();
            return Some(Err("truncated cpio data"));
        }
        self.pos = align4(data_end);
        if name == "TRAILER!!!" {
            self.pos = b.len();
            return None;
        }
        Some(Ok(Entry { name: name.trim_start_matches("./"), mode, data: &b[data_start..data_end], mtime }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let h = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            1, mode, 0, 0, 1, 1234, data.len(), 0, 0, 0, 0, name.len() + 1, 0
        );
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    #[test]
    fn reads_an_archive() {
        let mut a = Vec::new();
        entry(&mut a, "bin", S_IFDIR | 0o755, b"");
        entry(&mut a, "bin/hello", S_IFREG | 0o755, b"#!/bin/sh\necho hi\n");
        entry(&mut a, "./etc/motd", S_IFREG | 0o644, b"x");
        entry(&mut a, "TRAILER!!!", 0, b"");
        let es: Vec<_> = Reader::new(&a).map(|e| e.unwrap()).collect();
        assert_eq!(es.len(), 3);
        assert_eq!(es[1].name, "bin/hello");
        assert_eq!(es[1].data, b"#!/bin/sh\necho hi\n");
        assert_eq!(es[1].mode & S_IFMT, S_IFREG);
        assert_eq!(es[2].name, "etc/motd");
        assert_eq!(es[0].mtime, 1234);
    }

    #[test]
    fn rejects_garbage() {
        let mut a = Vec::new();
        entry(&mut a, "x", S_IFREG, b"abc");
        let mut bad = a.clone();
        bad[0] = b'1';
        assert!(Reader::new(&bad).next().unwrap().is_err());
        let truncated = &a[..a.len() - 4];
        assert!(Reader::new(truncated).next().unwrap().is_err());
        assert!(Reader::new(&[]).next().is_none());
    }
}
