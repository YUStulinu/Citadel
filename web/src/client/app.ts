// Browser side: a terminal on Citadel's serial console and a live view of the kernel profiler.
import { Screen } from "../shared/vt.js";

const screen = new Screen(30, 100, 2000);
const term = document.getElementById("term") as HTMLPreElement;
const statusEl = document.getElementById("status") as HTMLSpanElement;
const chart = document.getElementById("chart") as HTMLCanvasElement;
const windowTable = document.getElementById("window") as HTMLTableSectionElement;
const totalTable = document.getElementById("total") as HTMLTableSectionElement;
const procTable = document.getElementById("procs") as HTMLTableSectionElement;
const split = document.getElementById("split") as HTMLDivElement;
const scaleEl = document.getElementById("scale") as HTMLSpanElement;

const COLORS = ["#2e3440", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#d7dae0"];

let dirty = false;
function render(): void {
  dirty = false;
  const rows: string[] = [];
  const lines = [...screen.scrollback.slice(-200), ...screen.lines];
  const cursorLine = lines.length - screen.rows + screen.row;
  lines.forEach((line, i) => {
    let html = "";
    let open = -2;
    line.forEach((cell, c) => {
      const color = cell.fg < 0 ? -1 : cell.fg % 8;
      if (color !== open) {
        if (open !== -2) html += "</span>";
        html += color < 0 ? "<span>" : `<span style="color:${COLORS[color]}">`;
        open = color;
      }
      const ch = cell.ch === "<" ? "&lt;" : cell.ch === ">" ? "&gt;" : cell.ch === "&" ? "&amp;" : cell.ch;
      html += i === cursorLine && c === screen.col ? `<b class="cursor">${ch}</b>` : ch;
    });
    rows.push(html + "</span>");
  });
  term.innerHTML = rows.join("\n");
  term.scrollTop = term.scrollHeight;
}

function schedule(): void {
  if (!dirty) {
    dirty = true;
    requestAnimationFrame(render);
  }
}

// ---------------------------------------------------------------- profile view
interface Point {
  kernel: number;
  user: number;
  idle: number;
}
const history: Point[] = [];

function drawChart(): void {
  const ctx = chart.getContext("2d")!;
  const w = (chart.width = chart.clientWidth * devicePixelRatio);
  const h = (chart.height = chart.clientHeight * devicePixelRatio);
  ctx.clearRect(0, 0, w, h);
  const n = 120;
  const pts = history.slice(-n);
  const bw = w / n;
  // Scale to the busiest moment shown (at least 10% of all CPUs' time).
  const share = (p: Point) => (p.kernel + p.user) / (p.kernel + p.user + p.idle || 1);
  const top = Math.max(0.1, ...pts.map(share));
  scaleEl.textContent = `scale: ${(top * 100).toFixed(0)}% of all CPUs`;
  pts.forEach((p, i) => {
    const total = (p.kernel + p.user + p.idle || 1) * top;
    const kh = (p.kernel / total) * h;
    const uh = (p.user / total) * h;
    const x = w - (pts.length - i) * bw;
    ctx.fillStyle = "#e5c07b";
    ctx.fillRect(x, h - kh, bw - 1, kh);
    ctx.fillStyle = "#61afef";
    ctx.fillRect(x, h - kh - uh, bw - 1, uh);
  });
}

function fill(tbody: HTMLTableSectionElement, rows: [string, number][], total: number): void {
  const max = Math.max(1, ...rows.map((r) => r[1]));
  tbody.innerHTML = rows
    .map(([name, n]) => {
      const pct = total > 0 ? ((100 * n) / total).toFixed(1) : "0.0";
      const esc = name.replace(/&/g, "&amp;").replace(/</g, "&lt;");
      return `<tr><td class="num">${pct}%</td><td class="bar"><i style="width:${(100 * n) / max}%"></i></td><td title="${esc}">${esc}</td></tr>`;
    })
    .join("");
}

interface ProfileMsg {
  ms: number;
  samples: number;
  idle: number;
  user: number;
  kernel: number;
  window: [string, number][];
  total: [string, number][];
  totalSamples: number;
  totalBusy: number;
  procs: [string, number][];
}

function onProfile(m: ProfileMsg): void {
  history.push({ kernel: m.kernel, user: m.user, idle: m.idle });
  if (history.length > 600) history.shift();
  const busy = m.samples ? (100 * (m.samples - m.idle)) / m.samples : 0;
  split.textContent = `t=${(m.ms / 1000).toFixed(1)} s  busy ${busy.toFixed(0)}%  kernel ${m.kernel}  user ${m.user}  idle ${m.idle}  (samples in the last 0.5 s, all CPUs)`;
  // Shares of busy time (samples that were not idle).
  fill(windowTable, m.window, m.samples - m.idle);
  fill(totalTable, m.total, m.totalBusy);
  fill(procTable, m.procs, m.samples - m.idle);
  drawChart();
}

// ---------------------------------------------------------------- connection and keyboard
let ws: WebSocket;
function connect(): void {
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => (statusEl.textContent = "connected");
  ws.onclose = () => {
    statusEl.textContent = "disconnected, retrying";
    setTimeout(connect, 1000);
  };
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data as string);
    if (m.t === "console") {
      screen.write(m.d);
      schedule();
    } else if (m.t === "profile") onProfile(m as ProfileMsg);
  };
}

function send(d: string): void {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ t: "input", d }));
}

const KEYS: Record<string, string> = {
  Enter: "\r",
  Backspace: "\x7f",
  Tab: "\t",
  Escape: "\x1b",
  ArrowUp: "\x1b[A",
  ArrowDown: "\x1b[B",
  ArrowRight: "\x1b[C",
  ArrowLeft: "\x1b[D",
};

term.addEventListener("keydown", (e) => {
  if (e.ctrlKey && e.key.length === 1 && /[a-z]/i.test(e.key)) {
    send(String.fromCharCode(e.key.toUpperCase().charCodeAt(0) - 64));
  } else if (KEYS[e.key]) {
    send(KEYS[e.key]);
  } else if (e.key.length === 1 && !e.metaKey) {
    send(e.key);
  } else return;
  e.preventDefault();
});
term.addEventListener("paste", (e) => {
  send((e.clipboardData?.getData("text") ?? "").replace(/\n/g, "\r"));
  e.preventDefault();
});

connect();
term.focus();
window.addEventListener("resize", drawChart);
