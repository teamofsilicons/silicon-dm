"use client";

/**
 * Every conversation's address, ready to hand to a Silicon or a terminal: the address itself (copy), the link to it on
 * this website (Copy link), and the CLI commands that read and write it in Silicon UI's code block, laid out for the
 * width the block really has (the panel, the drawer or the bottom sheet; cli.ts), so it never scrolls sideways.
 */
import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { CodeBlock } from "@/components/silicon-ui/code-block/code-block";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { conversationHref } from "@/lib/chat/address";
import { conversationCommands, DEFAULT_COLUMNS } from "./cli";
import styles from "./details.module.css";

let canvas: HTMLCanvasElement | null = null;

/** How many characters of the block's monospace text fit on one of its lines (null while it is not laid out). */
function columnsOf(pre: HTMLPreElement): number | null {
  const style = getComputedStyle(pre);
  const room = pre.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  if (room <= 0) return null;
  canvas ??= document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!context) return null;
  context.font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
  const advance = context.measureText("0".repeat(20)).width / 20;
  return advance > 0 ? Math.max(20, Math.floor(room / advance)) : null;
}

/** The block's width in characters, measured before the first paint and again when the column changes width. */
function useColumns(): [RefObject<HTMLDivElement | null>, number] {
  const box = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(DEFAULT_COLUMNS);
  useLayoutEffect(() => {
    const node = box.current;
    if (!node) return;
    const measure = () => {
      const pre = node.querySelector("pre");
      const next = pre ? columnsOf(pre) : null;
      if (next) setColumns(next);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  return [box, columns];
}

export function AddressSection({ address, view, group }: { address: string; view: string | null; group: boolean }) {
  // The panel is drawn in the browser only (a portal), so the origin is known.
  const link = `${typeof window === "undefined" ? "" : window.location.origin}${conversationHref(address, { view })}`;
  const [box, columns] = useColumns();
  return (
    <section className={styles.section} aria-labelledby="details-address">
      <h3 id="details-address" className={styles.heading}>Address</h3>
      <div className={styles.address}>
        <code className={styles.addressValue}>{address}</code>
        <CopyButton value={address} label="Copy address" iconOnly variant="plain" />
      </div>
      <p className={styles.text}>
        {view ? "The CLI reads this conversation by its address; your own sign-in can read it, as its custodian." : "Silicons and the CLI reach this conversation by its address."}
      </p>
      <div className={styles.copyRow}>
        <CopyButton value={link} label="Copy link" />
      </div>
      <div className={styles.cli} ref={box}>
        {/* Keyed by the width: a new layout mounts afresh instead of cross-fading from the one it replaces. */}
        <CodeBlock key={columns} code={conversationCommands(address, { group, view, width: columns })} filename="Terminal" language="sh" />
      </div>
    </section>
  );
}
