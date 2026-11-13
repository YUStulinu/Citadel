// A terminal screen model: the subset of VT100/ANSI that Citadel's console uses (printable
// text, CR, LF, BS, TAB, cursor movement, erase, SGR colours, scrolling).

export interface Cell {
  ch: string;
  fg: number; // -1 = default, 0..7 ANSI colours, +8 = bright
  bold: boolean;
}

export class Screen {
  readonly rows: number;
  readonly cols: number;
  lines: Cell[][];
  scrollback: Cell[][] = [];
  row = 0;
  col = 0;
  private fg = -1;
  private bold = false;
  private esc = "";
  private inEsc = false;
  readonly maxScrollback: number;

  constructor(rows = 25, cols = 80, maxScrollback = 1000) {
    this.rows = rows;
    this.cols = cols;
    this.maxScrollback = maxScrollback;
    this.lines = Array.from({ length: rows }, () => this.blankLine());
  }

  private blankLine(): Cell[] {
    return Array.from({ length: this.cols }, () => ({ ch: " ", fg: -1, bold: false }));
  }

  private newline(): void {
    this.row++;
    if (this.row >= this.rows) {
      const gone = this.lines.shift()!;
      this.scrollback.push(gone);
      if (this.scrollback.length > this.maxScrollback) this.scrollback.shift();
      this.lines.push(this.blankLine());
      this.row = this.rows - 1;
    }
  }

  write(text: string): void {
    for (const ch of text) this.put(ch);
  }

  private put(ch: string): void {
    if (this.inEsc) {
      this.esc += ch;
      if (this.esc === "[" || (this.esc.startsWith("[") && /[0-9;?]$/.test(this.esc))) return;
      this.inEsc = false;
      this.control(this.esc);
      this.esc = "";
      return;
    }
    switch (ch) {
      case "\x1b":
        this.inEsc = true;
        this.esc = "";
        return;
      case "\r":
        this.col = 0;
        return;
      case "\n":
        this.newline();
        return;
      case "\b":
        if (this.col > 0) this.col--;
        return;
      case "\t":
        this.col = Math.min(this.cols - 1, (this.col + 8) & ~7);
        return;
      case "\x07":
        return;
    }
    if (ch < " ") return;
    if (this.col >= this.cols) {
      this.col = 0;
      this.newline();
    }
    this.lines[this.row][this.col] = { ch, fg: this.fg, bold: this.bold };
    this.col++;
  }

  private control(seq: string): void {
    if (!seq.startsWith("[")) return; // only CSI sequences
    const final = seq[seq.length - 1];
    const params = seq.slice(1, -1).replace("?", "").split(";").filter((p) => p !== "").map(Number);
    const n = params[0] ?? 1;
    switch (final) {
      case "A":
        this.row = Math.max(0, this.row - n);
        break;
      case "B":
        this.row = Math.min(this.rows - 1, this.row + n);
        break;
      case "C":
        this.col = Math.min(this.cols - 1, this.col + n);
        break;
      case "D":
        this.col = Math.max(0, this.col - n);
        break;
      case "H":
      case "f":
        this.row = Math.min(this.rows - 1, Math.max(0, (params[0] ?? 1) - 1));
        this.col = Math.min(this.cols - 1, Math.max(0, (params[1] ?? 1) - 1));
        break;
      case "J": {
        const mode = params[0] ?? 0;
        if (mode === 2 || mode === 3) {
          this.lines = Array.from({ length: this.rows }, () => this.blankLine());
        } else if (mode === 0) {
          this.eraseLine(0);
          for (let r = this.row + 1; r < this.rows; r++) this.lines[r] = this.blankLine();
        }
        break;
      }
      case "K":
        this.eraseLine(params[0] ?? 0);
        break;
      case "m":
        if (params.length === 0) params.push(0);
        for (const p of params) {
          if (p === 0) {
            this.fg = -1;
            this.bold = false;
          } else if (p === 1) this.bold = true;
          else if (p === 22) this.bold = false;
          else if (p >= 30 && p <= 37) this.fg = p - 30;
          else if (p >= 90 && p <= 97) this.fg = p - 90 + 8;
          else if (p === 39) this.fg = -1;
        }
        break;
    }
  }

  private eraseLine(mode: number): void {
    const line = this.lines[this.row];
    const [from, to] = mode === 1 ? [0, this.col + 1] : mode === 2 ? [0, this.cols] : [this.col, this.cols];
    for (let c = from; c < to; c++) line[c] = { ch: " ", fg: -1, bold: false };
  }

  /** The visible text, one string per row, trailing blanks removed. */
  text(): string[] {
    return this.lines.map((l) => l.map((c) => c.ch).join("").replace(/\s+$/, ""));
  }
}
