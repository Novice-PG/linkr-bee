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
    this.control = "text";
  }

  append(bytes) {
    const text = this.decoder.decode(bytes, { stream: true });
    this.end += text.length;
    // Mask controls as they arrive, before paging or ring eviction can split a
    // sequence. One placeholder per raw UTF-16 code unit preserves all cursors.
    let visible = "";
    for (let i = 0; i < text.length; i++) {
      const char = text[i];
      const code = text.charCodeAt(i);
      let keep = false;
      if (char === "\x18" || char === "\x1a") {
        this.control = "text";
      } else if (char === "\x1b") {
        // ESC can terminate a string with ST (ESC \\) or start a new sequence.
        this.control = "escape";
      } else if (this.control === "osc" || this.control === "string") {
        if (char === "\x9c" || (this.control === "osc" && char === "\x07")) this.control = "text";
      } else if (code < 0x20 || code === 0x7f) {
        // Embedded C0 controls do not terminate ESC/CSI parsing.
        keep = "\t\r\n".includes(char);
      } else if (char === "\x9b") {
        this.control = "csi";
      } else if (char === "\x9d") {
        this.control = "osc";
      } else if ("\x90\x98\x9e\x9f".includes(char)) {
        this.control = "string";
      } else if (code >= 0x80 && code <= 0x9f) {
        this.control = "text";
      } else if (this.control === "escape") {
        if (char === "[") this.control = "csi";
        else if (char === "]") this.control = "osc";
        else if ("PX^_".includes(char)) this.control = "string";
        else this.control = code >= 0x20 && code <= 0x2f ? "intermediate" : "text";
      } else if (this.control === "csi") {
        if (code >= 0x40 && code <= 0x7e) this.control = "text";
      } else if (this.control === "intermediate") {
        if (code >= 0x30 && code <= 0x7e) this.control = "text";
      } else {
        keep = true;
      }
      visible += keep ? char : "\0";
    }
    this.text = (this.text + visible).slice(-this.capacity);
    this.updatedAt = new Date().toISOString();
  }

  read({ after, limit = 12000 } = {}) {
    limit = Math.max(1, Math.min(16000, Math.trunc(limit) || 12000));
    const oldest = this.end - this.text.length;
    const requested = Number.isFinite(after) ? Math.trunc(after) : Math.max(0, this.end - limit);
    const start = Math.min(this.end, Math.max(oldest, requested));
    const end = Math.min(this.end, start + limit);
    const text = this.text.slice(start - oldest, end - oldest).replace(/\0/g, "");
    return { text, start, cursor: end, latestCursor: this.end,
      truncated: requested < oldest, updatedAt: this.updatedAt };
  }
}
