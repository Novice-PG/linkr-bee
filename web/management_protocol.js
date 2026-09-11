"use strict";

export const MGMT_FLAG_FINAL = 1 << 0;
export const MGMT_FLAG_ERROR = 1 << 1;

export const MGMT_HEADER_SIZE = 12;
export const MGMT_API_MAJOR = 1;
export const RELIABLE_UART_HEADER_SIZE = 12;
export const RELIABLE_UART_API_MAJOR = 1;
export const RELIABLE_UART_MAX_PAYLOAD = 232;

/* Frame rejections are user-visible, so each reason maps to the message the
 * terminal shows. */
export const MGMT_FRAME_ERRORS = {
  "short-header": "orphaned management fragment",
  "bad-magic": "orphaned management fragment",
  "bad-version": "unsupported management response",
  "bad-type": "unsupported management response",
  "bad-length": "oversized management response header",
  oversized: "oversized management response",
};

export const RELIABLE_FRAME_ERRORS = {
  "short-header": "orphaned Reliable UART fragment",
  "bad-magic": "orphaned Reliable UART fragment",
  "bad-version": "unsupported Reliable UART version",
  "bad-length": "invalid Reliable UART frame header",
  oversized: "oversized Reliable UART frame",
};

const MGMT_MAGIC = [0x4c, 0x4b]; /* "LK" */
const RELIABLE_MAGIC = [0x4c, 0x52]; /* "LR" */

export function parseManagementHeader(bytes) {
  if (bytes[0] !== MGMT_MAGIC[0] || bytes[1] !== MGMT_MAGIC[1]) {
    return { error: "bad-magic" };
  }
  if (bytes[2] !== MGMT_API_MAJOR) {
    return { error: "bad-version" };
  }
  if (bytes[3] !== 2 && bytes[3] !== 3) {
    return { error: "bad-type" };
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    type: bytes[3],
    requestId: view.getUint32(4, true),
    expected: view.getUint16(8, true),
    flags: view.getUint16(10, true),
  };
}

export function parseReliableHeader(bytes, maxPayload = RELIABLE_UART_MAX_PAYLOAD) {
  if (bytes[0] !== RELIABLE_MAGIC[0] || bytes[1] !== RELIABLE_MAGIC[1]) {
    return { error: "bad-magic" };
  }
  if (bytes[2] !== RELIABLE_UART_API_MAJOR) {
    return { error: "bad-version" };
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const sequence = view.getUint32(4, true);
  const expected = view.getUint16(8, true);
  if (sequence === 0 || expected === 0 || expected > maxPayload) {
    return { error: "bad-length" };
  }
  return { sequence, expected };
}

/* Documented completion phases for an asynchronous WiFi operation
 * (docs/LINKR_BLE_API.zh-CN.md): the initial "OK …accepted" reply only means
 * the request was queued, and only a FINAL event ends the operation. */
export const WIFI_FINAL_PHASES = ["ready", "failed", "off"];

const WIFI_EVENT_PREFIX = "@event wifi ";
const WIFI_EVENT_RE =
  /^operation=(\d+)\s+phase=([a-z]+)\s+result=(-?\d+)([\s\S]*)$/;

export function isIpv4Address(value) {
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(String(value ?? ""));
}

/* Parse one "@event wifi operation=42 phase=ready result=0 state=… ip=… error=…"
 * line. Returns null for anything that is not a WiFi lifecycle event. */
export function parseWifiEvent(text) {
  const value = String(text ?? "").trim();
  if (!value.startsWith(WIFI_EVENT_PREFIX)) {
    return null;
  }
  const match = value.slice(WIFI_EVENT_PREFIX.length).match(WIFI_EVENT_RE);
  if (!match) {
    return null;
  }
  const fields = {};
  for (const token of match[4].trim().split(/\s+/)) {
    const at = token.indexOf("=");
    if (at > 0) {
      fields[token.slice(0, at)] = token.slice(at + 1);
    }
  }
  const phase = match[2];
  return {
    operation: Number(match[1]),
    phase,
    result: Number(match[3]),
    state: fields.state ?? "",
    ip: fields.ip ?? "",
    error: fields.error ?? "",
    isFinal: WIFI_FINAL_PHASES.includes(phase),
    failed: phase === "failed",
  };
}

/* The API is explicit that reaching the ready phase is not enough: the bridge
 * must also report a usable IPv4 address, because "ip=ready"/"ip=down" only
 * mean the interface has no address yet. */
export function wifiProvisioningComplete(event) {
  return Boolean(event) && event.phase === "ready" && isIpv4Address(event.ip);
}

/* Both Linkr framing schemes put the whole header in the first fragment and
 * send the remaining payload bytes bare, so reassembly is shared here instead of
 * being re-implemented (and left untested) inside the terminal app. */
export function createFragmentReassembler({ headerSize, maxPayload, parseHeader }) {
  let pending = null;
  const limit = () => (typeof maxPayload === "function" ? maxPayload() : maxPayload);

  const fail = (reason) => {
    pending = null;
    return { status: "error", reason };
  };

  return {
    get pending() {
      return pending !== null;
    },
    reset() {
      pending = null;
    },
    /* Returns {status:"incomplete"} while more fragments are needed,
     * {status:"complete", payload, meta} once the logical message is whole, or
     * {status:"error", reason} when the stream cannot be trusted and the
     * reassembler has dropped its state. */
    push(value) {
      const bytes = value instanceof Uint8Array ? value : new Uint8Array(value);
      let fragment = bytes;

      if (!pending) {
        if (bytes.length < headerSize) {
          return { status: "error", reason: "short-header" };
        }
        const header = parseHeader(bytes);
        if (header.error) {
          return { status: "error", reason: header.error };
        }
        const expected = header.expected;
        if (!Number.isInteger(expected) || expected <= 0 || expected > limit()) {
          return { status: "error", reason: "bad-length" };
        }
        pending = { meta: header, chunks: [], received: 0 };
        fragment = bytes.slice(headerSize);
      }

      const message = pending;
      if (message.received + fragment.length > message.meta.expected) {
        return fail("oversized");
      }
      message.chunks.push(fragment);
      message.received += fragment.length;
      if (message.received !== message.meta.expected) {
        return { status: "incomplete" };
      }

      const payload = new Uint8Array(message.meta.expected);
      let offset = 0;
      for (const chunk of message.chunks) {
        payload.set(chunk, offset);
        offset += chunk.length;
      }
      pending = null;
      return { status: "complete", payload, meta: message.meta };
    },
  };
}

export class ManagementResponseTracker {  constructor(timeoutMs = 10000) {
    this.timeoutMs = timeoutMs;
    this.pending = new Map();
  }

  wait(requestId) {
    if (this.pending.has(requestId)) {
      throw new Error(`Management request ${requestId} is already pending`);
    }

    let resolvePromise;
    let rejectPromise;
    const promise = new Promise((resolve, reject) => {
      resolvePromise = resolve;
      rejectPromise = reject;
    });
    promise.catch(() => {});

    const timer = setTimeout(() => {
      this.pending.delete(requestId);
      rejectPromise(new Error(`Management response timed out (#${requestId})`));
    }, this.timeoutMs);

    this.pending.set(requestId, {
      resolve: resolvePromise,
      reject: rejectPromise,
      timer,
    });
    return promise;
  }

  settle(message) {
    if (message.type !== 2) {
      return false;
    }
    const pending = this.pending.get(message.requestId);
    if (!pending) {
      return false;
    }

    this.pending.delete(message.requestId);
    clearTimeout(pending.timer);
    const text = message.text?.trim() || "";
    if ((message.flags & MGMT_FLAG_ERROR) !== 0 || /^ERR(?:\s|$)/.test(text)) {
      pending.reject(
        new Error(text || `Management request ${message.requestId} failed`),
      );
    } else {
      pending.resolve(message);
    }
    return true;
  }

  reject(requestId, error) {
    const pending = this.pending.get(requestId);
    if (!pending) {
      return false;
    }
    this.pending.delete(requestId);
    clearTimeout(pending.timer);
    pending.reject(error);
    return true;
  }

  rejectAll(error) {
    for (const requestId of this.pending.keys()) {
      this.reject(requestId, error);
    }
  }
}
