//! Parsing ELF64 executables (x86-64, static or with no interpreter) for exec.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    pub offset: u64,
    pub filesz: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    NotX8664,
    NotExecutable,
    BadSegment,
    Interpreter,
}

pub const MAX_SEGMENTS: usize = 16;

#[derive(Debug)]
pub struct Elf {
    pub entry: u64,
    pub segments: [Option<Segment>; MAX_SEGMENTS],
    pub phdr_vaddr: u64,
    pub phnum: u16,
    pub phent: u16,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// User space ends here (the lower half of the canonical address space).
pub const USER_TOP: u64 = 0x0000_7fff_ffff_f000;

pub fn parse(b: &[u8]) -> Result<Elf, ElfError> {
    if b.len() < 64 {
        return Err(ElfError::TooShort);
    }
    if &b[0..4] != b"\x7fELF" || b[4] != 2 || b[5] != 1 {
        return Err(ElfError::BadMagic);
    }
    if u16le(b, 18) != 62 {
        return Err(ElfError::NotX8664);
    }
    let typ = u16le(b, 16);
    if typ != 2 {
        return Err(ElfError::NotExecutable); // ET_EXEC only: no relocation
    }
    let entry = u64le(b, 24);
    let phoff = u64le(b, 32) as usize;
    let phent = u16le(b, 54);
    let phnum = u16le(b, 56);
    if phent < 56 || phoff.checked_add(phent as usize * phnum as usize).is_none_or(|e| e > b.len()) {
        return Err(ElfError::TooShort);
    }
    let mut segments = [None; MAX_SEGMENTS];
    let mut n = 0;
    let mut phdr_vaddr = 0;
    for i in 0..phnum as usize {
        let o = phoff + i * phent as usize;
        let ptype = u32le(b, o);
        let flags = u32le(b, o + 4);
        let offset = u64le(b, o + 8);
        let vaddr = u64le(b, o + 16);
        let filesz = u64le(b, o + 32);
        let memsz = u64le(b, o + 40);
        match ptype {
            1 => {
                let end = vaddr.checked_add(memsz).ok_or(ElfError::BadSegment)?;
                if filesz > memsz || end > USER_TOP || vaddr < 0x1000 || offset.checked_add(filesz).is_none_or(|e| e > b.len() as u64) {
                    return Err(ElfError::BadSegment);
                }
                if n == MAX_SEGMENTS {
                    return Err(ElfError::BadSegment);
                }
                segments[n] = Some(Segment { vaddr, memsz, offset, filesz, readable: flags & 4 != 0, writable: flags & 2 != 0, executable: flags & 1 != 0 });
                n += 1;
            }
            3 => return Err(ElfError::Interpreter), // PT_INTERP: dynamic linking is not supported
            6 => phdr_vaddr = vaddr,                // PT_PHDR
            _ => {}
        }
    }
    if n == 0 || entry >= USER_TOP {
        return Err(ElfError::BadSegment);
    }
    if phdr_vaddr == 0 {
        // Not given: the headers are usually in the first segment at file offset phoff.
        if let Some(s) = segments[0] {
            if s.offset <= phoff as u64 && (phoff as u64) < s.offset + s.filesz {
                phdr_vaddr = s.vaddr + phoff as u64 - s.offset;
            }
        }
    }
    Ok(Elf { entry, segments, phdr_vaddr, phnum, phent })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(phdrs: &[(u32, u32, u64, u64, u64, u64)]) -> Vec<u8> {
        let mut b = vec![0u8; 64 + 56 * phdrs.len() + 256];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&62u16.to_le_bytes());
        b[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
        for (i, &(t, f, off, va, fs, ms)) in phdrs.iter().enumerate() {
            let o = 64 + 56 * i;
            b[o..o + 4].copy_from_slice(&t.to_le_bytes());
            b[o + 4..o + 8].copy_from_slice(&f.to_le_bytes());
            b[o + 8..o + 16].copy_from_slice(&off.to_le_bytes());
            b[o + 16..o + 24].copy_from_slice(&va.to_le_bytes());
            b[o + 32..o + 40].copy_from_slice(&fs.to_le_bytes());
            b[o + 40..o + 48].copy_from_slice(&ms.to_le_bytes());
        }
        b
    }

    #[test]
    fn parses_segments() {
        let b = exe(&[(1, 5, 0, 0x400000, 200, 200), (1, 6, 100, 0x402000, 50, 4096)]);
        let e = parse(&b).unwrap();
        assert_eq!(e.entry, 0x401000);
        let s0 = e.segments[0].unwrap();
        assert!(s0.executable && s0.readable && !s0.writable);
        let s1 = e.segments[1].unwrap();
        assert!(s1.writable && s1.memsz == 4096);
        assert_eq!(e.phdr_vaddr, 0x400040);
        assert!(e.segments[2].is_none());
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(parse(b"short").unwrap_err(), ElfError::TooShort);
        let mut b = exe(&[(1, 5, 0, 0x400000, 200, 200)]);
        b[0] = 0;
        assert_eq!(parse(&b).unwrap_err(), ElfError::BadMagic);
        assert_eq!(parse(&exe(&[(3, 4, 0, 0, 0, 0)])).unwrap_err(), ElfError::Interpreter);
        // filesz > memsz, kernel address, file range past the end
        for ph in [(1, 6, 0, 0x400000, 300, 200), (1, 6, 0, 0xffff800000000000, 10, 10), (1, 6, 10_000, 0x400000, 10, 10)] {
            assert_eq!(parse(&exe(&[ph])).unwrap_err(), ElfError::BadSegment);
        }
    }
}
