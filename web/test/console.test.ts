import { test } from "node:test";
import assert from "node:assert/strict";
import { Screen } from "../src/shared/vt";
import { byFunction, kernelSamples, parseProfileLine } from "../src/shared/profile";
import { demangle } from "../src/server/demangle";
import { SymbolTable } from "../src/server/elf";
import { acceptKey, decodeFrames, encodeFrame } from "../src/server/ws";

test("terminal: text, CR/LF, backspace, wrapping and scrolling", () => {
  const s = new Screen(3, 10);
  s.write("hello\r\nwor\bld");
  assert.deepEqual(s.text(), ["hello", "wold", ""]);
  s.write("\r\n0123456789AB");
  assert.deepEqual(s.text(), ["wold", "0123456789", "AB"]);
  assert.equal(s.scrollback.length, 1);
  assert.equal(s.row, 2);
  assert.equal(s.col, 2);
});

test("terminal: cursor movement, erase, clear and colours", () => {
  const s = new Screen(4, 20);
  s.write("abcdef\x1b[3D\x1b[K!");
  assert.equal(s.text()[0], "abc!");
  s.write("\x1b[3;5Hxy");
  assert.equal(s.text()[2], "    xy");
  s.write("\x1b[1;31mred\x1b[0m");
  assert.equal(s.lines[2][6].fg, 1);
  assert.equal(s.lines[2][6].bold, true);
  assert.equal(s.lines[2][9].fg, -1);
  s.write("\x1b[H\x1b[2J");
  assert.deepEqual(s.text(), ["", "", "", ""]);
  assert.equal(s.row, 0);
});

test("profile lines parse and aggregate by function", () => {
  const w = parseProfileLine("P 1500 4000 3000 600 k:ffffffff81001000:300 k:ffffffff81001010:50 k:ffffffff81002000:20 u:3:httpd:600")!;
  assert.equal(w.ms, 1500);
  assert.equal(kernelSamples(w), 400);
  assert.equal(w.kernel.length, 3);
  assert.deepEqual(w.procs, [{ pid: 3, name: "httpd", count: 600 }]);
  const f = byFunction(w, (rip) => (rip < 0xffffffff81002000n ? "a" : "b"));
  assert.deepEqual([...f.entries()], [["a", 350], ["b", 20]]);
  assert.equal(parseProfileLine("hello"), null);
  assert.equal(parseProfileLine("P x y z w"), null);
});

test("Rust legacy demangling", () => {
  assert.equal(demangle("_ZN6kernel5sched8schedule17h0123456789abcdefE"), "kernel::sched::schedule");
  assert.equal(demangle("_ZN4core3ptr13drop_in_place17hfedcba9876543210E"), "core::ptr::drop_in_place");
  assert.equal(demangle("_ZN52_$LT$kernel..fs..File$u20$as$u20$core..ops..Drop$GT$4drop17h1111111111111111E"), "<kernel::fs::File as core::ops::Drop>::drop");
  assert.equal(demangle("memcpy"), "memcpy");
});

/** A minimal ELF64 file with a .symtab and .strtab holding the given functions. */
function makeElf(funcs: [string, bigint, bigint][]): Buffer {
  const strs = Buffer.from("\0" + funcs.map((f) => f[0] + "\0").join(""));
  const sym = Buffer.alloc(24 * (funcs.length + 1));
  let nameOff = 1;
  funcs.forEach(([name, addr, size], i) => {
    const o = 24 * (i + 1);
    sym.writeUInt32LE(nameOff, o);
    sym[o + 4] = 0x12; // global function
    sym.writeBigUInt64LE(addr, o + 8);
    sym.writeBigUInt64LE(size, o + 16);
    nameOff += name.length + 1;
  });
  const header = Buffer.alloc(64);
  header.writeUInt32BE(0x7f454c46, 0);
  header[4] = 2;
  header[5] = 1;
  const symOff = 64;
  const strOff = symOff + sym.length;
  const shOff = strOff + strs.length;
  header.writeBigUInt64LE(BigInt(shOff), 0x28);
  header.writeUInt16LE(64, 0x3a);
  header.writeUInt16LE(3, 0x3c);
  const sh = Buffer.alloc(64 * 3);
  // [1] .symtab, linked to [2] .strtab
  sh.writeUInt32LE(2, 64 + 4);
  sh.writeBigUInt64LE(BigInt(symOff), 64 + 0x18);
  sh.writeBigUInt64LE(BigInt(sym.length), 64 + 0x20);
  sh.writeUInt32LE(2, 64 + 0x28);
  sh.writeBigUInt64LE(24n, 64 + 0x38);
  sh.writeUInt32LE(3, 128 + 4);
  sh.writeBigUInt64LE(BigInt(strOff), 128 + 0x18);
  sh.writeBigUInt64LE(BigInt(strs.length), 128 + 0x20);
  return Buffer.concat([header, sym, strs, sh]);
}

test("ELF symbol table lookup", () => {
  const elf = makeElf([
    ["_ZN6kernel4main17h0000000000000000E", 0x1000n, 0x100n],
    ["memcpy", 0x2000n, 0x40n],
  ]);
  const t = SymbolTable.fromElf(elf);
  assert.equal(t.syms.length, 2);
  assert.equal(t.lookup(0x1000n), "kernel::main");
  assert.equal(t.lookup(0x10ffn), "kernel::main");
  assert.equal(t.lookup(0x1100n), "0x1100");
  assert.equal(t.lookup(0x2020n), "memcpy");
  assert.equal(t.lookup(0x500n), "0x500");
  assert.throws(() => SymbolTable.fromElf(Buffer.from("not an elf file at all, sorry..................................")));
});

test("WebSocket handshake key and frames", () => {
  // The example from RFC 6455, section 1.3.
  assert.equal(acceptKey("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
  for (const n of [0, 5, 125, 126, 1000, 70000]) {
    const payload = Buffer.alloc(n, 0x61);
    const { frames, used } = decodeFrames(encodeFrame(payload));
    assert.equal(used, encodeFrame(payload).length);
    assert.equal(frames.length, 1);
    assert.equal(frames[0].payload.length, n);
  }
  // A masked client frame ("Hello", from the RFC) followed by half of another frame.
  const masked = Buffer.from([0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58, 0x81]);
  const { frames, used } = decodeFrames(masked);
  assert.equal(frames[0].payload.toString(), "Hello");
  assert.equal(used, 11);
});

// Symbols from Citadel's kernel, with what binutils' c++filt prints for them (crate hashes
// removed, as this demangler leaves them out).
const V0_CASES: [string, string][] = [
  ["_RNvXs_NtNtCscUvu6BD0F24_6kernel2fs5tmpfsNtB4_8TmpInodeNtB6_5Inode8readlink", "<kernel::fs::tmpfs::TmpInode as kernel::fs::Inode>::readlink"],
  ["_RNCNvXs_NtCsbege3lwr2uJ_5kcore4cpioNtB6_6ReaderNtNtNtNtCsbQJWu2zlgTK_4core4iter6traits8iterator8Iterator4next0B8_", "<kcore::cpio::Reader as core::iter::traits::iterator::Iterator>::next::{closure#0}"],
  ["_RNvMs5_NtNtNtNtCsdFCTIYpBdc3_5alloc11collections5btree3map5entryINtB5_13OccupiedEntrytINtNtBd_4sync3ArcNtNtCscUvu6BD0F24_6kernel3net8ListenerEE9remove_kvB1H_", "<alloc::collections::btree::map::entry::OccupiedEntry<u16, alloc::sync::Arc<kernel::net::Listener>>>::remove_kv"],
  ["_RNvNtCscUvu6BD0F24_6kernel7profile6report", "kernel::profile::report"],
  ["_RINvMs4_NtCscUvu6BD0F24_6kernel5schedNtB6_9WaitQueue10wait_untilNCNvNtNtB8_3net6socket7syscalls_0EB8_", "<kernel::sched::WaitQueue>::wait_until::<kernel::net::socket::syscall::{closure#1}>"],
  ["_RINvMs_NtCsdFCTIYpBdc3_5alloc3vecINtB5_3VecNtNtCscUvu6BD0F24_6kernel2fs5MountE10retain_mutNCINvB2_6retainNCNvBH_5mount0E0EBJ_", "<alloc::vec::Vec<kernel::fs::Mount>>::retain_mut::<<alloc::vec::Vec<kernel::fs::Mount>>::retain<kernel::fs::mount::{closure#0}>::{closure#0}>"],
  ["_RNvXsm_NtNtCsbQJWu2zlgTK_4core3fmt3numtNtB7_8LowerHex3fmt", "<u16 as core::fmt::LowerHex>::fmt"],
  ["_RNvYNtNtNtCscUvu6BD0F24_6kernel2fs4pipe4FifoNtB6_5Inode7symlinkB8_", "<kernel::fs::pipe::Fifo as kernel::fs::Inode>::symlink"],
  ["_RNvNvNtNtCscUvu6BD0F24_6kernel2mm6paging18free_address_space10free_level.llvm.17043449304714190526", "kernel::mm::paging::free_address_space::free_level"],
  ["_RNvXs1g_NtCsbQJWu2zlgTK_4core3fmtRyNtB6_5Debug3fmtCscUvu6BD0F24_6kernel", "<&u64 as core::fmt::Debug>::fmt"],
  ["_RNCNvNtCscUvu6BD0F24_6kernel4proc4wait0B5_", "kernel::proc::wait::{closure#0}"],
];

test("Rust v0 demangling matches c++filt", () => {
  for (const [sym, want] of V0_CASES) assert.equal(demangle(sym), want, sym);
  assert.equal(demangle("_Rgarbage"), "_Rgarbage");
});
