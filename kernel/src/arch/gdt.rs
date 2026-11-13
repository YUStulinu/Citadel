//! Segments: one GDT and one TSS per CPU.
//!
//! Selector layout (fixed by SYSCALL/SYSRET, which derive CS/SS from STAR):
//!   0x08 kernel code, 0x10 kernel data, 0x18 user data, 0x20 user code, 0x28 TSS (16 bytes)
use core::arch::asm;
use core::mem::size_of;

pub const KCODE: u16 = 0x08;
pub const KDATA: u16 = 0x10;
pub const UDATA: u16 = 0x18 | 3;
pub const UCODE: u16 = 0x20 | 3;
pub const TSS_SEL: u16 = 0x28;

#[repr(C, packed)]
#[derive(Default)]
pub struct Tss {
    _r0: u32,
    pub rsp0: u64,
    pub rsp1: u64,
    pub rsp2: u64,
    _r1: u64,
    pub ist: [u64; 7],
    _r2: u64,
    _r3: u16,
    pub iomap: u16,
}

#[repr(C, align(16))]
pub struct Gdt {
    entries: [u64; 7],
}

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

impl Gdt {
    pub const fn new() -> Self {
        Gdt {
            entries: [
                0,
                0x00af_9a00_0000_ffff, // kernel code: present, ring 0, executable, long mode
                0x00cf_9200_0000_ffff, // kernel data
                0x00cf_f200_0000_ffff, // user data: ring 3
                0x00af_fa00_0000_ffff, // user code: ring 3, long mode
                0,                     // TSS (low)
                0,                     // TSS (high)
            ],
        }
    }

    /// Loads this GDT and the TSS on the current CPU. Both must live forever (per-CPU data).
    pub fn load(&'static mut self, tss: &'static Tss) {
        let base = tss as *const Tss as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        self.entries[5] = limit | ((base & 0xff_ffff) << 16) | (0x89 << 40) | (((base >> 24) & 0xff) << 56);
        self.entries[6] = base >> 32;
        let p = Pointer { limit: (size_of::<[u64; 7]>() - 1) as u16, base: self.entries.as_ptr() as u64 };
        unsafe {
            asm!(
                "lgdt [{p}]",
                "push {kcode}",
                "lea {tmp}, [rip + 2f]",
                "push {tmp}",
                "retfq",
                "2:",
                "mov ax, {kdata}",
                "mov ds, ax",
                "mov es, ax",
                "mov ss, ax",
                "xor eax, eax",
                "mov fs, ax",
                "mov gs, ax",
                p = in(reg) &p,
                kcode = const KCODE as u64,
                kdata = const KDATA,
                tmp = lateout(reg) _,
                out("rax") _,
            );
            asm!("ltr {0:x}", in(reg) TSS_SEL);
        }
    }
}

impl Default for Gdt {
    fn default() -> Self {
        Self::new()
    }
}
