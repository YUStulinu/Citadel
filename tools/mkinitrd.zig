//! mkinitrd: packs a directory tree into a newc cpio archive (the format the kernel unpacks
//! into its tmpfs root). Entries are sorted so the output is reproducible.
//! Usage: mkinitrd ROOT_DIR OUT.cpio
const std = @import("std");

const Entry = struct {
    path: []const u8,
    kind: std.fs.File.Kind,
};

fn lessThan(_: void, a: Entry, b: Entry) bool {
    return std.mem.lessThan(u8, a.path, b.path);
}

fn header(w: anytype, ino: u32, mode: u32, nlink: u32, size: u32, name: []const u8) !void {
    var buf: [110]u8 = undefined;
    const fields = [_]u32{ ino, mode, 0, 0, nlink, 0, size, 0, 0, 0, 0, @intCast(name.len + 1), 0 };
    @memcpy(buf[0..6], "070701");
    for (fields, 0..) |f, i| {
        _ = std.fmt.bufPrint(buf[6 + i * 8 ..][0..8], "{x:0>8}", .{f}) catch unreachable;
    }
    try w.writeAll(&buf);
    try w.writeAll(name);
    try w.writeByte(0);
    try pad(w, 110 + name.len + 1);
}

fn pad(w: anytype, n: usize) !void {
    const zeros = [_]u8{ 0, 0, 0, 0 };
    try w.writeAll(zeros[0 .. (4 - n % 4) % 4]);
}

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const alloc = gpa.allocator();
    const args = try std.process.argsAlloc(alloc);
    if (args.len != 3) {
        std.debug.print("usage: mkinitrd ROOT_DIR OUT.cpio\n", .{});
        std.process.exit(2);
    }
    var root = try std.fs.cwd().openDir(args[1], .{ .iterate = true });
    defer root.close();

    var entries = std.ArrayList(Entry).init(alloc);
    var walker = try root.walk(alloc);
    while (try walker.next()) |e| {
        try entries.append(.{ .path = try alloc.dupe(u8, e.path), .kind = e.kind });
    }
    std.mem.sort(Entry, entries.items, {}, lessThan);

    const out = try std.fs.cwd().createFile(args[2], .{});
    defer out.close();
    var bw = std.io.bufferedWriter(out.writer());
    const w = bw.writer();

    var ino: u32 = 1;
    var files: usize = 0;
    var bytes: usize = 0;
    for (entries.items) |e| {
        ino += 1;
        switch (e.kind) {
            .directory => {
                const st = try root.statFile(e.path);
                try header(w, ino, 0o040000 | @as(u32, @intCast(st.mode & 0o7777)), 2, 0, e.path);
            },
            .sym_link => {
                var tbuf: [std.fs.max_path_bytes]u8 = undefined;
                const target = try root.readLink(e.path, &tbuf);
                try header(w, ino, 0o120777, 1, @intCast(target.len), e.path);
                try w.writeAll(target);
                try pad(w, target.len);
            },
            .file => {
                const st = try root.statFile(e.path);
                const data = try root.readFileAlloc(alloc, e.path, 1 << 30);
                defer alloc.free(data);
                try header(w, ino, 0o100000 | @as(u32, @intCast(st.mode & 0o7777)), 1, @intCast(data.len), e.path);
                try w.writeAll(data);
                try pad(w, data.len);
                files += 1;
                bytes += data.len;
            },
            else => std.debug.print("mkinitrd: skipping {s} ({s})\n", .{ e.path, @tagName(e.kind) }),
        }
    }
    try header(w, 0, 0, 1, 0, "TRAILER!!!");
    try bw.flush();
    std.debug.print("mkinitrd: {d} entries, {d} files, {d} KiB of data\n", .{ entries.items.len, files, bytes / 1024 });
}
