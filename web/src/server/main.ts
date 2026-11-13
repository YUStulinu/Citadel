// The web console server: starts Citadel in QEMU, connects its first serial port (the console)
// and its second (the profiler stream) to browsers over WebSocket, and names the kernel
// addresses in the profile using the kernel's own symbol table.
//
//   node dist/node/src/server/main.js [--image build/citadel.img] [--kernel ELF] [--port 8090]
//                                     [--host 127.0.0.1] [--record FILE] [--script "cmd;;cmd"]
//   node dist/node/src/server/main.js --replay FILE     (plays a recorded session, no QEMU)
//
// Boot the image with "profile=2" on the kernel command line (or run `profctl stream` in the
// guest) to get the live profile.
import { spawn, ChildProcess } from "child_process";
import { appendFileSync, existsSync, readFileSync, writeFileSync } from "fs";
import { createServer } from "http";
import { connect, Socket } from "net";
import { join, resolve, extname } from "path";
import { byFunction, kernelSamples, parseProfileLine } from "../shared/profile";
import { SymbolTable } from "./elf";
import { upgrade, WsClient } from "./ws";

function arg(name: string, def: string): string {
  const i = process.argv.indexOf("--" + name);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : def;
}

const root = resolve(__dirname, "../../../..");
const repo = resolve(root, "..");
const image = arg("image", join(repo, "build/citadel.img"));
const kernelElf = arg("kernel", join(repo, "build/target/x86_64-unknown-none/release/kernel"));
const port = Number(arg("port", "8090"));
const host = arg("host", "127.0.0.1");
const com2Path = arg("com2", `/tmp/citadel-com2-${process.pid}.sock`);
const recordFile = arg("record", "");
const replayFile = arg("replay", "");
const script = arg("script", "").split(";;").map((c) => c.trim()).filter((c) => c.length > 0);
const scriptDelay = Number(arg("script-delay", "4000"));
const t0 = Date.now();
if (recordFile) writeFileSync(recordFile, "");

/** Session recording: one JSON object per line, {at: ms, k: "c" (console) | "p" (profile), d}. */
function record(k: string, d: string): void {
  if (recordFile) appendFileSync(recordFile, JSON.stringify({ at: Date.now() - t0, k, d }) + "\n");
}

let symbols = new SymbolTable([]);
if (existsSync(kernelElf)) {
  symbols = SymbolTable.fromElf(readFileSync(kernelElf));
  console.log(`web: ${symbols.syms.length} kernel symbols from ${kernelElf}`);
} else {
  console.log(`web: no kernel ELF at ${kernelElf}; addresses will not be named`);
}

const clients = new Set<WsClient>();
let backlog = "";
const totals = new Map<string, number>();
let totalSamples = 0;
let totalBusy = 0;

function broadcast(msg: object): void {
  const s = JSON.stringify(msg);
  for (const c of clients) c.send(s);
}

// ---------------------------------------------------------------- QEMU
let qemu: ChildProcess | null = null;
function startQemu(): void {
  const args = [join(repo, "tools/qemu-run.sh"), image, "-serial", `unix:${com2Path},server=on,wait=off`];
  qemu = spawn("sh", args, { stdio: ["pipe", "pipe", "inherit"], env: process.env });
  qemu.stdout!.on("data", (d: Buffer) => onConsole(d.toString("latin1")));
  qemu.on("exit", (code) => {
    broadcast({ t: "console", d: `\r\n[qemu exited with status ${code}]\r\n` });
    qemu = null;
  });
  setTimeout(connectCom2, 500);
}

let scriptStarted = false;
function onConsole(text: string): void {
  record("c", text);
  backlog = (backlog + text).slice(-65536);
  broadcast({ t: "console", d: text });
  // Scripted session: type the commands once the shell prompt appears.
  if (script.length > 0 && !scriptStarted && backlog.includes("citadel:")) {
    scriptStarted = true;
    const enter = String.fromCharCode(13);
    script.forEach((cmd, i) => setTimeout(() => qemu?.stdin!.write(cmd + enter), 1000 + i * scriptDelay));
  }
}

function connectCom2(): void {
  if (!qemu) return;
  const s: Socket = connect(com2Path);
  let pending = "";
  s.on("data", (d: Buffer) => {
    pending += d.toString("latin1");
    let nl: number;
    while ((nl = pending.indexOf("\n")) >= 0) {
      const line = pending.slice(0, nl);
      pending = pending.slice(nl + 1);
      onProfileLine(line);
    }
  });
  s.on("error", () => setTimeout(connectCom2, 500));
}

function onProfileLine(line: string): void {
  record("p", line);
  const w = parseProfileLine(line);
  if (!w) return;
  const funcs = byFunction(w, (rip) => symbols.lookup(rip));
  for (const [name, n] of funcs) totals.set(name, (totals.get(name) ?? 0) + n);
  totalSamples += w.samples;
  totalBusy += w.samples - w.idle;
  const top = (m: Map<string, number>, n: number) =>
    [...m.entries()].sort((a, b) => b[1] - a[1]).slice(0, n);
  broadcast({
    t: "profile",
    ms: w.ms,
    samples: w.samples,
    idle: w.idle,
    user: w.user,
    kernel: kernelSamples(w),
    window: top(funcs, 25),
    total: top(totals, 25),
    totalSamples,
    totalBusy,
    procs: w.procs.map((p) => [`${p.name} (${p.pid})`, p.count]),
  });
}

// ---------------------------------------------------------------- HTTP + WebSocket
const types: Record<string, string> = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript" };
const server = createServer((req, res) => {
  let path = (req.url ?? "/").split("?")[0];
  if (path === "/") path = "/index.html";
  const file = path.startsWith("/client/") ? join(root, "dist", path) : join(root, "static", path);
  if (!file.startsWith(root) || !existsSync(file)) {
    res.writeHead(404).end("not found\n");
    return;
  }
  res.writeHead(200, { "Content-Type": types[extname(file)] ?? "application/octet-stream" });
  res.end(readFileSync(file));
});

server.on("upgrade", (req, sock) => {
  if (req.url !== "/ws" || !upgrade(req, sock)) {
    sock.destroy();
    return;
  }
  const c: WsClient = new WsClient(
    sock,
    (text) => {
      try {
        const m = JSON.parse(text);
        if (m.t === "input" && typeof m.d === "string" && qemu) qemu.stdin!.write(m.d);
        if (m.t === "restart" && !qemu) startQemu();
      } catch {
        /* ignore malformed messages */
      }
    },
    () => clients.delete(c),
  );
  clients.add(c);
  c.send(JSON.stringify({ t: "console", d: backlog }));
});

server.listen(port, host, () => {
  console.log(`web: console on http://${host}:${port}/`);
  if (replayFile) replay(replayFile);
  else startQemu();
});

/** Plays a recorded session with its original timing. */
function replay(file: string): void {
  const events = readFileSync(file, "utf8")
    .split(/\r?\n/)
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l) as { at: number; k: string; d: string });
  for (const e of events) {
    setTimeout(() => (e.k === "c" ? onConsole(e.d) : onProfileLine(e.d)), e.at);
  }
  console.log(`web: replaying ${events.length} events from ${file}`);
}

process.on("SIGINT", () => {
  qemu?.kill();
  process.exit(0);
});
