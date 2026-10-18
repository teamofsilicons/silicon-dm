// A small WebSocket server side (RFC 6455) for the stand-ins, so they need no npm package: the opening handshake,
// masked client frames, fragmented messages, ping/pong and the closing handshake. Text arrives as strings (UTF-8
// checked), binary as Buffers. Not a general library: one connection per upgrade, no extensions (no compression).
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const OPCODES = { continuation: 0x0, text: 0x1, binary: 0x2, close: 0x8, ping: 0x9, pong: 0xa };
const decoder = new TextDecoder('utf-8', { fatal: true });

/**
 * Accepts an HTTP upgrade (from server.on('upgrade')). `protocol` picks the subprotocol to answer from the offered
 * list (return null to answer none). Returns the connection, or null after answering 400 when the request is not a
 * WebSocket opening handshake.
 */
export function acceptWebSocket(request, socket, head, { protocol = () => null, maxMessageBytes = 16 * 1024 * 1024 } = {}) {
  const key = request.headers['sec-websocket-key'];
  const upgrade = String(request.headers.upgrade || '').toLowerCase();
  if (request.method !== 'GET' || upgrade !== 'websocket' || !key || request.headers['sec-websocket-version'] !== '13') {
    socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nExpected a WebSocket opening handshake (version 13).\n');
    return null;
  }
  const offered = String(request.headers['sec-websocket-protocol'] || '').split(',').map((value) => value.trim()).filter(Boolean);
  const chosen = offered.length ? protocol(offered) : null;
  const accept = createHash('sha1').update(key + GUID).digest('base64');
  const lines = ['HTTP/1.1 101 Switching Protocols', 'Upgrade: websocket', 'Connection: Upgrade', `Sec-WebSocket-Accept: ${accept}`];
  if (chosen) lines.push(`Sec-WebSocket-Protocol: ${chosen}`);
  socket.write(`${lines.join('\r\n')}\r\n\r\n`);
  socket.setNoDelay(true);
  const connection = new WebSocketConnection(socket, { maxMessageBytes, protocol: chosen, offered });
  if (head && head.length) connection.receive(head);
  return connection;
}

/** Writes an HTTP error answer instead of upgrading (e.g. 401 for a missing key). */
export function refuseUpgrade(socket, status, reason, body = '') {
  const text = typeof body === 'string' ? body : JSON.stringify(body);
  socket.end(`HTTP/1.1 ${status} ${reason}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(text)}\r\n\r\n${text}`);
}

export class WebSocketConnection extends EventEmitter {
  constructor(socket, { maxMessageBytes, protocol, offered }) {
    super();
    this.socket = socket;
    this.maxMessageBytes = maxMessageBytes;
    this.protocol = protocol;
    this.offered = offered;
    this.buffer = Buffer.alloc(0);
    this.fragments = null;
    this.fragmentOpcode = 0;
    this.fragmentBytes = 0;
    this.closeSent = false;
    this.closed = false;
    socket.on('data', (chunk) => this.receive(chunk));
    socket.on('close', () => this.finish(1006, 'connection lost'));
    socket.on('error', () => this.finish(1006, 'connection error'));
  }

  get open() {
    return !this.closed && !this.closeSent;
  }

  receive(chunk) {
    this.buffer = this.buffer.length ? Buffer.concat([this.buffer, chunk]) : chunk;
    for (;;) {
      const frame = this.parse();
      if (!frame) return;
      this.handle(frame);
      if (this.closed) return;
    }
  }

  parse() {
    const data = this.buffer;
    if (data.length < 2) return null;
    const fin = (data[0] & 0x80) !== 0;
    const rsv = data[0] & 0x70;
    const opcode = data[0] & 0x0f;
    const masked = (data[1] & 0x80) !== 0;
    let length = data[1] & 0x7f;
    let offset = 2;
    if (length === 126) {
      if (data.length < 4) return null;
      length = data.readUInt16BE(2);
      offset = 4;
    } else if (length === 127) {
      if (data.length < 10) return null;
      const big = data.readBigUInt64BE(2);
      if (big > BigInt(this.maxMessageBytes)) {
        this.fail(1009, 'message too big');
        return null;
      }
      length = Number(big);
      offset = 10;
    }
    if (masked) offset += 4;
    if (data.length < offset + length) return null;
    if (rsv) {
      this.fail(1002, 'reserved bits set (no extension was agreed)');
      return null;
    }
    if (!masked) {
      this.fail(1002, 'client frames must be masked');
      return null;
    }
    const mask = data.subarray(offset - 4, offset);
    const payload = Buffer.from(data.subarray(offset, offset + length));
    for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
    this.buffer = data.subarray(offset + length);
    return { fin, opcode, payload };
  }

  handle({ fin, opcode, payload }) {
    if (opcode >= 0x8) {
      if (!fin || payload.length > 125) return this.fail(1002, 'bad control frame');
      if (opcode === OPCODES.close) {
        const code = payload.length >= 2 ? payload.readUInt16BE(0) : 1005;
        const reason = payload.length > 2 ? payload.subarray(2).toString('utf8') : '';
        if (!this.closeSent) this.sendFrame(OPCODES.close, payload.length >= 2 ? payload.subarray(0, 2) : Buffer.alloc(0));
        this.closeSent = true;
        this.socket.end();
        return this.finish(code, reason);
      }
      if (opcode === OPCODES.ping) return this.sendFrame(OPCODES.pong, payload);
      if (opcode === OPCODES.pong) return undefined;
      return this.fail(1002, 'unknown control opcode');
    }
    if (opcode === OPCODES.continuation) {
      if (!this.fragments) return this.fail(1002, 'continuation without a start');
    } else if (opcode === OPCODES.text || opcode === OPCODES.binary) {
      if (this.fragments) return this.fail(1002, 'new message inside a fragmented one');
      this.fragments = [];
      this.fragmentOpcode = opcode;
      this.fragmentBytes = 0;
    } else {
      return this.fail(1002, 'unknown data opcode');
    }
    this.fragmentBytes += payload.length;
    if (this.fragmentBytes > this.maxMessageBytes) return this.fail(1009, 'message too big');
    this.fragments.push(payload);
    if (!fin) return undefined;
    const message = Buffer.concat(this.fragments);
    const kind = this.fragmentOpcode;
    this.fragments = null;
    if (kind === OPCODES.text) {
      let text;
      try {
        text = decoder.decode(message);
      } catch {
        return this.fail(1007, 'text is not UTF-8');
      }
      this.emit('text', text);
    } else {
      this.emit('binary', message);
    }
    return undefined;
  }

  sendFrame(opcode, payload) {
    if (this.closed || this.socket.destroyed) return false;
    const length = payload.length;
    let header;
    if (length < 126) {
      header = Buffer.from([0x80 | opcode, length]);
    } else if (length < 65536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 126;
      header.writeUInt16BE(length, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 127;
      header.writeBigUInt64BE(BigInt(length), 2);
    }
    return this.socket.write(length ? Buffer.concat([header, payload]) : header);
  }

  /** Sends a text message (a string, or a value serialised as JSON). */
  sendText(value) {
    if (!this.open) return false;
    return this.sendFrame(OPCODES.text, Buffer.from(typeof value === 'string' ? value : JSON.stringify(value), 'utf8'));
  }

  /** Sends binary data; resolves once the socket can take more (backpressure). */
  async sendBinary(buffer) {
    if (!this.open) return false;
    if (this.sendFrame(OPCODES.binary, buffer)) return true;
    await new Promise((resolve) => {
      const done = () => {
        this.socket.off('drain', done);
        this.socket.off('close', done);
        resolve();
      };
      this.socket.once('drain', done);
      this.socket.once('close', done);
    });
    return this.open;
  }

  /** Starts the closing handshake; the socket ends when the client answers or after two seconds. */
  close(code = 1000, reason = '') {
    if (this.closed || this.closeSent) return;
    const body = Buffer.alloc(2 + Buffer.byteLength(reason));
    body.writeUInt16BE(code, 0);
    body.write(reason, 2);
    this.sendFrame(OPCODES.close, body.subarray(0, Math.min(body.length, 125)));
    this.closeSent = true;
    setTimeout(() => {
      this.socket.destroy();
      this.finish(code, reason);
    }, 2000).unref();
  }

  /** A protocol error: one close frame (best effort), then the connection ends. */
  fail(code, reason) {
    if (!this.closed && !this.closeSent) {
      const body = Buffer.alloc(2);
      body.writeUInt16BE(code, 0);
      this.sendFrame(OPCODES.close, body);
      this.closeSent = true;
    }
    this.socket.end();
    setTimeout(() => this.socket.destroy(), 500).unref();
    this.finish(code, reason);
  }

  finish(code, reason) {
    if (this.closed) return;
    this.closed = true;
    this.emit('close', code, reason);
  }
}
