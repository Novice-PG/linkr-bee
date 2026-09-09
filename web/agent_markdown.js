import { marked } from "./vendor/marked/marked.esm.js";
import DOMPurify from "./vendor/dompurify/purify.es.mjs";

// Keep the source separate from the rendered DOM while streaming partial tokens.
const sources = new WeakMap();

export function renderAssistantMarkdown(body, source, { append = false } = {}) {
  source = append ? (sources.get(body) || "") + source : source;
  sources.set(body, source);
  const fragment = DOMPurify.sanitize(marked.parse(source, { gfm: true, breaks: true, async: false }), {
    ALLOWED_TAGS: ["p", "br", "h1", "h2", "h3", "h4", "h5", "h6", "strong", "b", "em", "i", "del",
      "ul", "ol", "li", "blockquote", "pre", "code", "hr", "a", "table", "thead", "tbody", "tr", "th", "td"],
    ALLOWED_ATTR: ["href", "title", "start"],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
    RETURN_DOM_FRAGMENT: true,
  });
  for (const link of fragment.querySelectorAll("a")) {
    // Only explicit web links can navigate; never relative app routes or scripts.
    const href = link.getAttribute("href") || "";
    try {
      const url = new URL(href);
      if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) throw new Error();
      link.href = url.href;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
    } catch { link.removeAttribute("href"); }
  }
  for (const table of fragment.querySelectorAll("table")) {
    const scroll = document.createElement("div");
    scroll.className = "agent-table-scroll";
    scroll.tabIndex = 0;
    table.replaceWith(scroll);
    scroll.append(table);
  }
  for (const pre of fragment.querySelectorAll("pre")) {
    const copy = document.createElement("button");
    const zh = document.documentElement.lang.startsWith("zh");
    copy.type = "button"; copy.className = "btn agent-code-copy";
    copy.textContent = zh ? "复制代码" : "Copy code";
    copy.addEventListener("click", async () => {
      try { await navigator.clipboard.writeText(pre.textContent); copy.textContent = zh ? "已复制" : "Copied"; }
      catch { copy.textContent = zh ? "复制失败，请手动选择" : "Copy failed; select manually"; }
    });
    pre.before(copy);
  }
  body.classList.add("agent-markdown");
  body.replaceChildren(fragment);
}
