/**
 * Markdown-lite for message text: what people and Silicons actually write in chat, and nothing that could run.
 *
 *   paragraphs (blank lines), line breaks, `inline code`, ```fenced code``` (with an optional language),
 *   **bold** / __bold__, *italic* / _italic_, lists ("- ", "* ", "1. "), and links: bare http(s):// and www. addresses.
 *
 * The parser returns a tree (never HTML); components/chat/thread/rich-text.tsx renders it as React elements, so text
 * can never become markup. Only http and https links are produced. Every rule is linear: a long or hostile message
 * (the service allows 100 million characters) costs time in proportion to its length.
 */

export type Inline =
  | { type: "text"; text: string }
  | { type: "break" }
  | { type: "code"; text: string }
  | { type: "strong"; children: Inline[] }
  | { type: "em"; children: Inline[] }
  | { type: "link"; href: string; text: string };

export type Block =
  | { type: "paragraph"; children: Inline[] }
  | { type: "code"; language: string | null; text: string }
  | { type: "list"; ordered: boolean; start: number; items: Inline[][] };

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w+#.-]*)\s*$/;
const BULLET = /^ {0,3}[-*+]\s+(.*)$/;
const ORDERED = /^ {0,3}(\d{1,9})[.)]\s+(.*)$/;

/** Parses message text into blocks. */
export function parseMarkdown(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length) blocks.push({ type: "paragraph", children: parseInline(paragraph.join("\n")) });
    paragraph = [];
  };
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]!;
    const fence = FENCE.exec(line);
    if (fence) {
      flush();
      const marker = fence[1]!;
      const body: string[] = [];
      index++;
      while (index < lines.length && !new RegExp(`^ {0,3}${marker[0] === "`" ? "`" : "~"}{${marker.length},}\\s*$`).test(lines[index]!)) body.push(lines[index++]!);
      blocks.push({ type: "code", language: fence[2] || null, text: body.join("\n") });
      continue;
    }
    const bullet = BULLET.exec(line);
    const ordered = bullet ? null : ORDERED.exec(line);
    if (bullet || ordered) {
      flush();
      const isOrdered = !!ordered;
      const items: Inline[][] = [];
      const start = ordered ? Number(ordered[1]) : 1;
      while (index < lines.length) {
        const match = isOrdered ? ORDERED.exec(lines[index]!) : BULLET.exec(lines[index]!);
        if (!match) break;
        items.push(parseInline(isOrdered ? match[2]! : match[1]!));
        index++;
      }
      index--;
      blocks.push({ type: "list", ordered: isOrdered, start, items });
      continue;
    }
    if (line.trim() === "") {
      flush();
      continue;
    }
    paragraph.push(line);
  }
  flush();
  return blocks;
}

const PUNCTUATION_END = /[.,;:!?'"’”)\]}>]+$/;

/** The link a bare address stands for (http/https only), trimmed of the punctuation that ends a sentence. */
function linkAt(text: string, start: number): { href: string; text: string; end: number } | null {
  const rest = text.slice(start);
  const match = /^(?:https?:\/\/|www\.)[^\s<>"`]+/i.exec(rest);
  if (!match) return null;
  let raw = match[0];
  // Keep a closing parenthesis that closes one opened inside the link (wikipedia.org/wiki/Foo_(bar)).
  for (;;) {
    const trimmed = raw.replace(PUNCTUATION_END, "");
    if (trimmed === raw) break;
    const tail = raw.slice(trimmed.length);
    if (tail.startsWith(")") && (trimmed.match(/\(/g)?.length ?? 0) > (trimmed.match(/\)/g)?.length ?? 0)) {
      raw = trimmed + ")";
      break;
    }
    raw = trimmed;
  }
  if (!raw || /^(https?:\/\/|www\.)$/i.test(raw)) return null;
  const href = /^www\./i.test(raw) ? `https://${raw}` : raw;
  try {
    const url = new URL(href);
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  } catch {
    return null;
  }
  return { href, text: raw, end: start + raw.length };
}

/** How far an inline code span or an emphasis may reach for its closing marker. */
const REACH = 4000;

const isWord = (char: string | undefined) => !!char && /[\p{L}\p{N}_]/u.test(char);

/** Parses inline text: code, emphasis, links and line breaks. */
export function parseInline(source: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const pushText = () => {
    if (text) out.push({ type: "text", text });
    text = "";
  };
  let index = 0;
  while (index < source.length) {
    const char = source[index]!;
    if (char === "\\" && index + 1 < source.length && "\\`*_~[]()#+-.!".includes(source[index + 1]!)) {
      text += source[index + 1];
      index += 2;
      continue;
    }
    if (char === "\n") {
      pushText();
      out.push({ type: "break" });
      index++;
      continue;
    }
    if (char === "`") {
      let ticks = 1;
      while (source[index + ticks] === "`") ticks++;
      const fence = "`".repeat(ticks);
      const reach = source.slice(index + ticks, index + ticks + REACH).indexOf(fence);
      const close = reach < 0 ? -1 : index + ticks + reach;
      if (close > index) {
        pushText();
        const body = source.slice(index + ticks, close);
        out.push({ type: "code", text: body.length > 2 && body.startsWith(" ") && body.endsWith(" ") ? body.slice(1, -1) : body });
        index = close + ticks;
        continue;
      }
      text += fence;
      index += ticks;
      continue;
    }
    if ((char === "h" || char === "H" || char === "w" || char === "W") && !isWord(source[index - 1])) {
      const link = linkAt(source, index);
      if (link) {
        pushText();
        out.push({ type: "link", href: link.href, text: link.text });
        index = link.end;
        continue;
      }
    }
    if (char === "*" || char === "_") {
      const double = source[index + 1] === char;
      const marker = double ? char + char : char;
      // Underscores inside words (snake_case) are text; an opening marker must be followed by non-space.
      const opens = !isWord(char === "_" ? source[index - 1] : undefined) && source[index + marker.length] !== undefined && !/\s/.test(source[index + marker.length]!);
      if (opens) {
        const close = findClose(source, index + marker.length, marker);
        if (close > index + marker.length) {
          pushText();
          const children = parseInline(source.slice(index + marker.length, close));
          out.push(double ? { type: "strong", children } : { type: "em", children });
          index = close + marker.length;
          continue;
        }
      }
      text += marker;
      index += marker.length;
      continue;
    }
    text += char;
    index++;
  }
  pushText();
  return out;
}

/**
 * Where `marker` closes an emphasis opened just before `from`, or -1. Emphasis is looked for within a bounded reach,
 * so text full of stray asterisks stays linear instead of rescanning to the end for each one.
 */
function findClose(source: string, from: number, marker: string): number {
  const reach = source.slice(0, from + REACH);
  let at = from;
  for (;;) {
    const found = reach.indexOf(marker, at);
    if (found < 0) return -1;
    const before = source[found - 1];
    const after = source[found + marker.length];
    // A single marker that is part of a double one belongs to that one.
    if (marker.length === 1 && (after === marker || before === marker)) {
      at = found + 2;
      continue;
    }
    const closes = before !== undefined && !/\s/.test(before) && (marker[0] !== "_" || !isWord(after));
    if (closes) return found;
    at = found + marker.length;
  }
}

/** The text of inline nodes without formatting (previews, accessible names, copy). */
export function inlineText(nodes: Inline[]): string {
  return nodes.map(node => (node.type === "text" || node.type === "code" ? node.text : node.type === "break" ? "\n" : node.type === "link" ? node.text : inlineText(node.children))).join("");
}

/** One line of plain text for a preview: markers removed, whitespace collapsed. */
export function plainPreview(source: string, max = 160): string {
  const blocks = parseMarkdown(source.length > max * 8 ? source.slice(0, max * 8) : source);
  const words = blocks
    .map(block => (block.type === "paragraph" ? inlineText(block.children) : block.type === "code" ? block.text : block.items.map(inlineText).join(" ")))
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
  return words.length > max ? `${words.slice(0, max - 1).trimEnd()}…` : words;
}
