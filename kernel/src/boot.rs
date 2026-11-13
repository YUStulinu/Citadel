//! What the bootloader hands over (layout shared with boot/src/main.zig).

pub const BOOT_MAGIC: u64 = 0x31_4b_4c_44_41_54_49_43; // "CITADLK1"
pub const MAX_REGIONS: usize = 512;

/// Virtual address of physical address 0 (all RAM is mapped here).
pub const DIRECT_MAP: u64 = 0xffff_8000_0000_0000;
/// The kernel image lives above this address (mapped wherever the loader put it).
pub const KERNEL_BASE: u64 = 0xffff_ffff_8000_0000;

static mut KERNEL_PHYS: u64 = 0;
static mut KERNEL_VIRT: u64 = 0;

/// Records where the loader put the kernel image (for virt_to_phys of kernel addresses).
pub fn set_kernel_location(phys: u64, virt: u64) {
    unsafe {
        KERNEL_PHYS = phys;
        KERNEL_VIRT = virt;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemRegion {
    pub base: u64,
    pub len: u64,
    pub kind: u32, // 1 usable, 2 reserved, 3 ACPI reclaimable, 4 ACPI NVS, 5 loader/kernel
    pub _pad: u32,
}

#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub region_count: u64,
    pub regions: [MemRegion; MAX_REGIONS],
    pub rsdp: u64,
    pub fb_base: u64,
    pub fb_size: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_stride: u32,
    pub fb_format: u32,
    pub kernel_phys_start: u64,
    pub kernel_phys_end: u64,
    pub kernel_virt_start: u64,
    pub initrd_phys: u64,
    pub initrd_size: u64,
    pub boot_tsc: u64,
    pub cmdline_len: u64,
    pub cmdline: [u8; 512],
}

impl BootInfo {
    pub fn regions(&self) -> &[MemRegion] {
        &self.regions[..(self.region_count as usize).min(MAX_REGIONS)]
    }
    pub fn cmdline(&self) -> &str {
        core::str::from_utf8(&self.cmdline[..(self.cmdline_len as usize).min(512)]).unwrap_or("")
    }
    /// The value of `key=...` on the command line.
    pub fn arg(&self, key: &str) -> Option<&str> {
        self.cmdline().split_whitespace().find_map(|w| w.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
    }
}

pub fn phys_to_virt(p: u64) -> u64 {
    p + DIRECT_MAP
}

pub fn virt_to_phys(v: u64) -> u64 {
    if v >= KERNEL_BASE { unsafe { v - KERNEL_VIRT + KERNEL_PHYS } } else { v - DIRECT_MAP }
}
