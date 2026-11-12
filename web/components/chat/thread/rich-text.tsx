"use client";

/**
 * Message text as React elements: Markdown and GitHub-style tables, task lists, and strikethrough. Links use only
 * HTTP(S), raw HTML stays text, and remote images are links rather than automatic requests. Code blocks keep their
 * copy control. The source is never injected as HTML.
 *
 * Very long messages (the service allows 100 million characters) show their first part with a way to show the rest,
 * so one message cannot stall the thread.
 */
import { isValidElement, memo, useEffect, useRef, useState } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import type { Block } from "@/lib/chat/markdown";
import { cutAtWord } from "./thread-model";
import styles from "./thread.module.css";

const SHOWN_AT_FIRST = 12_000;

/**
 * A code block scrolls inside itself, so the keyboard must be able to reach it (WCAG 2.1.1). It is one of its message's
 * controls: a Tab stop while that message is selected (row-focus.ts), not in every message on the way through the
 * thread (Tab would then stop at, and scroll the thread to, every long block above). Whether it scrolls changes with
 * the column's width, so its tabindex is set here as that changes, in the same form row-focus.ts keeps for a row that
 * is not selected. (axe cannot see a roving Tab order; the code inside answers focus, so axe asks for a review of a
 * block in an unselected message instead of failing it.)
 */
function CodeBlockView({ block }: { block: Extract<Block, { type: "code" }> }) {
  const pre = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const node = pre.current;
    if (!node) return;
    const check = () => {
      const scrolls = node.scrollWidth > node.clientWidth + 1 || node.scrollHeight > node.clientHeight + 1;
      if (!scrolls) {
        node.removeAttribute("tabindex");
        node.removeAttribute("data-row-tab");
        return;
      }
      if (node.hasAttribute("tabindex")) return;
      const row = node.closest("[data-message-row]");
      if (!row || row.contains(document.activeElement)) node.setAttribute("tabindex", "0");
      else {
        node.setAttribute("data-row-tab", "0");
        node.setAttribute("tabindex", "-1");
      }
    };
    check();
    const observer = new ResizeObserver(check);
    observer.observe(node);
    return () => observer.disconnect();
  }, [block.text]);
  return (
    <div className={styles.codeBlock} data-sq="surface">
      <div className={styles.codeHead}>
        <span className={styles.codeLanguage}>{block.language ?? "code"}</span>
        <CopyButton value={block.text} label="Copy code" iconOnly variant="plain" />
      </div>
      <pre ref={pre} className={styles.codePre} aria-label={`${block.language ?? "Code"} block`}><code tabIndex={-1}>{block.text}</code></pre>
    </div>
  );
}

/** Only explicit web links can navigate; images never load on their own. */
function webUrl(value: string): string | undefined {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:" ? value : undefined;
  } catch { return undefined; }
}

const markdownComponents: Components = {
  p: ({ children }) => <p className={styles.paragraph}>{children}</p>,
  a: ({ href, children }) => href ? <a href={href} target="_blank" rel="noopener noreferrer nofollow" className={styles.link}>{children}</a> : <>{children}</>,
  img: ({ src, alt }) => typeof src === "string" && src ? <a href={src} target="_blank" rel="noopener noreferrer nofollow" className={styles.link}>{alt || "Open image"}</a> : <>{alt}</>,
  code: ({ children }) => <code className={styles.inlineCode} data-sq="surface">{children}</code>,
  pre: ({ children }) => {
    if (!isValidElement<{ className?: string; children?: unknown }>(children)) return <pre>{children}</pre>;
    const content = children.props.children;
    if (typeof content !== "string") return <pre>{children}</pre>;
    const language = /^language-(.+)$/.exec(children.props.className ?? "")?.[1] ?? null;
    return <CodeBlockView block={{ type: "code", language, text: content.replace(/\n$/, "") }} />;
  },
  ul: ({ children }) => <ul className={styles.list}>{children}</ul>,
  ol: ({ children, start }) => <ol className={styles.list} start={start}>{children}</ol>,
  table: ({ children }) => <div className={styles.tableScroll} tabIndex={0} role="region" aria-label="Message table"><table className={styles.markdownTable}>{children}</table></div>,
};

export const RichText = memo(function RichText({ text }: { text: string }) {
  const [all, setAll] = useState(false);
  const long = text.length > SHOWN_AT_FIRST && !all;
  const shown = long ? cutAtWord(text, SHOWN_AT_FIRST) : text;
  return (
    <div className={styles.richText}>
      <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents} urlTransform={value => webUrl(value)}>{shown}</Markdown>
      {long ? (
        <button type="button" className={styles.showAll} onClick={() => setAll(true)}>
          Show the whole message ({text.length.toLocaleString("en-US")} characters)
        </button>
      ) : null}
    </div>
  );
});
