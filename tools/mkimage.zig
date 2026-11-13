//! mkimage: builds Citadel's bootable disk image from two directories.
//!
//!   mkimage OUT.img ESP_DIR ROOT_DIR [ESP_MiB] [ROOT_MiB]
//!
//! The image is a GPT disk with
//!   1. an EFI System Partition formatted as FAT32 (512-byte clusters), holding ESP_DIR
//!      (EFI/BOOT/BOOTX64.EFI and CITADEL/{KERNEL.ELF,INITRD,CMDLINE}); names must be 8.3;
//!   2. a "Linux filesystem" partition formatted as ext2 (revision 1, 4 KiB blocks, FILETYPE,
//!      SPARSE_SUPER, LARGE_FILE), holding ROOT_DIR (files, directories, symbolic links).
//! The output is deterministic: no timestamps or random identifiers depend on the build time.
const std = @import("std");
const Allocator = std.mem.Allocator;

const SECTOR = 512;
const MiB = 1024 * 1024;

fn put16(b: []u8, o: usize, v: u16) void {
    std.mem.writeInt(u16, b[o..][0..2], v, .little);
}
fn put32(b: []u8, o: usize, v: u32) void {
    std.mem.writeInt(u32, b[o..][0..4], v, .little);
}
fn put64(b: []u8, o: usize, v: u64) void {
    std.mem.writeInt(u64, b[o..][0..8], v, .little);
}

// ------------------------------------------------------------------ directory trees

const Kind = enum { file, dir, symlink };

const Node = struct {
    name: []const u8,
    kind: Kind,
    mode: u32,
    data: []const u8 = "", // file contents or link target
    children: std.ArrayList(*Node),
};

fn lessByName(_: void, a: *Node, b: *Node) bool {
    return std.mem.lessThan(u8, a.name, b.name);
}

fn loadTree(alloc: Allocator, dir: std.fs.Dir, name: []const u8) !*Node {
    const node = try alloc.create(Node);
    node.* = .{ .name = name, .kind = .dir, .mode = 0o755, .children = std.ArrayList(*Node).init(alloc) };
    var it = dir.iterate();
    while (try it.next()) |e| {
        const child_name = try alloc.dupe(u8, e.name);
        switch (e.kind) {
            .directory => {
                var sub = try dir.openDir(e.name, .{ .iterate = true });
                defer sub.close();
                const c = try loadTree(alloc, sub, child_name);
                const st = try dir.statFile(e.name);
                c.mode = @intCast(st.mode & 0o7777);
                try node.children.append(c);
            },
            .file => {
                const st = try dir.statFile(e.name);
                const c = try alloc.create(Node);
                c.* = .{ .name = child_name, .kind = .file, .mode = @intCast(st.mode & 0o7777), .data = try dir.readFileAlloc(alloc, e.name, 1 << 30), .children = std.ArrayList(*Node).init(alloc) };
                try node.children.append(c);
            },
            .sym_link => {
                var buf: [std.fs.max_path_bytes]u8 = undefined;
                const target = try dir.readLink(e.name, &buf);
                const c = try alloc.create(Node);
                c.* = .{ .name = child_name, .kind = .symlink, .mode = 0o777, .data = try alloc.dupe(u8, target), .children = std.ArrayList(*Node).init(alloc) };
                try node.children.append(c);
            },
            else => std.debug.print("mkimage: skipping {s}\n", .{e.name}),
        }
    }
    std.mem.sort(*Node, node.children.items, {}, lessByName);
    return node;
}

// ------------------------------------------------------------------ FAT32

const Fat = struct {
    img: []u8, // the whole partition
    fat: []u32,
    first_data_sector: u32,
    next_cluster: u32,
    clusters: u32,

    const RESERVED = 32;
    const EOC: u32 = 0x0fff_ffff;

    fn clusterOffset(self: *Fat, c: u32) usize {
        return (self.first_data_sector + (c - 2)) * SECTOR;
    }

    fn allocChain(self: *Fat, bytes: usize) !u32 {
        const n: u32 = @intCast(@max(1, (bytes + SECTOR - 1) / SECTOR));
        if (self.next_cluster + n > self.clusters + 2) return error.EspFull;
        const first = self.next_cluster;
        var i: u32 = 0;
        while (i < n) : (i += 1) {
            const c = first + i;
            self.fat[c] = if (i + 1 == n) EOC else c + 1;
        }
        self.next_cluster += n;
        return first;
    }

    fn shortName(name: []const u8) ![11]u8 {
        var out: [11]u8 = [_]u8{' '} ** 11;
        const dot = std.mem.lastIndexOfScalar(u8, name, '.');
        const base = if (dot) |d| name[0..d] else name;
        const ext = if (dot) |d| name[d + 1 ..] else "";
        if (base.len == 0 or base.len > 8 or ext.len > 3) return error.NotAn83Name;
        for (base, 0..) |ch, i| out[i] = std.ascii.toUpper(ch);
        for (ext, 0..) |ch, i| out[8 + i] = std.ascii.toUpper(ch);
        return out;
    }

    fn entry(buf: []u8, name: [11]u8, attr: u8, cluster: u32, size: u32) void {
        @memcpy(buf[0..11], &name);
        buf[11] = attr;
        put16(buf, 14, 0); // creation time
        put16(buf, 16, (46 << 9) | (1 << 5) | 1); // 2026-01-01
        put16(buf, 18, (46 << 9) | (1 << 5) | 1);
        put16(buf, 20, @intCast(cluster >> 16));
        put16(buf, 22, 0);
        put16(buf, 24, (46 << 9) | (1 << 5) | 1);
        put16(buf, 26, @intCast(cluster & 0xffff));
        put32(buf, 28, size);
    }

    /// Writes directory `node` (its cluster chain already allocated at `cluster`).
    fn writeDir(self: *Fat, node: *Node, cluster: u32, parent: u32) !void {
        const n = node.children.items.len + 2;
        var entries = try std.heap.page_allocator.alloc(u8, (n + 1) * 32);
        defer std.heap.page_allocator.free(entries);
        @memset(entries, 0);
        var i: usize = 0;
        if (cluster == 2) {
            // The volume label, matching the one in the boot sector.
            entry(entries[0..32], "CITADEL ESP".*, 0x08, 0, 0);
            i = 1;
        }
        if (cluster != 2) {
            entry(entries[0..32], ".          ".*, 0x10, cluster, 0);
            entry(entries[32..64], "..         ".*, 0x10, if (parent == 2) 0 else parent, 0);
            i = 2;
        }
        for (node.children.items) |c| {
            const name = try shortName(c.name);
            const e = entries[i * 32 ..][0..32];
            switch (c.kind) {
                .dir => {
                    const bytes = (c.children.items.len + 3) * 32;
                    const cc = try self.allocChain(bytes);
                    entry(e, name, 0x10, cc, 0);
                    try self.writeDir(c, cc, cluster);
                },
                .file => {
                    const cc = if (c.data.len == 0) 0 else try self.allocChain(c.data.len);
                    entry(e, name, 0x20, cc, @intCast(c.data.len));
                    if (cc != 0) self.writeChain(cc, c.data);
                },
                .symlink => return error.NoSymlinksOnFat,
            }
            i += 1;
        }
        self.writeChain(cluster, entries[0 .. i * 32]);
    }

    fn writeChain(self: *Fat, first: u32, data: []const u8) void {
        var c = first;
        var off: usize = 0;
        while (off < data.len) {
            const n = @min(SECTOR, data.len - off);
            const at = self.clusterOffset(c);
            @memcpy(self.img[at .. at + n], data[off .. off + n]);
            off += n;
            if (self.fat[c] == EOC) break;
            c = self.fat[c];
        }
    }
};

fn formatFat32(alloc: Allocator, img: []u8, start_lba: u64, root: *Node) !void {
    const total: u32 = @intCast(img.len / SECTOR);
    // Solve for the FAT size: clusters = (total - reserved - 2 * fatsz), fatsz covers them.
    var fatsz: u32 = 1;
    while (true) {
        const clusters = total - Fat.RESERVED - 2 * fatsz;
        const need = ((clusters + 2) * 4 + SECTOR - 1) / SECTOR;
        if (need <= fatsz) break;
        fatsz = need;
    }
    const clusters = total - Fat.RESERVED - 2 * fatsz;
    if (clusters < 65525) return error.EspTooSmallForFat32;
    @memset(img, 0);
    const bs = img[0..SECTOR];
    bs[0] = 0xeb;
    bs[1] = 0x58;
    bs[2] = 0x90;
    @memcpy(bs[3..11], "CITADEL ");
    put16(bs, 11, SECTOR);
    bs[13] = 1; // sectors per cluster
    put16(bs, 14, Fat.RESERVED);
    bs[16] = 2; // FATs
    bs[21] = 0xf8;
    put16(bs, 24, 63);
    put16(bs, 26, 255);
    put32(bs, 28, @intCast(start_lba));
    put32(bs, 32, total);
    put32(bs, 36, fatsz);
    put32(bs, 44, 2); // root cluster
    put16(bs, 48, 1); // FSInfo sector
    put16(bs, 50, 6); // backup boot sector
    bs[64] = 0x80;
    bs[66] = 0x29;
    put32(bs, 67, 0x43495441); // volume id "CITA"
    @memcpy(bs[71..82], "CITADEL ESP");
    @memcpy(bs[82..90], "FAT32   ");
    bs[510] = 0x55;
    bs[511] = 0xaa;

    const fat = try alloc.alloc(u32, clusters + 2);
    @memset(fat, 0);
    fat[0] = 0x0fff_fff8;
    fat[1] = Fat.EOC;
    var f = Fat{ .img = img, .fat = fat, .first_data_sector = Fat.RESERVED + 2 * fatsz, .next_cluster = 2, .clusters = clusters };
    const root_bytes = (root.children.items.len + 2) * 32;
    const rc = try f.allocChain(root_bytes);
    std.debug.assert(rc == 2);
    try f.writeDir(root, 2, 0);

    // FSInfo, backups, and the two FATs.
    const fsinfo = img[SECTOR .. 2 * SECTOR];
    put32(fsinfo, 0, 0x41615252);
    put32(fsinfo, 484, 0x61417272);
    put32(fsinfo, 488, clusters + 2 - f.next_cluster);
    put32(fsinfo, 492, f.next_cluster);
    put32(fsinfo, 508, 0xaa550000);
    @memcpy(img[6 * SECTOR .. 7 * SECTOR], img[0..SECTOR]);
    @memcpy(img[7 * SECTOR .. 8 * SECTOR], img[SECTOR .. 2 * SECTOR]);
    for (0..2) |k| {
        const base = (Fat.RESERVED + k * fatsz) * SECTOR;
        for (fat, 0..) |v, i| put32(img, base + i * 4, v);
    }
}

// ------------------------------------------------------------------ ext2

const Ext2 = struct {
    img: []u8,
    alloc: Allocator,
    block_size: u32 = 4096,
    blocks: u32,
    groups: u32,
    blocks_per_group: u32 = 32768,
    inodes_per_group: u32 = 2048,
    inode_size: u32 = 256,
    used_blocks: std.DynamicBitSet,
    used_inodes: std.DynamicBitSet,
    dirs_per_group: []u32,
    next_inode: u32 = 12, // 11 is lost+found
    next_block: u32 = 0,

    const S_IFDIR = 0o040000;
    const S_IFREG = 0o100000;
    const S_IFLNK = 0o120000;
    const TIME: u32 = 1767225600; // 2026-01-01, fixed for reproducible images

    fn hasSuper(g: u32) bool {
        if (g <= 1) return true;
        for ([_]u32{ 3, 5, 7 }) |p| {
            var x: u32 = p;
            while (x < g) x *= p;
            if (x == g) return true;
        }
        return false;
    }

    fn gdtBlocks(self: *Ext2) u32 {
        return (self.groups * 32 + self.block_size - 1) / self.block_size;
    }
    fn itableBlocks(self: *Ext2) u32 {
        return self.inodes_per_group * self.inode_size / self.block_size;
    }
    fn groupStart(self: *Ext2, g: u32) u32 {
        return g * self.blocks_per_group;
    }
    /// (block bitmap, inode bitmap, inode table) of a group.
    fn layout(self: *Ext2, g: u32) [3]u32 {
        var b = self.groupStart(g);
        if (hasSuper(g)) b += 1 + self.gdtBlocks();
        return .{ b, b + 1, b + 2 };
    }

    fn block(self: *Ext2, n: u32) []u8 {
        const at = @as(usize, n) * self.block_size;
        return self.img[at .. at + self.block_size];
    }

    fn allocBlock(self: *Ext2) !u32 {
        while (self.next_block < self.blocks and self.used_blocks.isSet(self.next_block)) self.next_block += 1;
        if (self.next_block >= self.blocks) return error.RootTooSmall;
        self.used_blocks.set(self.next_block);
        self.next_block += 1;
        return self.next_block - 1;
    }

    fn inodeBytes(self: *Ext2, ino: u32) []u8 {
        const g = (ino - 1) / self.inodes_per_group;
        const idx = (ino - 1) % self.inodes_per_group;
        const at = @as(usize, self.layout(g)[2]) * self.block_size + @as(usize, idx) * self.inode_size;
        return self.img[at .. at + self.inode_size];
    }

    /// Stores `data` in newly allocated blocks and fills the inode's block map (and i_blocks).
    fn writeData(self: *Ext2, ino: u32, data: []const u8) !void {
        const n = self.inodeBytes(ino);
        const bs = self.block_size;
        const nblocks: u32 = @intCast((data.len + bs - 1) / bs);
        const p = bs / 4;
        var count: u32 = 0; // blocks used, data and indirect
        var single: u32 = 0;
        var double: u32 = 0;
        var double_child: u32 = 0;
        var i: u32 = 0;
        while (i < nblocks) : (i += 1) {
            const b = try self.allocBlock();
            count += 1;
            const off = @as(usize, i) * bs;
            const piece = data[off..@min(data.len, off + bs)];
            @memcpy(self.block(b)[0..piece.len], piece);
            if (i < 12) {
                put32(n, 40 + 4 * i, b);
            } else if (i < 12 + p) {
                if (single == 0) {
                    single = try self.allocBlock();
                    count += 1;
                    put32(n, 40 + 4 * 12, single);
                }
                put32(self.block(single), (i - 12) * 4, b);
            } else if (i < 12 + p + p * p) {
                const k = i - 12 - p;
                if (double == 0) {
                    double = try self.allocBlock();
                    count += 1;
                    put32(n, 40 + 4 * 13, double);
                }
                if (k % p == 0) {
                    double_child = try self.allocBlock();
                    count += 1;
                    put32(self.block(double), (k / p) * 4, double_child);
                }
                put32(self.block(double_child), (k % p) * 4, b);
            } else return error.FileTooLargeForImage;
        }
        put32(n, 28, count * (bs / 512));
    }

    fn writeInode(self: *Ext2, ino: u32, mode: u32, links: u16, size: u64) void {
        const n = self.inodeBytes(ino);
        put16(n, 0, @intCast(mode));
        put32(n, 4, @truncate(size));
        put32(n, 8, TIME);
        put32(n, 12, TIME);
        put32(n, 16, TIME);
        put16(n, 26, links);
        if (mode & 0o170000 == S_IFREG) put32(n, 108, @intCast(size >> 32));
        self.used_inodes.set(ino - 1);
    }

    fn fileType(k: Kind) u8 {
        return switch (k) {
            .file => 1,
            .dir => 2,
            .symlink => 7,
        };
    }

    /// Builds the bytes of a directory: "." and ".." then the entries, packed into blocks.
    fn dirBytes(self: *Ext2, ino: u32, parent: u32, names: []const []const u8, inos: []const u32, types: []const u8) ![]u8 {
        var out = std.ArrayList(u8).init(self.alloc);
        var used: usize = 0; // bytes used in the current block
        var last: usize = 0; // offset of the last entry in the current block
        const all_names = try self.alloc.alloc([]const u8, names.len + 2);
        const all_inos = try self.alloc.alloc(u32, names.len + 2);
        const all_types = try self.alloc.alloc(u8, names.len + 2);
        all_names[0] = ".";
        all_names[1] = "..";
        all_inos[0] = ino;
        all_inos[1] = parent;
        all_types[0] = 2;
        all_types[1] = 2;
        @memcpy(all_names[2..], names);
        @memcpy(all_inos[2..], inos);
        @memcpy(all_types[2..], types);
        for (all_names, all_inos, all_types) |name, e_ino, t| {
            const rec: usize = (8 + name.len + 3) & ~@as(usize, 3);
            if (out.items.len == 0 or used + rec > self.block_size) {
                if (out.items.len != 0) {
                    // Stretch the previous block's last entry to the end of that block.
                    put16(out.items, last + 4, @intCast(out.items.len - last));
                }
                try out.appendNTimes(0, self.block_size);
                used = 0;
            }
            const at = out.items.len - self.block_size + used;
            put32(out.items, at, e_ino);
            put16(out.items, at + 4, @intCast(rec));
            out.items[at + 6] = @intCast(name.len);
            out.items[at + 7] = t;
            @memcpy(out.items[at + 8 .. at + 8 + name.len], name);
            last = at;
            used += rec;
        }
        put16(out.items, last + 4, @intCast(out.items.len - last));
        return out.items;
    }

    fn addTree(self: *Ext2, node: *Node, ino: u32, parent: u32) !void {
        const n = node.children.items.len;
        const inos = try self.alloc.alloc(u32, n);
        const names = try self.alloc.alloc([]const u8, n);
        const types = try self.alloc.alloc(u8, n);
        for (node.children.items, 0..) |c, i| {
            inos[i] = self.next_inode;
            self.next_inode += 1;
            if (self.next_inode > self.inodes_per_group * self.groups) return error.OutOfInodes;
            names[i] = c.name;
            types[i] = fileType(c.kind);
        }
        var subdirs: u16 = 0;
        for (node.children.items, 0..) |c, i| {
            switch (c.kind) {
                .file => {
                    self.writeInode(inos[i], S_IFREG | c.mode, 1, c.data.len);
                    try self.writeData(inos[i], c.data);
                },
                .symlink => {
                    self.writeInode(inos[i], S_IFLNK | 0o777, 1, c.data.len);
                    if (c.data.len < 60) {
                        @memcpy(self.inodeBytes(inos[i])[40 .. 40 + c.data.len], c.data);
                    } else try self.writeData(inos[i], c.data);
                },
                .dir => {
                    subdirs += 1;
                    try self.addTree(c, inos[i], ino);
                },
            }
        }
        // lost+found lives in the root directory.
        var all_names = std.ArrayList([]const u8).init(self.alloc);
        var all_inos = std.ArrayList(u32).init(self.alloc);
        var all_types = std.ArrayList(u8).init(self.alloc);
        if (ino == 2) {
            try all_names.append("lost+found");
            try all_inos.append(11);
            try all_types.append(2);
            subdirs += 1;
        }
        try all_names.appendSlice(names);
        try all_inos.appendSlice(inos);
        try all_types.appendSlice(types);
        const bytes = try self.dirBytes(ino, parent, all_names.items, all_inos.items, all_types.items);
        self.writeInode(ino, S_IFDIR | node.mode, 2 + subdirs, bytes.len);
        try self.writeData(ino, bytes);
        self.dirs_per_group[(ino - 1) / self.inodes_per_group] += 1;
    }

    fn format(alloc: Allocator, img: []u8, root: *Node) !void {
        const blocks: u32 = @intCast(img.len / 4096);
        var fs = Ext2{
            .img = img,
            .alloc = alloc,
            .blocks = blocks,
            .groups = (blocks + 32767) / 32768,
            .used_blocks = try std.DynamicBitSet.initEmpty(alloc, blocks),
            .used_inodes = undefined,
            .dirs_per_group = undefined,
        };
        if (blocks % fs.blocks_per_group != 0 and blocks % fs.blocks_per_group < 64) return error.LastGroupTooSmall;
        fs.used_inodes = try std.DynamicBitSet.initEmpty(alloc, fs.groups * fs.inodes_per_group);
        fs.dirs_per_group = try alloc.alloc(u32, fs.groups);
        @memset(fs.dirs_per_group, 0);
        @memset(img, 0);
        // Metadata blocks of every group are in use.
        var g: u32 = 0;
        while (g < fs.groups) : (g += 1) {
            var b = fs.groupStart(g);
            const end = fs.layout(g)[2] + fs.itableBlocks();
            while (b < end) : (b += 1) fs.used_blocks.set(b);
        }
        // Reserved inodes 1..10 are in use; 11 is lost+found.
        var r: u32 = 0;
        while (r < 10) : (r += 1) fs.used_inodes.set(r);
        const lf = try fs.dirBytes(11, 2, &.{}, &.{}, &.{});
        const lf_full = try alloc.alloc(u8, 4 * fs.block_size); // lost+found gets 16 KiB
        @memset(lf_full, 0);
        @memcpy(lf_full[0..lf.len], lf);
        var k: usize = 1;
        while (k < 4) : (k += 1) put16(lf_full, k * fs.block_size + 4, @intCast(fs.block_size));
        fs.writeInode(11, S_IFDIR | 0o700, 2, lf_full.len);
        try fs.writeData(11, lf_full);
        fs.dirs_per_group[0] += 1;
        try fs.addTree(root, 2, 2);
        try fs.writeMetadata();
    }

    fn writeMetadata(self: *Ext2) !void {
        var free_total: u32 = 0;
        var free_inodes_total: u32 = 0;
        const gdt = try self.alloc.alloc(u8, self.gdtBlocks() * self.block_size);
        @memset(gdt, 0);
        var g: u32 = 0;
        while (g < self.groups) : (g += 1) {
            const l = self.layout(g);
            const start = self.groupStart(g);
            const count = @min(self.blocks_per_group, self.blocks - start);
            // Block bitmap (bits past the end of a short last group are set).
            const bb = self.block(l[0]);
            var free: u32 = 0;
            var i: u32 = 0;
            while (i < self.blocks_per_group) : (i += 1) {
                const used = i >= count or self.used_blocks.isSet(start + i);
                if (used) bb[i / 8] |= @as(u8, 1) << @intCast(i % 8) else free += 1;
            }
            // Inode bitmap (bits past inodes_per_group, up to the block's end, are set).
            const ib = self.block(l[1]);
            var free_i: u32 = 0;
            i = 0;
            while (i < self.block_size * 8) : (i += 1) {
                const used = i >= self.inodes_per_group or self.used_inodes.isSet(g * self.inodes_per_group + i);
                if (used) ib[i / 8] |= @as(u8, 1) << @intCast(i % 8) else free_i += 1;
            }
            const d = gdt[g * 32 ..][0..32];
            put32(d, 0, l[0]);
            put32(d, 4, l[1]);
            put32(d, 8, l[2]);
            put16(d, 12, @intCast(free));
            put16(d, 14, @intCast(free_i));
            put16(d, 16, @intCast(self.dirs_per_group[g]));
            free_total += free;
            free_inodes_total += free_i;
        }
        // Superblock (primary at byte 1024 of block 0, backups at the start of sparse groups).
        var sb: [1024]u8 = [_]u8{0} ** 1024;
        put32(&sb, 0, self.groups * self.inodes_per_group);
        put32(&sb, 4, self.blocks);
        put32(&sb, 8, 0);
        put32(&sb, 12, free_total);
        put32(&sb, 16, free_inodes_total);
        put32(&sb, 20, 0); // first data block
        put32(&sb, 24, 2); // 4 KiB blocks
        put32(&sb, 28, 2);
        put32(&sb, 32, self.blocks_per_group);
        put32(&sb, 36, self.blocks_per_group);
        put32(&sb, 40, self.inodes_per_group);
        put32(&sb, 48, TIME);
        put16(&sb, 54, 0xffff); // max mount count: no forced checks
        put16(&sb, 56, 0xef53);
        put16(&sb, 58, 1); // clean
        put16(&sb, 60, 1); // continue on errors
        put32(&sb, 64, TIME);
        put32(&sb, 76, 1); // revision 1
        put32(&sb, 84, 11); // first non-reserved inode
        put16(&sb, 88, @intCast(self.inode_size));
        put32(&sb, 96, 0x2); // FILETYPE
        put32(&sb, 100, 0x1 | 0x2); // SPARSE_SUPER | LARGE_FILE
        const uuid = [16]u8{ 0xc1, 0x7a, 0xde, 0x10, 0x00, 0x40, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01 };
        @memcpy(sb[104..120], &uuid);
        @memcpy(sb[120..127], "citadel");
        var g2: u32 = 0;
        while (g2 < self.groups) : (g2 += 1) {
            if (!hasSuper(g2)) continue;
            const start = self.groupStart(g2);
            put16(&sb, 90, @intCast(g2)); // block group number of this copy
            const at: usize = if (g2 == 0) 1024 else @as(usize, start) * self.block_size;
            @memcpy(self.img[at .. at + 1024], &sb);
            const gat = (@as(usize, start) + 1) * self.block_size;
            @memcpy(self.img[gat .. gat + gdt.len], gdt);
        }
    }
};

// ------------------------------------------------------------------ GPT

const ESP_GUID = [16]u8{ 0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b };
const LINUX_GUID = [16]u8{ 0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4 };

fn gptEntry(e: []u8, type_guid: [16]u8, unique: u8, first: u64, last: u64, name: []const u8) void {
    @memcpy(e[0..16], &type_guid);
    const u = [16]u8{ 0xc1, 0x7a, 0xde, 0x10, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, unique };
    @memcpy(e[16..32], &u);
    put64(e, 32, first);
    put64(e, 40, last);
    for (name, 0..) |ch, i| put16(e, 56 + 2 * i, ch);
}

fn gptHeader(h: []u8, my: u64, alt: u64, last_usable: u64, entries_lba: u64, entries_crc: u32) void {
    @memset(h[0..SECTOR], 0);
    @memcpy(h[0..8], "EFI PART");
    put32(h, 8, 0x00010000);
    put32(h, 12, 92);
    put64(h, 24, my);
    put64(h, 32, alt);
    put64(h, 40, 34);
    put64(h, 48, last_usable);
    const disk_guid = [16]u8{ 0xc1, 0x7a, 0xde, 0x10, 0xd1, 0x5c, 0x40, 0x00, 0x80, 0x00, 0, 0, 0, 0, 0, 0x99 };
    @memcpy(h[56..72], &disk_guid);
    put64(h, 72, entries_lba);
    put32(h, 80, 128);
    put32(h, 84, 128);
    put32(h, 88, entries_crc);
    put32(h, 16, std.hash.Crc32.hash(h[0..92]));
}

pub fn main() !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const alloc = arena.allocator();
    const args = try std.process.argsAlloc(alloc);
    if (args.len < 4) {
        std.debug.print("usage: mkimage OUT.img ESP_DIR ROOT_DIR [ESP_MiB=64] [ROOT_MiB=128]\n", .{});
        std.process.exit(2);
    }
    const esp_mib: u64 = if (args.len > 4) try std.fmt.parseInt(u64, args[4], 10) else 64;
    const root_mib: u64 = if (args.len > 5) try std.fmt.parseInt(u64, args[5], 10) else 128;

    var esp_dir = try std.fs.cwd().openDir(args[2], .{ .iterate = true });
    defer esp_dir.close();
    var root_dir = try std.fs.cwd().openDir(args[3], .{ .iterate = true });
    defer root_dir.close();
    const esp_tree = try loadTree(alloc, esp_dir, "");
    const root_tree = try loadTree(alloc, root_dir, "");

    // Layout: 1 MiB for the GPT, the ESP, the root partition, then the backup GPT (33 sectors).
    const esp_first: u64 = MiB / SECTOR;
    const esp_last = esp_first + esp_mib * MiB / SECTOR - 1;
    const root_first = esp_last + 1;
    const root_last = root_first + root_mib * MiB / SECTOR - 1;
    const total_sectors = root_last + 1 + 33;

    const esp = try alloc.alloc(u8, esp_mib * MiB);
    try formatFat32(alloc, esp, esp_first, esp_tree);
    const rootfs = try alloc.alloc(u8, root_mib * MiB);
    try Ext2.format(alloc, rootfs, root_tree);

    var entries = [_]u8{0} ** (128 * 128);
    gptEntry(entries[0..128], ESP_GUID, 1, esp_first, esp_last, "EFI system");
    gptEntry(entries[128..256], LINUX_GUID, 2, root_first, root_last, "citadel-root");
    const ecrc = std.hash.Crc32.hash(&entries);

    var mbr = [_]u8{0} ** SECTOR;
    mbr[446 + 4] = 0xee; // protective partition
    put32(&mbr, 446 + 8, 1);
    put32(&mbr, 446 + 12, @intCast(@min(total_sectors - 1, 0xffffffff)));
    mbr[446 + 1] = 0;
    mbr[446 + 2] = 2;
    mbr[446 + 5] = 0xff;
    mbr[446 + 6] = 0xff;
    mbr[446 + 7] = 0xff;
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    var primary = [_]u8{0} ** SECTOR;
    var backup = [_]u8{0} ** SECTOR;
    const last_lba = total_sectors - 1;
    gptHeader(&primary, 1, last_lba, last_lba - 33, 2, ecrc);
    gptHeader(&backup, last_lba, 1, last_lba - 33, last_lba - 32, ecrc);

    const out = try std.fs.cwd().createFile(args[1], .{ .truncate = true });
    defer out.close();
    try out.setEndPos(total_sectors * SECTOR);
    try out.pwriteAll(&mbr, 0);
    try out.pwriteAll(&primary, SECTOR);
    try out.pwriteAll(&entries, 2 * SECTOR);
    try out.pwriteAll(esp, esp_first * SECTOR);
    try out.pwriteAll(rootfs, root_first * SECTOR);
    try out.pwriteAll(&entries, (last_lba - 32) * SECTOR);
    try out.pwriteAll(&backup, last_lba * SECTOR);
    std.debug.print("mkimage: {s}: {d} MiB (ESP {d} MiB FAT32, root {d} MiB ext2)\n", .{ args[1], total_sectors * SECTOR / MiB, esp_mib, root_mib });
}
