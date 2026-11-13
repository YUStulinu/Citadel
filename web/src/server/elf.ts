// Function symbols from an ELF64 little-endian file (.symtab + its string table), for turning
// the profiler's instruction addresses into names.
import { demangle } from "./demangle";

export interface Symbol {
  addr: bigint;
  size: bigint;
  name: string;
}

export class SymbolTable {
  readonly syms: Symbol[];

  constructor(syms: Symbol[]) {
    this.syms = syms.slice().sort((a, b) => (a.addr < b.addr ? -1 : a.addr > b.addr ? 1 : 0));
  }

  static fromElf(buf: Buffer): SymbolTable {
    if (buf.length < 64 || buf.readUInt32BE(0) !== 0x7f454c46 || buf[4] !== 2 || buf[5] !== 1) {
      throw new Error("not a little-endian ELF64 file");
    }
    const shoff = Number(buf.readBigUInt64LE(0x28));
    const shentsize = buf.readUInt16LE(0x3a);
    const shnum = buf.readUInt16LE(0x3c);
    const sections = [];
    for (let i = 0; i < shnum; i++) {
      const o = shoff + i * shentsize;
      sections.push({
        type: buf.readUInt32LE(o + 4),
        offset: Number(buf.readBigUInt64LE(o + 0x18)),
        size: Number(buf.readBigUInt64LE(o + 0x20)),
        link: buf.readUInt32LE(o + 0x28),
        entsize: Number(buf.readBigUInt64LE(o + 0x38)),
      });
    }
    const syms: Symbol[] = [];
    for (const s of sections) {
      if (s.type !== 2 /* SHT_SYMTAB */) continue;
      const strtab = sections[s.link];
      const ent = s.entsize || 24;
      for (let o = s.offset; o + ent <= s.offset + s.size; o += ent) {
        const nameOff = buf.readUInt32LE(o);
        const info = buf[o + 4];
        const value = buf.readBigUInt64LE(o + 8);
        const size = buf.readBigUInt64LE(o + 16);
        // Functions, and untyped labels (the assembly entry points: trap stubs, switch_context).
        const type = info & 0xf;
        if ((type !== 2 /* STT_FUNC */ && type !== 0 /* STT_NOTYPE */) || value === 0n) continue;
        let end = strtab.offset + nameOff;
        while (buf[end] !== 0) end++;
        const raw = buf.toString("utf8", strtab.offset + nameOff, end);
        if (raw === "" || raw.startsWith(".") || raw.startsWith("$")) continue;
        syms.push({ addr: value, size, name: demangle(raw) });
      }
    }
    return new SymbolTable(syms);
  }

  /** The function containing `addr` ("?" plus the address when unknown). */
  lookup(addr: bigint): string {
    let lo = 0;
    let hi = this.syms.length - 1;
    let best = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (this.syms[mid].addr <= addr) {
        best = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    if (best >= 0) {
      const s = this.syms[best];
      if (s.size === 0n || addr < s.addr + s.size) return s.name;
    }
    return "0x" + addr.toString(16);
  }
}
