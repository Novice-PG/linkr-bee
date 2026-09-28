// Keep received device data separate from local echo, UI messages and AI text.
export class SerialJournal {
  constructor(capacity = 128 * 1024) {
    this.capacity = capacity;
    this.reset();
  }

  reset() {
    this.decoder = new TextDecoder();
    /* The retained window is kept as the chunks that produced it: rebuilding one
     * string per append copies the whole 128KB ring on every log burst. `start`
     * skips the evicted head, `length` counts retained chars in raw UTF-16 units
     * so cursors stay directly comparable, exactly like the slices did. */
    this.parts = [];
    this.start = 0;
    this.length = 0;
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
    if (visible) {
      this.parts.push(visible);
      this.length += visible.length;
      this.evict();
    }
    this.updatedAt = new Date().toISOString();
  }

  /* Drop from the head until the window fits: whole chunks first, then the head of
   * the chunk that straddles the limit, so at most `capacity` chars are retained. */
  evict() {
    while (this.length > this.capacity) {
      if (this.start >= this.parts.length) {
        this.length = 0;
        break;
      }
      const head = this.parts[this.start];
      if (!head) {
        this.start += 1;
        continue;
      }
      const excess = this.length - this.capacity;
      if (head.length <= excess) {
        this.length -= head.length;
        this.parts[this.start] = "";
        this.start += 1;
      } else {
        this.parts[this.start] = head.slice(excess);
        this.length -= excess;
      }
      if (this.start > 256) {
        this.parts = this.parts.slice(this.start);
        this.start = 0;
      }
    }
    // Never begin the retained window on a low surrogate: the split pair would
    // surface as a replacement character in the journal and to the agent.
    const head = this.parts[this.start];
    const first = head ? head.charCodeAt(0) : NaN;
    if (first >= 0xdc00 && first <= 0xdfff) {
      this.parts[this.start] = head.slice(1);
      this.length -= 1;
    }
  }

  /* A page is collected from the tail of the ring, so a read costs the page and not
   * the whole window. `from` is an offset into the retained text; `need` counts
   * back to it, so the head chunk contributes only its tail and the first char of
   * `text` sits exactly at `from` — anything past `count` is trimmed after. */
  page(from, count) {
    if (count <= 0) return "";
    let need = this.length - from;
    if (need <= 0) return "";
    let text = "";
    for (let i = this.parts.length - 1; i >= this.start && need > 0; i--) {
      const part = this.parts[i];
      if (!part) continue;
      const take = part.length > need ? part.slice(part.length - need) : part;
      text = take + text;
      need -= take.length;
    }
    return text.length > count ? text.slice(0, count) : text;
  }

  read({ after, limit = 12000 } = {}) {
    limit = Math.max(1, Math.min(16000, Math.trunc(limit) || 12000));
    const oldest = this.end - this.length;
    const requested = Number.isFinite(after) ? Math.trunc(after) : Math.max(0, this.end - limit);
    const start = Math.min(this.end, Math.max(oldest, requested));
    const end = Math.min(this.end, start + limit);
    const text = this.page(start - oldest, end - start).replace(/\0/g, "");
    return { text, start, cursor: end, latestCursor: this.end,
      truncated: requested < oldest, updatedAt: this.updatedAt };
  }
}
