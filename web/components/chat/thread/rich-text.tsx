"use client";

/**
 * Message text as React elements, from lib/chat/markdown.ts's tree: paragraphs, line breaks, links (http and https
 * only, opening in a new tab), inline code, code blocks with a copy button, bold, italic and lists. Text never becomes
 * markup (no dangerouslySetInnerHTML), so what anyone writes stays what they wrote.
 *
 * Very long messages (the service allows 100 million characters) show their first part with a way to show the rest,
 * so one message cannot stall the thread.
 */
import { memo, useMemo, useState, type ReactNode } from "react";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { parseMarkdown, type Block, type Inline } from "@/lib/chat/markdown";
import styles from "./thread.module.css";

const SHOWN_AT_FIRST = 12_000;

function renderInline(nodes: Inline[], key = "i"): ReactNode[] {
  return nodes.map((node, index) => {
    const id = `${key}-${index}`;
    switch (node.type) {
      case "text":
        return node.text;
      case "break":
        return <br key={id} />;
      case "code":
        return <code key={id} className={styles.inlineCode} data-sq="surface">{node.text}</code>;
      case "strong":
        return <strong key={id}>{renderInline(node.children, id)}</strong>;
      case "em":
        return <em key={id}>{renderInline(node.children, id)}</em>;
      case "link":
        return <a key={id} href={node.href} target="_blank" rel="noopener noreferrer nofollow" className={styles.link}>{node.text}</a>;
    }
  });
}

function CodeBlockView({ block }: { block: Extract<Block, { type: "code" }> }) {
  return (
    <div className={styles.codeBlock} data-sq="surface">
      <div className={styles.codeHead}>
        <span className={styles.codeLanguage}>{block.language ?? "code"}</span>
        <CopyButton value={block.text} label="Copy code" iconOnly variant="plain" />
      </div>
      {/* Focusable, so a long line can be scrolled from the keyboard too. */}
      <pre className={styles.codePre} tabIndex={0} aria-label={`${block.language ?? "Code"} block`}><code>{block.text}</code></pre>
    </div>
  );
}

function renderBlocks(blocks: Block[]): ReactNode[] {
  return blocks.map((block, index) => {
    if (block.type === "paragraph") return <p key={index} className={styles.paragraph}>{renderInline(block.children, `p${index}`)}</p>;
    if (block.type === "code") return <CodeBlockView key={index} block={block} />;
    const List = block.ordered ? "ol" : "ul";
    return (
      <List key={index} className={styles.list} start={block.ordered && block.start !== 1 ? block.start : undefined}>
        {block.items.map((item, at) => <li key={at}>{renderInline(item, `l${index}-${at}`)}</li>)}
      </List>
    );
  });
}

export const RichText = memo(function RichText({ text }: { text: string }) {
  const [all, setAll] = useState(false);
  const long = text.length > SHOWN_AT_FIRST && !all;
  const shown = long ? text.slice(0, SHOWN_AT_FIRST) : text;
  const blocks = useMemo(() => parseMarkdown(shown), [shown]);
  return (
    <div className={styles.richText}>
      {renderBlocks(blocks)}
      {long ? (
        <button type="button" className={styles.showAll} onClick={() => setAll(true)}>
          Show the whole message ({text.length.toLocaleString("en-US")} characters)
        </button>
      ) : null}
    </div>
  );
});
