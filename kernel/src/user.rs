//! Copying between the kernel and user memory. Every range is checked against the process's
//! memory areas and faulted in first, so the copy itself cannot fault.
use alloc::string::String;
use alloc::vec::Vec;

use kcore::elf::USER_TOP;
use kcore::errno::*;

use crate::mm::{fault, paging};

fn page_ok(cr3: u64, page: u64, write: bool) -> bool {
    match paging::leaf(cr3, page) {
        Some(e) => e & paging::U != 0 && (!write || e & paging::W != 0),
        None => false,
    }
}

/// Checks that [addr, addr+len) is user memory the process may read (or write).
pub fn check(addr: u64, len: usize, write: bool) -> KResult<()> {
    if len == 0 {
        return Ok(());
    }
    let end = addr.checked_add(len as u64).ok_or(EFAULT)?;
    if addr == 0 || end > USER_TOP {
        return Err(EFAULT);
    }
    let cr3 = crate::arch::cpu::read_cr3() & paging::ADDR;
    let mut page = addr & !0xfff;
    while page < end {
        if !page_ok(cr3, page, write) && !fault::handle_user(page.max(addr), write, false) {
            return Err(EFAULT);
        }
        page += 4096;
    }
    Ok(())
}

/// The user range as a kernel slice, after checking it and faulting it in.
///
/// # Safety
/// Valid while the process's mappings do not change: Citadel processes are single-threaded, so
/// nothing can unmap the range during the system call that uses it.
pub unsafe fn slice<'a>(addr: u64, len: usize) -> KResult<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    check(addr, len, false)?;
    Ok(unsafe { core::slice::from_raw_parts(addr as *const u8, len) })
}

/// # Safety
/// As for [`slice`].
pub unsafe fn slice_mut<'a>(addr: u64, len: usize) -> KResult<&'a mut [u8]> {
    if len == 0 {
        return Ok(&mut []);
    }
    check(addr, len, true)?;
    Ok(unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) })
}

pub fn copy_in(addr: u64, dst: &mut [u8]) -> KResult<()> {
    check(addr, dst.len(), false)?;
    unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, dst.as_mut_ptr(), dst.len()) };
    Ok(())
}

pub fn copy_out(addr: u64, src: &[u8]) -> KResult<()> {
    check(addr, src.len(), true)?;
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), addr as *mut u8, src.len()) };
    Ok(())
}

pub fn read_vec(addr: u64, len: usize) -> KResult<Vec<u8>> {
    let mut v = alloc::vec![0u8; len];
    copy_in(addr, &mut v)?;
    Ok(v)
}

/// A NUL-terminated string of at most `max` bytes.
pub fn read_cstr(addr: u64, max: usize) -> KResult<String> {
    let mut out: Vec<u8> = Vec::new();
    let mut a = addr;
    loop {
        // Check a page at a time (the string may end before the next page exists), then copy
        // up to the terminator in one piece.
        let page_end = (a & !0xfff) + 4096;
        check(a, (page_end - a) as usize, false)?;
        let chunk = unsafe { core::slice::from_raw_parts(a as *const u8, (page_end - a) as usize) };
        let end = chunk.iter().position(|&c| c == 0);
        let piece = &chunk[..end.unwrap_or(chunk.len())];
        if out.len() + piece.len() > max {
            return Err(ENAMETOOLONG);
        }
        out.extend_from_slice(piece);
        if end.is_some() {
            return String::from_utf8(out).map_err(|_| EINVAL);
        }
        a = page_end;
    }
}

pub fn read_path(addr: u64) -> KResult<String> {
    let s = read_cstr(addr, 4096)?;
    if s.is_empty() {
        return Err(ENOENT);
    }
    Ok(s)
}

/// A NULL-terminated array of string pointers (argv, envp).
pub fn read_str_array(addr: u64, max_total: usize) -> KResult<Vec<String>> {
    let mut out = Vec::new();
    if addr == 0 {
        return Ok(out);
    }
    let mut total = 0;
    for i in 0.. {
        let p = get_u64(addr + 8 * i)?;
        if p == 0 {
            break;
        }
        let s = read_cstr(p, 128 * 1024)?;
        total += s.len() + 1;
        if total > max_total {
            return Err(E2BIG);
        }
        out.push(s);
    }
    Ok(out)
}

pub fn read_struct<T: Copy>(addr: u64) -> KResult<T> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe { core::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, core::mem::size_of::<T>()) };
    copy_in(addr, bytes)?;
    Ok(unsafe { v.assume_init() })
}

pub fn write_struct<T: Copy>(addr: u64, v: &T) -> KResult<()> {
    let bytes = unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) };
    copy_out(addr, bytes)
}

pub fn get_u32(addr: u64) -> KResult<u32> {
    read_struct(addr)
}
pub fn put_u32(addr: u64, v: u32) -> KResult<()> {
    write_struct(addr, &v)
}
pub fn get_u64(addr: u64) -> KResult<u64> {
    read_struct(addr)
}
pub fn put_u64(addr: u64, v: u64) -> KResult<()> {
    write_struct(addr, &v)
}
pub fn put_i32(addr: u64, v: i32) -> KResult<()> {
    write_struct(addr, &v)
}
