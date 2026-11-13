// Rust symbol demangling for the profiler view: the v0 scheme (current rustc, "_R...") and the
// legacy scheme ("_ZN...E"). Crate disambiguators and hashes are left out, so names read like
// source code: <kernel::fs::tmpfs::TmpInode as kernel::fs::Inode>::readlink.

// ------------------------------------------------------------------ legacy

const ESCAPES: Record<string, string> = { SP: "@", BP: "*", RF: "&", LT: "<", GT: ">", LP: "(", RP: ")", C: "," };

function unescape(ident: string): string {
  // A leading "_" only protects an identifier that would start with "$".
  let out = ident.startsWith("_$") ? ident.slice(1) : ident;
  out = out.replace(/\$([A-Z]+|u[0-9a-f]+)\$/g, (m, code: string) => {
    if (code in ESCAPES) return ESCAPES[code];
    if (code.startsWith("u")) return String.fromCharCode(parseInt(code.slice(1), 16));
    return m;
  });
  return out.replace(/\.\./g, "::");
}

function demangleLegacy(s: string): string | null {
  if (!s.startsWith("_ZN") || !s.endsWith("E")) return null;
  let i = 3;
  const parts: string[] = [];
  while (i < s.length - 1) {
    let j = i;
    while (j < s.length && s[j] >= "0" && s[j] <= "9") j++;
    if (j === i) return null;
    const len = Number(s.slice(i, j));
    const ident = s.slice(j, j + len);
    if (ident.length !== len) return null;
    parts.push(ident);
    i = j + len;
  }
  if (parts.length > 1 && /^h[0-9a-f]{16}$/.test(parts[parts.length - 1])) parts.pop();
  return parts.map(unescape).join("::");
}

// ------------------------------------------------------------------ v0

const BASIC: Record<string, string> = {
  a: "i8", b: "bool", c: "char", d: "f64", e: "str", f: "f32", h: "u8", i: "isize", j: "usize", l: "i32", m: "u32",
  n: "i128", o: "u128", s: "i16", t: "u16", u: "()", v: "...", x: "i64", y: "u64", z: "!", p: "_",
};

class Fail extends Error {}

class V0 {
  private depth = 0;
  constructor(
    private s: string,
    private pos = 0,
  ) {}

  private peek(): string {
    return this.s[this.pos] ?? "";
  }
  private next(): string {
    if (this.pos >= this.s.length) throw new Fail();
    return this.s[this.pos++];
  }
  private eat(c: string): boolean {
    if (this.peek() === c) {
      this.pos++;
      return true;
    }
    return false;
  }

  private base62(): number {
    if (this.eat("_")) return 0;
    let v = 0;
    for (;;) {
      const c = this.next();
      if (c === "_") return v + 1;
      const d = c >= "0" && c <= "9" ? c.charCodeAt(0) - 48 : c >= "a" && c <= "z" ? c.charCodeAt(0) - 87 : c >= "A" && c <= "Z" ? c.charCodeAt(0) - 29 : -1;
      if (d < 0) throw new Fail();
      v = v * 62 + d;
    }
  }

  private optBase62(tag: string): number {
    return this.eat(tag) ? this.base62() + 1 : 0;
  }

  private decimal(): number {
    const start = this.pos;
    while (this.peek() >= "0" && this.peek() <= "9") this.pos++;
    if (start === this.pos) throw new Fail();
    return Number(this.s.slice(start, this.pos));
  }

  private ident(): { name: string; dis: number } {
    const dis = this.optBase62("s");
    const puny = this.eat("u");
    const len = this.decimal();
    this.eat("_");
    const name = this.s.slice(this.pos, this.pos + len);
    if (name.length !== len) throw new Fail();
    this.pos += len;
    return { name: puny ? `{punycode:${name}}` : name, dis };
  }

  /** Runs `f` at a back-referenced position, then returns to where we were. */
  private backref<T>(f: () => T): T {
    const at = this.base62();
    if (at >= this.pos) throw new Fail();
    const saved = this.pos;
    this.pos = at;
    if (++this.depth > 200) throw new Fail();
    const r = f();
    this.depth--;
    this.pos = saved;
    return r;
  }

  path(value: boolean): string {
    if (++this.depth > 200) throw new Fail();
    try {
      const tag = this.next();
      switch (tag) {
        case "C":
          return this.ident().name;
        case "M": {
          this.optBase62("s");
          this.path(false); // where the impl is: not shown
          return `<${this.type()}>`;
        }
        case "X": {
          this.optBase62("s");
          this.path(false);
          const t = this.type();
          return `<${t} as ${this.path(false)}>`;
        }
        case "Y": {
          const t = this.type();
          return `<${t} as ${this.path(false)}>`;
        }
        case "N": {
          const ns = this.next();
          const prefix = this.path(value);
          const id = this.ident();
          if (ns >= "A" && ns <= "Z") {
            const kind = ns === "C" ? "closure" : ns === "S" ? "shim" : ns;
            return `${prefix}::{${kind}${id.name ? ":" + id.name : ""}#${id.dis}}`;
          }
          return id.name ? `${prefix}::${id.name}` : prefix;
        }
        case "I": {
          const p = this.path(value);
          const args: string[] = [];
          while (!this.eat("E")) {
            const a = this.genericArg();
            if (a !== null) args.push(a);
          }
          return args.length ? `${p}${value ? "::" : ""}<${args.join(", ")}>` : p;
        }
        case "B":
          return this.backref(() => this.path(value));
        default:
          throw new Fail();
      }
    } finally {
      this.depth--;
    }
  }

  private genericArg(): string | null {
    if (this.eat("L")) {
      this.base62();
      return null; // lifetimes are not shown
    }
    if (this.eat("K")) return this.konst();
    return this.type();
  }

  private konst(): string {
    if (this.eat("p")) return "_";
    if (this.eat("B")) return this.backref(() => this.konst());
    const t = this.next();
    const neg = this.eat("n");
    const start = this.pos;
    while (this.peek() !== "_") this.next();
    const hex = this.s.slice(start, this.pos);
    this.next();
    const v = hex ? BigInt("0x" + hex) : 0n;
    if (t === "b") return v ? "true" : "false";
    if (t === "c") return `'${String.fromCodePoint(Number(v))}'`;
    return (neg ? "-" : "") + v.toString();
  }

  private binder(): void {
    if (this.eat("G")) this.base62();
  }

  type(): string {
    if (++this.depth > 200) throw new Fail();
    try {
      const c = this.peek();
      if (BASIC[c]) {
        this.pos++;
        return BASIC[c];
      }
      if ("CMXYNI".includes(c)) return this.path(false);
      this.pos++;
      switch (c) {
        case "B":
          return this.backref(() => this.type());
        case "A": {
          const t = this.type();
          return `[${t}; ${this.konst()}]`;
        }
        case "S":
          return `[${this.type()}]`;
        case "R":
        case "Q": {
          if (this.eat("L")) this.base62();
          return (c === "R" ? "&" : "&mut ") + this.type();
        }
        case "P":
          return "*const " + this.type();
        case "O":
          return "*mut " + this.type();
        case "T": {
          const ts: string[] = [];
          while (!this.eat("E")) ts.push(this.type());
          return ts.length === 1 ? `(${ts[0]},)` : `(${ts.join(", ")})`;
        }
        case "F": {
          this.binder();
          const unsafe = this.eat("U");
          let abi = "";
          if (this.eat("K")) abi = this.eat("C") ? 'extern "C" ' : `extern "${this.ident().name}" `;
          const args: string[] = [];
          while (!this.eat("E")) args.push(this.type());
          const ret = this.type();
          return `${unsafe ? "unsafe " : ""}${abi}fn(${args.join(", ")})${ret === "()" ? "" : " -> " + ret}`;
        }
        case "D": {
          this.binder();
          const bounds: string[] = [];
          while (!this.eat("E")) {
            let b = this.path(false);
            while (this.eat("p")) {
              const name = this.ident().name;
              b += `<${name} = ${this.type()}>`;
            }
            bounds.push(b);
          }
          if (this.eat("L")) this.base62();
          return "dyn " + bounds.join(" + ");
        }
        default:
          throw new Fail();
      }
    } finally {
      this.depth--;
    }
  }
}

function demangleV0(sym: string): string | null {
  let s = sym.startsWith("__R") ? sym.slice(3) : sym.startsWith("_R") ? sym.slice(2) : null;
  if (s === null) return null;
  const dot = s.indexOf(".");
  if (dot >= 0) s = s.slice(0, dot); // .llvm.123... suffixes
  if (s[0] >= "0" && s[0] <= "9") return null; // an encoding version we do not know
  try {
    return new V0(s).path(true);
  } catch {
    return null;
  }
}

/** A readable name for a symbol (unchanged if it is not a Rust mangled name). */
export function demangle(sym: string): string {
  let s = sym;
  if (s.startsWith("__ZN")) s = s.slice(1);
  return demangleV0(s) ?? demangleLegacy(s.replace(/\.llvm\.\d+$/, "")) ?? sym;
}
