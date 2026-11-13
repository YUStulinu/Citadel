// The profiler's wire format (one line on COM2 every 500 ms, see kernel/src/profile.rs):
//   P <ms> <samples> <idle> <user> k:<rip hex>:<count> ... u:<pid>:<name>:<count> ...

export interface ProfileWindow {
  ms: number;
  samples: number;
  idle: number;
  user: number;
  kernel: { rip: bigint; count: number }[];
  procs: { pid: number; name: string; count: number }[];
}

export function parseProfileLine(line: string): ProfileWindow | null {
  const parts = line.trim().split(/\s+/);
  if (parts[0] !== "P" || parts.length < 5) return null;
  const nums = parts.slice(1, 5).map(Number);
  if (nums.some((n) => !Number.isFinite(n))) return null;
  const w: ProfileWindow = { ms: nums[0], samples: nums[1], idle: nums[2], user: nums[3], kernel: [], procs: [] };
  for (const p of parts.slice(5)) {
    const f = p.split(":");
    if (f[0] === "k" && f.length === 3) {
      w.kernel.push({ rip: BigInt("0x" + f[1]), count: Number(f[2]) });
    } else if (f[0] === "u" && f.length === 4) {
      w.procs.push({ pid: Number(f[1]), name: f[2], count: Number(f[3]) });
    }
  }
  return w;
}

/** Kernel time = everything that is neither idle nor user. */
export function kernelSamples(w: ProfileWindow): number {
  return Math.max(0, w.samples - w.idle - w.user);
}

/** Adds counts per function name (several addresses map to one function). */
export function byFunction(w: ProfileWindow, name: (rip: bigint) => string): Map<string, number> {
  const m = new Map<string, number>();
  for (const k of w.kernel) {
    const n = name(k.rip);
    m.set(n, (m.get(n) ?? 0) + k.count);
  }
  return m;
}
