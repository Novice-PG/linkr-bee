// Keep received device data separate from local echo, UI messages and AI text.
export class SerialJournal {
  constructor(capacity = 128 * 1024) {
    this.capacity = capacity;
    this.reset();
  }

  reset() {
    this.decoder = new TextDecoder();
    this.text = "";
    this.end = 0;
    this.updatedAt = null;
  }

  append(bytes) {
    const text = this.decoder.decode(bytes, { stream: true });
    this.end += text.length;
    this.text = (this.text + text).slice(-this.capacity);
    this.updatedAt = new Date().toISOString();
  }

  read({ after, limit = 12000 } = {}) {
    limit = Math.max(1, Math.min(16000, Math.trunc(limit) || 12000));
    const oldest = this.end - this.text.length;
    const requested = Number.isFinite(after) ? Math.trunc(after) : Math.max(0, this.end - limit);
    const start = Math.min(this.end, Math.max(oldest, requested));
    const end = Math.min(this.end, start + limit);
    const raw = this.text.slice(start - oldest, end - oldest);
    // Strip common CSI/OSC terminal controls, retaining the original cursor
    // offsets so callers can read subsequent chunks without duplicating data.
    const text = raw.replace(/\x1b\][\s\S]*?(?:\x07|\x1b\\)/g, "")
      .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
      .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "");
    return { text, start, cursor: end, latestCursor: this.end,
      truncated: requested < oldest, updatedAt: this.updatedAt };
  }
}
