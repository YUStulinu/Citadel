//! Just enough ACPI to find the CPUs and the interrupt controllers: RSDP -> XSDT -> MADT.
use alloc::vec::Vec;

use crate::boot::phys_to_virt;

pub struct Madt {
    pub lapic_phys: u64,
    pub ioapic_phys: u64,
    pub ioapic_gsi_base: u32,
    /// APIC ids of the usable CPUs (the BSP first if it is listed first by the firmware).
    pub apic_ids: Vec<u32>,
    /// Legacy IRQ -> (GSI, active low, level triggered) overrides.
    pub overrides: Vec<(u8, u32, bool, bool)>,
}

fn bytes(phys: u64, len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(phys_to_virt(phys) as *const u8, len) }
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Finds a table by signature through the XSDT (or the RSDT for ACPI 1.0).
pub fn find_table(rsdp: u64, sig: &[u8; 4]) -> Option<u64> {
    let r = bytes(rsdp, 36);
    if &r[0..8] != b"RSD PTR " {
        return None;
    }
    let (root, entry) = if r[15] >= 2 { (u64_at(r, 24), 8) } else { (u32_at(r, 16) as u64, 4) };
    let hdr = bytes(root, 36);
    let len = u32_at(hdr, 4) as usize;
    let t = bytes(root, len);
    if !checksum_ok(t) {
        return None;
    }
    let mut o = 36;
    while o + entry <= len {
        let p = if entry == 8 { u64_at(t, o) } else { u32_at(t, o) as u64 };
        if bytes(p, 4) == sig {
            return Some(p);
        }
        o += entry;
    }
    None
}

pub fn madt(rsdp: u64) -> Madt {
    let p = find_table(rsdp, b"APIC").expect("no MADT in ACPI");
    let len = u32_at(bytes(p, 8), 4) as usize;
    let t = bytes(p, len);
    let mut m = Madt { lapic_phys: u32_at(t, 36) as u64, ioapic_phys: 0, ioapic_gsi_base: 0, apic_ids: Vec::new(), overrides: Vec::new() };
    let mut o = 44;
    while o + 2 <= len {
        let (typ, l) = (t[o], t[o + 1] as usize);
        if l == 0 {
            break;
        }
        match typ {
            0 => {
                // Processor local APIC: enabled, or online-capable.
                let flags = u32_at(t, o + 4);
                if flags & 3 != 0 {
                    m.apic_ids.push(t[o + 3] as u32);
                }
            }
            1 if m.ioapic_phys == 0 => {
                m.ioapic_phys = u32_at(t, o + 4) as u64;
                m.ioapic_gsi_base = u32_at(t, o + 8);
            }
            2 => {
                let flags = u16::from_le_bytes([t[o + 8], t[o + 9]]);
                m.overrides.push((t[o + 3], u32_at(t, o + 4), flags & 3 == 3, (flags >> 2) & 3 == 3));
            }
            5 => m.lapic_phys = u64_at(t, o + 4),
            _ => {}
        }
        o += l;
    }
    m
}
