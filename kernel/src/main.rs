//! Citadel: a small Unix-like kernel for x86-64.
#![no_std]
#![no_main]

extern crate alloc;

pub mod acpi;
pub mod arch;
pub mod boot;
pub mod drivers;
pub mod exec;
pub mod fs;
pub mod interrupts;
pub mod mm;
pub mod net;
pub mod proc;
pub mod profile;
pub mod sched;
pub mod serial;
pub mod signal;
pub mod sync;
pub mod syscall;
pub mod time;
pub mod tty;
pub mod user;

use core::arch::global_asm;


use arch::{apic, cpu, idt, percpu, smp};

// The entry point: the bootloader calls _start(boot_info_phys) on its own stack, with our page
// tables loaded. Switch to the boot stack in .bss and call kmain.
global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _start
_start:
    lea rsp, [rip + {stack} + {size}]
    xor rbp, rbp
    call {kmain}
1:  cli
    hlt
    jmp 1b
"#,
    stack = sym BOOT_STACK,
    size = const BOOT_STACK_SIZE,
    kmain = sym kmain,
);

const BOOT_STACK_SIZE: usize = 64 * 1024;
#[repr(C, align(16))]
struct Stack([u8; BOOT_STACK_SIZE]);
static mut BOOT_STACK: Stack = Stack([0; BOOT_STACK_SIZE]);
static mut DF_STACK: Stack = Stack([0; BOOT_STACK_SIZE]);

pub static BOOT_INFO: sync::Once<&'static boot::BootInfo> = sync::Once::new();

extern "C" fn kmain(boot_info_phys: u64) -> ! {
    serial::init_port(serial::COM1);
    let bi = unsafe { &*(boot::phys_to_virt(boot_info_phys) as *const boot::BootInfo) };
    if bi.magic != boot::BOOT_MAGIC {
        kprintln!("citadel: bad boot info magic {:#x}", bi.magic);
        cpu::halt_forever();
    }
    boot::set_kernel_location(bi.kernel_phys_start, bi.kernel_virt_start);
    BOOT_INFO.set(bi);
    cpu::init_features();
    kprintln!("citadel: booting, cmdline \"{}\"", bi.cmdline());

    if bi.arg("thp") == Some("off") {
        mm::fault::THP.store(false, core::sync::atomic::Ordering::Relaxed);
    }
    mm::frame::init(bi);
    mm::paging::init();
    kprintln!("citadel: memory {} MiB free of {} MiB", mm::frame::free_bytes() >> 20, mm::frame::total_bytes() >> 20);

    let df_top = core::ptr::addr_of!(DF_STACK) as u64 + BOOT_STACK_SIZE as u64;
    percpu::init_this_cpu(0, 0, df_top);
    idt::init();

    let madt = acpi::madt(bi.rsdp);
    apic::map(madt.lapic_phys, madt.ioapic_phys);
    apic::init_local();
    percpu::this().apic_id = apic::id();
    apic::calibrate();
    syscall::init_msrs();
    sched::init_cpu();
    apic::start_timer(sched::HZ);
    kprintln!("citadel: TSC {} MHz, {} CPUs in ACPI, {}", apic::tsc_per_us(), madt.apic_ids.len(), if apic::x2apic_enabled() { "x2APIC" } else { "xAPIC" });
    kprintln!("citadel: CPU {} ({})", cpu::brand(), cpu::vendor());

    let aps = smp::start_aps(&madt.apic_ids);
    kprintln!("citadel: {} CPUs online", 1 + aps);

    time::init();
    tty::init();
    // The console: COM1's receive interrupt feeds the terminal.
    let com1_gsi = madt.overrides.iter().find(|o| o.0 == 4).map(|o| o.1).unwrap_or(4);
    interrupts::register(apic::IRQ_BASE + com1_gsi as u8, || {
        while let Some(c) = serial::try_getb(serial::COM1) {
            tty::input(c);
        }
    });
    apic::route_irq(com1_gsi, percpu::this().apic_id, false);
    serial::enable_rx_irq(serial::COM1);

    // File systems: tmpfs at / (filled from the initrd), /dev, /proc, /tmp.
    fs::mount("/", fs::tmpfs::TmpInode::new_root(), "tmpfs");
    if bi.initrd_size > 0 {
        let data = unsafe { core::slice::from_raw_parts(boot::phys_to_virt(bi.initrd_phys) as *const u8, bi.initrd_size as usize) };
        match fs::unpack_initrd(data) {
            Ok(n) => kprintln!("citadel: initrd: {n} entries ({} KiB)", bi.initrd_size / 1024),
            Err(e) => kprintln!("citadel: initrd unreadable (errno {e})"),
        }
    }
    for d in ["/dev", "/proc", "/tmp"] {
        let _ = fs::mkdir_p(d);
    }
    fs::mount("/dev", fs::devfs::DevDir::new(), "devfs");
    fs::mount("/tmp", fs::tmpfs::TmpInode::new_root(), "tmpfs");
    fs::mount("/proc", fs::procfs::ProcRoot::new(), "proc");

    // Devices: disks (an ext2 partition is mounted at /mnt).
    drivers::pci::init();
    drivers::blk::init();
    if let Some(d) = fs::ext2::mount_first("/mnt") {
        kprintln!("citadel: mounted {d} on /mnt");
    }
    net::init();
    match bi.arg("profile") {
        Some("1") => profile::start(false),
        Some("2") => profile::start(true),
        _ => {}
    }

    let init = bi.arg("init").unwrap_or("/bin/init");
    // Words after "--" on the command line become init's arguments.
    let mut argv: alloc::vec::Vec<&str> = alloc::vec![init];
    if let Some(rest) = bi.cmdline().split_once(" -- ") {
        argv.extend(rest.1.split_whitespace());
    }
    proc::spawn_init(init, &argv);
    cpu::enable_irq();
    sched::idle_loop();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cpu::disable_irq();
    serial::print_raw(format_args!("\nKERNEL PANIC on CPU {}: {info}\n", percpu_id_or_zero()));
    apic_halt_others();
    cpu::halt_forever();
}

fn percpu_id_or_zero() -> usize {
    if cpu::rdmsr(cpu::MSR_GS_BASE) != 0 { percpu::cpu_id() } else { 0 }
}

fn apic_halt_others() {
    if percpu::online() > 1 {
        apic::ipi_all_others(apic::IPI_HALT);
    }
}

/// Power the machine off. Under QEMU the isa-debug-exit device (port 0xf4) ends the emulator
/// with status (code << 1) | 1; otherwise ACPI S5 through QEMU's PM1a control port.
pub fn poweroff(code: u32) -> ! {
    fs::ext2::sync_all();
    arch::port::outl(0xf4, code);
    arch::port::outw(0x604, 0x2000);
    arch::port::outw(0xb004, 0x2000);
    arch::cpu::halt_forever()
}
