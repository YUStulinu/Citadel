//! Basic CPU control: interrupts, control registers, MSRs, the TSC.
use core::arch::asm;

#[inline]
pub fn irq_enabled() -> bool {
    let f: u64;
    unsafe { asm!("pushfq; pop {}", out(reg) f, options(nomem, preserves_flags)) };
    f & (1 << 9) != 0
}

/// Disables interrupts and returns whether they were on.
#[inline]
pub fn irq_save() -> bool {
    let on = irq_enabled();
    unsafe { asm!("cli", options(nomem, nostack)) };
    on
}

#[inline]
pub fn irq_restore(on: bool) {
    if on {
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
}

#[inline]
pub fn enable_irq() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

#[inline]
pub fn disable_irq() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

#[inline]
pub fn halt() {
    unsafe { asm!("hlt", options(nomem, nostack)) };
}

/// Enables interrupts and halts in one step (no wake-up lost in between).
#[inline]
pub fn enable_and_halt() {
    unsafe { asm!("sti; hlt", options(nomem, nostack)) };
}

pub fn halt_forever() -> ! {
    loop {
        disable_irq();
        halt();
    }
}

#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    ((hi as u64) << 32) | lo as u64
}

#[inline]
pub fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    ((hi as u64) << 32) | lo as u64
}

#[inline]
pub fn wrmsr(msr: u32, v: u64) {
    unsafe { asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nomem, nostack)) };
}

#[inline]
pub fn read_cr2() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack)) };
    v
}

#[inline]
pub fn read_cr3() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack)) };
    v
}

#[inline]
pub fn write_cr3(v: u64) {
    unsafe { asm!("mov cr3, {}", in(reg) v, options(nostack)) };
}

#[inline]
pub fn invlpg(addr: u64) {
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack)) };
}

pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

pub fn cpuid(leaf: u32, sub: u32) -> CpuidResult {
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    CpuidResult { eax: r.eax, ebx: r.ebx, ecx: r.ecx, edx: r.edx }
}

pub const MSR_EFER: u32 = 0xc000_0080;
pub const MSR_STAR: u32 = 0xc000_0081;
pub const MSR_LSTAR: u32 = 0xc000_0082;
pub const MSR_FMASK: u32 = 0xc000_0084;
pub const MSR_FS_BASE: u32 = 0xc000_0100;
pub const MSR_GS_BASE: u32 = 0xc000_0101;
pub const MSR_KERNEL_GS_BASE: u32 = 0xc000_0102;
pub const MSR_APIC_BASE: u32 = 0x1b;

/// Turns on what the kernel relies on: SSE (user programs use it), no-execute pages, the
/// global-page bit, write protection in ring 0, FSGSBASE when present.
pub fn init_features() {
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // EM off
        cr0 |= (1 << 1) | (1 << 16); // MP, WP
        asm!("mov cr0, {}", in(reg) cr0);
        let mut cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= (1 << 9) | (1 << 10) | (1 << 7); // OSFXSR, OSXMMEXCPT, PGE
        asm!("mov cr4, {}", in(reg) cr4);
    }
    let efer = rdmsr(MSR_EFER);
    wrmsr(MSR_EFER, efer | (1 << 11) | 1); // NXE, SCE (syscall)
}
