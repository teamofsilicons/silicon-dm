"use client";

import { createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { FocusEvent, MouseEvent, PointerEvent, ReactNode, Ref } from "react";
import { MenuHighlight } from "../lib/menu-highlight";
import { SiliconLink } from "../lib/silicon-provider";
import { Badge } from "../badge/badge";
import styles from "./nav-list.module.css";

type Box = { top: number; height: number; danger: boolean; glide: boolean };
const ITEM = "[data-nav-item]";
const boxOf = (item: HTMLElement): Omit<Box, "glide"> => ({ top: item.offsetTop, height: item.offsetHeight, danger: false });

/** What NavList's onNavigate receives: the chosen row's href, or none for an action row. */
export interface NavChoice { href?: string }

/** Lets NavItem reach its list without prop drilling: the active marker's position, and reporting a choice. */
const NavContext = createContext<{ activeChanged: () => void; navigate: (choice: NavChoice) => void } | null>(null);

export interface NavListProps {
  /** Names the navigation for screen readers, such as "Main" or "Settings". */
  label: string;
  /** NavItem and NavSection elements, in order. */
  children: ReactNode;
  /** Placement only (width, margins, grid area); the rows' look comes from the component. */
  className?: string;
  /** The `<nav>` element. */
  ref?: Ref<HTMLElement>;
  /**
   * Called when any row is chosen (click, Enter or Space; link and action rows), before it navigates or acts. A notification,
   * such as closing a phone sheet: it cannot cancel. Locked rows and modified clicks (a new tab or window) do not call it.
   */
  onNavigate?: (choice: NavChoice) => void;
}

/**
 * A sidebar's list of destinations: a `<nav>` with a list of NavItem rows, optionally grouped by NavSection. One hover highlight
 * glides between rows for the pointer, and the current page's marker glides when the page changes.
 */
export function NavList({ label, children, className, ref, onNavigate }: NavListProps) {
  const root = useRef<HTMLElement | null>(null);
  const [hover, setHover] = useState<Box | null>(null);
  const [active, setActive] = useState<Box | null>(null);
  const pointer = useRef(false);
  // The latest handler, read at choice time, so a new function each render never changes the context.
  const navigateHandler = useRef(onNavigate);
  useEffect(() => { navigateHandler.current = onNavigate; });

  const setRoot = useCallback((node: HTMLElement | null) => {
    root.current = node;
    if (typeof ref === "function") return ref(node);
    if (ref) ref.current = node;
  }, [ref]);

  // The active marker follows the item with aria-current. It glides only when the page changes, never on first paint.
  const measureActive = useCallback(() => {
    const item = root.current?.querySelector<HTMLElement>(`${ITEM}[aria-current="page"]`);
    setActive(current => {
      if (!item) return null;
      const next = boxOf(item);
      return current && current.top === next.top && current.height === next.height ? current : { ...next, glide: current !== null };
    });
  }, []);
  useLayoutEffect(() => {
    measureActive();
    const node = root.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    // A resize (a late font, a collapsing section) moves the marker at once; only a page change glides.
    const observer = new ResizeObserver(() => {
      const item = node.querySelector<HTMLElement>(`${ITEM}[aria-current="page"]`);
      setActive(current => {
        if (!item) return null;
        const next = boxOf(item);
        return current && current.top === next.top && current.height === next.height ? current : { ...next, glide: false };
      });
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [measureActive]);

  function show(target: EventTarget | null, glide: boolean) {
    const item = target instanceof HTMLElement ? target.closest<HTMLElement>(ITEM) : null;
    if (!item || item.getAttribute("aria-disabled") === "true") return setHover(null);
    setHover(current => ({ ...boxOf(item), glide: glide && current !== null }));
  }

  const context = useMemo(() => ({ activeChanged: measureActive, navigate: (choice: NavChoice) => navigateHandler.current?.(choice) }), [measureActive]);
  return (
    <NavContext.Provider value={context}>
      <nav ref={setRoot} aria-label={label} className={[styles.list, className].filter(Boolean).join(" ")}
        onPointerOver={(event: PointerEvent<HTMLElement>) => { pointer.current = true; show(event.target, true); }}
        onPointerLeave={() => setHover(null)}
        onKeyDownCapture={() => { pointer.current = false; }}
        onFocus={(event: FocusEvent<HTMLElement>) => { if (!pointer.current && event.target.matches(":focus-visible")) show(event.target, false); }}
        onBlur={(event: FocusEvent<HTMLElement>) => { if (!pointer.current && !event.currentTarget.contains(event.relatedTarget as Node | null)) setHover(null); }}>
        {/* The current page's marker paints over the hover fill, so hovering the current row never lightens it. */}
        <MenuHighlight state={hover} className={styles.highlight} />
        <MenuHighlight state={active} className={styles.active} />
        <ul className={styles.items}>{children}</ul>
      </nav>
    </NavContext.Provider>
  );
}

export interface NavSectionProps {
  /** The section's heading, such as "Workspace". */
  label: string;
  /** The section's NavItem rows. */
  children: ReactNode;
}

/** A labelled group of NavItem rows inside a NavList. */
export function NavSection({ label, children }: NavSectionProps) {
  const id = useId();
  return (
    <li className={styles.section}>
      <div id={id} className={styles.sectionLabel}>{label}</div>
      <ul className={styles.items} aria-labelledby={id}>{children}</ul>
    </li>
  );
}

interface NavItemBase {
  /** The row's label. */
  children: ReactNode;
  /** A 16px icon before the label. */
  icon?: ReactNode;
  /** The current page: sets aria-current="page" and the active marker. */
  active?: boolean;
  /** Turns the row off. It stays focusable (aria-disabled) so keyboard users can reach it and hear why. */
  disabled?: boolean;
  /** Why the row is off, such as "Pro". Shown as a badge and read as the row's description. Only shown when disabled. */
  reason?: string;
  /** Content at the row's end, such as a count or an avatar. Hidden while a reason shows. */
  trailing?: ReactNode;
  /**
   * Called each time the pointer enters the row or it receives focus, before it is chosen: start loading the destination
   * (prefetch). It can fire more than once, so keep it idempotent. Not called while disabled.
   */
  onIntent?: () => void;
  /** Placement only; the row's look comes from the component. */
  className?: string;
}

export type NavItemProps = NavItemBase & (
  | { /** A destination, rendered with the SiliconProvider's link. */ href: string; onSelect?: never }
  | { href?: never; /** An action, such as opening search. */ onSelect: () => void }
);

/** One row of a NavList: a link by `href`, or an action by `onSelect`. */
export function NavItem({ children, icon, active = false, disabled = false, reason, trailing, onIntent, className, href, onSelect }: NavItemProps) {
  const nav = useContext(NavContext);
  const reasonId = useId();
  const showReason = disabled && reason;
  useLayoutEffect(() => { nav?.activeChanged(); }, [active, nav]);

  const content = <>
    {icon ? <span className={styles.icon} aria-hidden="true">{icon}</span> : null}
    <span className={styles.label}>{children}</span>
    {/* Hidden from the name and read once, as the description (aria-describedby reads hidden text). */}
    {showReason ? <Badge id={reasonId} size="sm" className={styles.reason} aria-hidden="true">{reason}</Badge>
      : trailing ? <span className={styles.trailing}>{trailing}</span> : null}
  </>;
  const shared = {
    className: [styles.item, className].filter(Boolean).join(" "),
    "data-nav-item": "",
    "aria-current": active ? "page" as const : undefined,
    "aria-disabled": disabled || undefined,
    "aria-describedby": showReason ? reasonId : undefined,
  };

  if (disabled) {
    // No href and no action: it can be focused and read, but not followed.
    return <li><span {...shared} role={href ? "link" : "button"} tabIndex={0} onClick={(event: MouseEvent<HTMLElement>) => event.preventDefault()}>{content}</span></li>;
  }
  const intent = onIntent ? { onPointerEnter: onIntent, onFocus: onIntent } : {};
  if (href !== undefined) {
    // A modified click opens a new tab or window, so this page stays where it is: no choice to report.
    const onClick = (event: MouseEvent<HTMLAnchorElement>) => { if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) nav?.navigate({ href }); };
    return <li><SiliconLink {...shared} {...intent} href={href} onClick={onClick}>{content}</SiliconLink></li>;
  }
  return <li><button {...shared} {...intent} type="button" onClick={() => { nav?.navigate({}); onSelect?.(); }}>{content}</button></li>;
}

export default NavList;
