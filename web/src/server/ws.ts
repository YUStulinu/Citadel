// A minimal WebSocket server side (RFC 6455): the opening handshake and text frames, enough for
// the console. No dependencies.
import { createHash } from "crypto";
import type { IncomingMessage } from "http";
import type { Duplex } from "stream";

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

export function acceptKey(key: string): string {
  return createHash("sha1").update(key + GUID).digest("base64");
}

/** Encodes one unmasked (server to client) frame. */
export function encodeFrame(payload: Buffer, opcode = 0x1): Buffer {
  const n = payload.length;
  let head: Buffer;
  if (n < 126) {
    head = Buffer.from([0x80 | opcode, n]);
  } else if (n < 65536) {
    head = Buffer.alloc(4);
    head[0] = 0x80 | opcode;
    head[1] = 126;
    head.writeUInt16BE(n, 2);
  } else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(n), 2);
  }
  return Buffer.concat([head, payload]);
}

export interface Frame {
  fin: boolean;
  opcode: number;
  payload: Buffer;
}

/** Decodes as many complete frames as `buf` holds; returns them and the bytes consumed. */
export function decodeFrames(buf: Buffer): { frames: Frame[]; used: number } {
  const frames: Frame[] = [];
  let o = 0;
  while (o + 2 <= buf.length) {
    const b0 = buf[o];
    const b1 = buf[o + 1];
    let len = b1 & 0x7f;
    let p = o + 2;
    if (len === 126) {
      if (p + 2 > buf.length) break;
      len = buf.readUInt16BE(p);
      p += 2;
    } else if (len === 127) {
      if (p + 8 > buf.length) break;
      len = Number(buf.readBigUInt64BE(p));
      p += 8;
    }
    const masked = (b1 & 0x80) !== 0;
    const maskEnd = p + (masked ? 4 : 0);
    if (maskEnd + len > buf.length) break;
    const payload = Buffer.from(buf.subarray(maskEnd, maskEnd + len));
    if (masked) {
      const mask = buf.subarray(p, p + 4);
      for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
    }
    frames.push({ fin: (b0 & 0x80) !== 0, opcode: b0 & 0x0f, payload });
    o = maskEnd + len;
  }
  return { frames, used: o };
}

/** A connected client. */
export class WsClient {
  private buf = Buffer.alloc(0);
  open = true;

  constructor(
    private sock: Duplex,
    onText: (s: string) => void,
    onClose: () => void,
  ) {
    sock.on("data", (d: Buffer) => {
      this.buf = Buffer.concat([this.buf, d]);
      const { frames, used } = decodeFrames(this.buf);
      this.buf = this.buf.subarray(used);
      for (const f of frames) {
        if (f.opcode === 0x1) onText(f.payload.toString("utf8"));
        else if (f.opcode === 0x8) this.close();
        else if (f.opcode === 0x9) sock.write(encodeFrame(f.payload, 0xa));
      }
    });
    sock.on("close", () => {
      this.open = false;
      onClose();
    });
    sock.on("error", () => this.close());
  }

  send(text: string): void {
    if (this.open) this.sock.write(encodeFrame(Buffer.from(text, "utf8")));
  }

  close(): void {
    if (!this.open) return;
    this.open = false;
    this.sock.end(encodeFrame(Buffer.alloc(0), 0x8));
  }
}

/** Completes the handshake for an HTTP upgrade request. Returns false if it is not one. */
export function upgrade(req: IncomingMessage, sock: Duplex): boolean {
  const key = req.headers["sec-websocket-key"];
  if (typeof key !== "string" || (req.headers.upgrade ?? "").toLowerCase() !== "websocket") return false;
  sock.write(
    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" + `Sec-WebSocket-Accept: ${acceptKey(key)}\r\n\r\n`,
  );
  return true;
}
