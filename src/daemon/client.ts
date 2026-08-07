/** The client half of the wire: connect, ask one question, read the answer. */

import { connect, type Socket } from 'node:net';
import { createInterface } from 'node:readline';
import {
  DaemonError,
  decodeFrame,
  encode,
  lines,
  PROTOCOL_VERSION,
  socketPath,
  type Envelope,
  type Request,
  type WatchRequest,
} from './protocol.js';

/** Long enough to cover a busy daemon, short enough not to look like a hang. */
const CONNECT_TIMEOUT_MS = 2_000;
const RESPONSE_TIMEOUT_MS = 30_000;

/**
 * Connect to the daemon, or answer null when nothing is listening.
 *
 * Null is an ordinary outcome, not an error: the CLI falls back to reading the
 * database itself, which is the path a machine without the daemon installed
 * takes on every command.
 */
export function connectDaemon(path = socketPath()): Promise<Socket | null> {
  return new Promise((resolve) => {
    const socket = connect(path);
    const timer = setTimeout(() => settle(null), CONNECT_TIMEOUT_MS);
    const settle = (value: Socket | null): void => {
      clearTimeout(timer);
      socket.removeAllListeners('connect');
      socket.removeAllListeners('error');
      if (value === null) socket.destroy();
      else socket.on('error', () => socket.destroy());
      resolve(value);
    };
    socket.once('connect', () => settle(socket));
    socket.once('error', () => settle(null));
  });
}

function ask(socket: Socket, request: Request): void {
  const envelope: Envelope = { ...request, v: PROTOCOL_VERSION };
  socket.write(encode(envelope));
}

/** Send one request and return its result, closing the connection after. */
export async function request(socket: Socket, message: Request): Promise<unknown> {
  const reader = createInterface({ input: socket, crlfDelay: Infinity });
  const timer = setTimeout(() => socket.destroy(), RESPONSE_TIMEOUT_MS);
  try {
    ask(socket, message);
    for await (const line of lines(reader)) {
      const frame = decodeFrame(line);
      if (frame.type === 'error') throw new DaemonError(frame.message, frame.code);
      if (frame.type === 'result') return frame.value;
    }
    throw new DaemonError('msgd closed the connection without answering', 'error');
  } finally {
    clearTimeout(timer);
    reader.close();
    socket.destroy();
  }
}

/**
 * Send a watch request and yield messages as they arrive, until the caller
 * stops iterating or the daemon goes away.
 */
export async function* stream(socket: Socket, message: WatchRequest): AsyncGenerator<unknown> {
  const reader = createInterface({ input: socket, crlfDelay: Infinity });
  try {
    ask(socket, message);
    for await (const line of lines(reader)) {
      const frame = decodeFrame(line);
      if (frame.type === 'error') throw new DaemonError(frame.message, frame.code);
      if (frame.type === 'item') yield frame.value;
    }
  } finally {
    reader.close();
    socket.destroy();
  }
}
