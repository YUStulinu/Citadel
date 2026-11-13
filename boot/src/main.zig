//! Citadel's UEFI bootloader (BOOTX64.EFI).
//!
//! 1. Reads \CITADEL\KERNEL.ELF (and \CITADEL\CMDLINE and the \CITADEL\INITRD cpio archive,
//!    if present) from the boot volume.
//! 2. Loads the kernel's PT_LOAD segments at their physical addresses.
//! 3. Builds page tables: an identity map of the low 4 GiB (this code keeps running there for a
//!    few instructions), a direct map of all physical memory at 0xffff800000000000, and the
//!    kernel's higher half at 0xffffffff80000000 (both with 2 MiB pages).
//! 4. Records the memory map, the framebuffer and the ACPI RSDP in a BootInfo page.
//! 5. Exits boot services, loads CR3 and jumps to the kernel entry with BootInfo's address.
//!
//! The BootInfo layout must match kernel/src/boot.rs.
const std = @import("std");
const uefi = std.os.uefi;
const Status = uefi.Status;

const DIRECT_MAP: u64 = 0xffff800000000000;
const BOOT_MAGIC: u64 = 0x31_4b_4c_44_41_54_49_43; // "CITADLK1"
const MAX_REGIONS = 512;

pub const MemRegion = extern struct {
    base: u64,
    len: u64,
    kind: u32, // 1 usable, 2 reserved, 3 ACPI reclaimable, 4 ACPI NVS, 5 loader/kernel
    _pad: u32 = 0,
};

pub const BootInfo = extern struct {
    magic: u64,
    region_count: u64,
    regions: [MAX_REGIONS]MemRegion,
    rsdp: u64,
    fb_base: u64,
    fb_size: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_format: u32,
    kernel_phys_start: u64,
    kernel_phys_end: u64,
    kernel_virt_start: u64,
    initrd_phys: u64,
    initrd_size: u64,
    boot_tsc: u64,
    cmdline_len: u64,
    cmdline: [512]u8,
};

// ---------------------------------------------------------------- serial (before and after boot services)

fn outb(port: u16, v: u8) void {
    asm volatile ("outb %[v], %[p]"
        :
        : [v] "{al}" (v),
          [p] "{dx}" (port),
    );
}
fn inb(port: u16) u8 {
    return asm volatile ("inb %[p], %[v]"
        : [v] "={al}" (-> u8),
        : [p] "{dx}" (port),
    );
}
fn serialPut(c: u8) void {
    var i: u32 = 0;
    while (inb(0x3F8 + 5) & 0x20 == 0 and i < 100000) : (i += 1) {}
    outb(0x3F8, c);
}
fn log(comptime fmt: []const u8, args: anytype) void {
    var buf: [256]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, fmt, args) catch return;
    for (s) |c| {
        if (c == '\n') serialPut('\r');
        serialPut(c);
    }
}

fn rdtsc() u64 {
    var lo: u32 = undefined;
    var hi: u32 = undefined;
    asm volatile ("rdtsc"
        : [lo] "={eax}" (lo),
          [hi] "={edx}" (hi),
    );
    return (@as(u64, hi) << 32) | lo;
}

fn fail(comptime what: []const u8, s: Status) noreturn {
    log("citadel-boot: " ++ what ++ " failed: {s}\n", .{@tagName(s)});
    while (true) asm volatile ("hlt");
}

fn check(s: Status, comptime what: []const u8) void {
    if (s != .success) fail(what, s);
}

// ---------------------------------------------------------------- memory

var bs: *uefi.tables.BootServices = undefined;

fn allocPages(n: usize) [*]align(4096) u8 {
    var p: [*]align(4096) u8 = undefined;
    check(bs.allocatePages(.allocate_any_pages, .loader_data, n, &p), "allocatePages");
    @memset(p[0 .. n * 4096], 0);
    return p;
}

// ---------------------------------------------------------------- files


fn readFile(root: *const uefi.protocol.File, comptime path: []const u8, optional: bool) ?[]u8 {
    var f: *const uefi.protocol.File = undefined;
    // A comptime literal: NUL-terminated by construction.
    const name = std.unicode.utf8ToUtf16LeStringLiteral(path);
    const s = root.open(&f, name, uefi.protocol.File.efi_file_mode_read, 0);
    if (s != .success) {
        if (optional) return null;
        fail("open " ++ path, s);
    }
    var size: u64 = 0;
    check(f.setPosition(uefi.protocol.File.efi_file_position_end_of_file), "seek end");
    check(f.getPosition(&size), "getPosition");
    check(f.setPosition(0), "seek start");
    const pages = (size + 4095) / 4096;
    const buf = allocPages(@intCast(@max(pages, 1)));
    var left: usize = @intCast(size);
    var off: usize = 0;
    while (left > 0) {
        var n: usize = left;
        check(f.read(&n, buf + off), "read " ++ path);
        if (n == 0) break;
        off += n;
        left -= n;
    }
    _ = f.close();
    return buf[0..off];
}

// ---------------------------------------------------------------- ELF

const Elf64Ehdr = extern struct {
    ident: [16]u8,
    type: u16,
    machine: u16,
    version: u32,
    entry: u64,
    phoff: u64,
    shoff: u64,
    flags: u32,
    ehsize: u16,
    phentsize: u16,
    phnum: u16,
    shentsize: u16,
    shnum: u16,
    shstrndx: u16,
};
const Elf64Phdr = extern struct {
    type: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    @"align": u64,
};

// ---------------------------------------------------------------- paging

const PRESENT: u64 = 1;
const WRITABLE: u64 = 2;
const HUGE: u64 = 1 << 7;

fn table() [*]u64 {
    return @ptrCast(allocPages(1));
}

/// Maps [virt, virt+len) to [phys, ...) with 2 MiB pages (all addresses 2 MiB aligned).
fn map2M(pml4: [*]u64, virt_start: u64, phys_start: u64, len: u64) void {
    var off: u64 = 0;
    while (off < len) : (off += 2 << 20) {
        const v = virt_start + off;
        const p = phys_start + off;
        const l4 = (v >> 39) & 511;
        const l3 = (v >> 30) & 511;
        const l2 = (v >> 21) & 511;
        if (pml4[l4] & PRESENT == 0) pml4[l4] = @intFromPtr(table()) | PRESENT | WRITABLE;
        const pdpt: [*]u64 = @ptrFromInt(pml4[l4] & 0x000ffffffffff000);
        if (pdpt[l3] & PRESENT == 0) pdpt[l3] = @intFromPtr(table()) | PRESENT | WRITABLE;
        const pd: [*]u64 = @ptrFromInt(pdpt[l3] & 0x000ffffffffff000);
        pd[l2] = p | PRESENT | WRITABLE | HUGE;
    }
}

// ---------------------------------------------------------------- main

pub fn main() Status {
    const st = uefi.system_table;
    bs = st.boot_services.?;
    const tsc0 = rdtsc();
    _ = bs.setWatchdogTimer(0, 0, 0, null);
    log("citadel-boot: UEFI loader starting\n", .{});

    // The volume we were loaded from.
    var li: ?*anyopaque = null;
    check(bs.handleProtocol(uefi.handle, &uefi.protocol.LoadedImage.guid, &li), "LoadedImage");
    const loaded: *uefi.protocol.LoadedImage = @ptrCast(@alignCast(li.?));
    var fsp: ?*anyopaque = null;
    check(bs.handleProtocol(loaded.device_handle.?, &uefi.protocol.SimpleFileSystem.guid, &fsp), "SimpleFileSystem");
    const sfs: *const uefi.protocol.SimpleFileSystem = @ptrCast(@alignCast(fsp.?));
    var root: *const uefi.protocol.File = undefined;
    check(sfs.openVolume(&root), "openVolume");

    const elf = readFile(root, "\\CITADEL\\KERNEL.ELF", false).?;
    const cmdline = readFile(root, "\\CITADEL\\CMDLINE", true);
    const initrd = readFile(root, "\\CITADEL\\INITRD", true);

    // Load the kernel's segments at their physical addresses.
    const eh: *const Elf64Ehdr = @ptrCast(@alignCast(elf.ptr));
    if (!std.mem.eql(u8, eh.ident[0..4], "\x7fELF") or eh.machine != 62) fail("kernel is not an x86-64 ELF", .load_error);
    // The kernel's virtual span (2 MiB aligned); it can live at any 2 MiB-aligned physical
    // address, since the page tables map its higher half there.
    const MB2: u64 = 2 << 20;
    var vstart: u64 = std.math.maxInt(u64);
    var vend: u64 = 0;
    var i: usize = 0;
    while (i < eh.phnum) : (i += 1) {
        const ph: *const Elf64Phdr = @ptrCast(@alignCast(elf.ptr + eh.phoff + i * eh.phentsize));
        if (ph.type != 1) continue; // PT_LOAD
        vstart = @min(vstart, ph.vaddr);
        vend = @max(vend, ph.vaddr + ph.memsz);
    }
    vstart &= ~(MB2 - 1);
    const span = (vend - vstart + MB2 - 1) & ~(MB2 - 1);
    const raw = @intFromPtr(allocPages(@intCast((span + MB2) / 4096)));
    const kstart = (raw + MB2 - 1) & ~(MB2 - 1);
    const kend = kstart + span;
    i = 0;
    while (i < eh.phnum) : (i += 1) {
        const ph: *const Elf64Phdr = @ptrCast(@alignCast(elf.ptr + eh.phoff + i * eh.phentsize));
        if (ph.type != 1) continue;
        const dst: [*]u8 = @ptrFromInt(kstart + (ph.vaddr - vstart));
        @memcpy(dst[0..ph.filesz], elf[ph.offset .. ph.offset + ph.filesz]);
        @memset(dst[ph.filesz..ph.memsz], 0);
    }
    log("citadel-boot: kernel {x}-{x} at physical {x}, entry {x}\n", .{ vstart, vstart + span, kstart, eh.entry });

    // BootInfo and the kernel's first stack-independent data.
    const bi: *BootInfo = @ptrCast(@alignCast(allocPages((@sizeOf(BootInfo) + 4095) / 4096)));
    bi.magic = BOOT_MAGIC;
    bi.kernel_phys_start = kstart;
    bi.kernel_phys_end = kend;
    bi.kernel_virt_start = vstart;
    if (initrd) |r| {
        bi.initrd_phys = @intFromPtr(r.ptr);
        bi.initrd_size = r.len;
    }
    bi.boot_tsc = tsc0;
    if (cmdline) |c| {
        var n = @min(c.len, bi.cmdline.len);
        while (n > 0 and (c[n - 1] == '\n' or c[n - 1] == '\r')) n -= 1;
        @memcpy(bi.cmdline[0..n], c[0..n]);
        bi.cmdline_len = n;
    }

    // ACPI 2.0 RSDP.
    var t: usize = 0;
    while (t < st.number_of_table_entries) : (t += 1) {
        const ct = st.configuration_table[t];
        if (ct.vendor_guid.eql(uefi.tables.ConfigurationTable.acpi_20_table_guid)) bi.rsdp = @intFromPtr(ct.vendor_table);
    }

    // Framebuffer (optional: the kernel works on the serial console alone).
    var gopp: ?*anyopaque = null;
    if (bs.locateProtocol(&uefi.protocol.GraphicsOutput.guid, null, &gopp) == .success) {
        const gop: *const uefi.protocol.GraphicsOutput = @ptrCast(@alignCast(gopp.?));
        bi.fb_base = gop.mode.frame_buffer_base;
        bi.fb_size = gop.mode.frame_buffer_size;
        bi.fb_width = gop.mode.info.horizontal_resolution;
        bi.fb_height = gop.mode.info.vertical_resolution;
        bi.fb_stride = gop.mode.info.pixels_per_scan_line;
        bi.fb_format = @intFromEnum(gop.mode.info.pixel_format);
    }

    // Page tables. The highest physical address decides the direct map's size.
    const pml4 = table();
    var mmap_size: usize = 0;
    var map_key: usize = 0;
    var desc_size: usize = 0;
    var desc_ver: u32 = 0;
    _ = bs.getMemoryMap(&mmap_size, null, &map_key, &desc_size, &desc_ver);
    mmap_size += 64 * desc_size; // room for the allocations below
    const mmap_buf = allocPages((mmap_size + 4095) / 4096);
    var top: u64 = 4 << 30;
    {
        var sz = mmap_size;
        check(bs.getMemoryMap(&sz, @ptrCast(@alignCast(mmap_buf)), &map_key, &desc_size, &desc_ver), "getMemoryMap");
        var off: usize = 0;
        while (off < sz) : (off += desc_size) {
            const d: *const uefi.tables.MemoryDescriptor = @ptrCast(@alignCast(mmap_buf + off));
            top = @max(top, d.physical_start + d.number_of_pages * 4096);
        }
    }
    const top2 = (top + (1 << 30) - 1) & ~@as(u64, (1 << 30) - 1);
    map2M(pml4, 0, 0, 4 << 30); // identity: the next few instructions run here
    map2M(pml4, DIRECT_MAP, 0, top2);
    map2M(pml4, vstart, kstart, span);

    // Final memory map, then exit boot services (retry once if the map changed).
    var tries: u32 = 0;
    while (true) : (tries += 1) {
        var sz = mmap_size;
        check(bs.getMemoryMap(&sz, @ptrCast(@alignCast(mmap_buf)), &map_key, &desc_size, &desc_ver), "getMemoryMap");
        var n: u64 = 0;
        var off: usize = 0;
        while (off < sz and n < MAX_REGIONS) : (off += desc_size) {
            const d: *const uefi.tables.MemoryDescriptor = @ptrCast(@alignCast(mmap_buf + off));
            const kind: u32 = switch (d.type) {
                .conventional_memory, .boot_services_code, .boot_services_data => 1,
                .acpi_reclaim_memory => 3,
                .acpi_memory_nvs => 4,
                .loader_code, .loader_data => 5,
                else => 2,
            };
            const base = d.physical_start;
            const len = d.number_of_pages * 4096;
            // Merge with the previous region when contiguous and of the same kind.
            if (n > 0 and bi.regions[n - 1].kind == kind and bi.regions[n - 1].base + bi.regions[n - 1].len == base) {
                bi.regions[n - 1].len += len;
            } else {
                bi.regions[n] = .{ .base = base, .len = len, .kind = kind };
                n += 1;
            }
        }
        bi.region_count = n;
        const s = bs.exitBootServices(uefi.handle, map_key);
        if (s == .success) break;
        if (tries > 2) fail("exitBootServices", s);
    }

    // No more boot services. Switch to our page tables and enter the kernel.
    asm volatile ("mov %[p], %%cr3"
        :
        : [p] "r" (@intFromPtr(pml4)),
        : "memory"
    );
    const entry: *const fn (u64) callconv(.{ .x86_64_sysv = .{} }) noreturn = @ptrFromInt(eh.entry);
    entry(@intFromPtr(bi));
}
