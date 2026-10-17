"use client";

/* eslint-disable @next/next/no-img-element -- GIFs are Giphy's own https media, shown as they are (no image proxy). */

/**
 * The GIF picker (spec §6.5): a popover above the field with a search (debounced), your recent GIFs and the trending
 * ones (DM's service asks Giphy: lib/chat/api.ts), in a grid the keyboard walks: ↓ from the search goes into the grid,
 * the arrows move (↑/↓ by row, as the tiles sit), Enter picks, Esc closes and gives the keyboard back to the field.
 * Enter in the search picks the first GIF for what was typed (never one still on screen from before: it waits for the
 * results, asking at once). Picking puts the GIF in the tray; Enter then sends it. Under reduced motion the tiles show
 * Giphy's still frames. Offline it says so instead of loading forever. "Powered by GIPHY" is Giphy's own condition for
 * using its search.
 *
 * While it is open the others see "looking for a GIF…" (reportActivity, throttled in lib/chat/presence.ts).
 */
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { useReducedMotion } from "motion/react";
import { Button } from "@/components/silicon-ui/button/button";
import { Popover, PopoverContent } from "@/components/silicon-ui/popover/popover";
import { SearchField } from "@/components/silicon-ui/search-field/search-field";
import { recentGifs, searchGifs, trendingGifs } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import { reportActivity } from "@/lib/chat/presence";
import type { Gif } from "@/lib/chat/types";
import { plainFailure } from "../thread/thread-model";
import styles from "./composer.module.css";

/** What the picker opens above (the composer's field). */
export const GifAnchor = PopoverPrimitive.Anchor;

const SEARCH_DEBOUNCE_MS = 300;
const RECENT_SHOWN = 6;

/** Giphy's still frame of a rendition (`200w.gif` → `200w_s.gif`), for reduced motion; null for other hosts. */
export function gifStill(url: string | undefined): string | null {
  if (!url) return null;
  const match = /^(https:\/\/media\d*\.giphy\.com\/media\/[^/]+\/)([a-z0-9_]+?)(_s)?\.gif(\?.*)?$/i.exec(url);
  if (!match) return null;
  return `${match[1]}${match[2]}_s.gif${match[4] ?? ""}`;
}

/** The tile to move to from `index` with an arrow key, from where the tiles sit on screen (rows wrap with the width). */
export function nextTile(boxes: Array<{ left: number; top: number; width: number; height: number }>, index: number, key: string): number {
  if (!boxes.length) return -1;
  if (key === "Home") return 0;
  if (key === "End") return boxes.length - 1;
  if (key === "ArrowRight") return Math.min(boxes.length - 1, index + 1);
  if (key === "ArrowLeft") return Math.max(0, index - 1);
  const here = boxes[index]!;
  const middle = here.left + here.width / 2;
  const below = key === "ArrowDown";
  let best = -1;
  let bestRow = Infinity;
  let bestDistance = Infinity;
  boxes.forEach((box, at) => {
    const row = below ? box.top - (here.top + here.height / 2) : here.top + here.height / 2 - (box.top + box.height);
    if (at === index || row < 0) return;
    const distance = Math.abs(box.left + box.width / 2 - middle);
    // The nearest row first, then the tile nearest the same column in it.
    if (row < bestRow - 4 || (Math.abs(row - bestRow) <= 4 && distance < bestDistance)) {
      best = at;
      bestRow = row;
      bestDistance = distance;
    }
  });
  return best;
}

/** The search as it is asked for: `value` after `ms` without typing (at once when cleared), and a way to ask now. */
function useDebounced(value: string, ms: number): [string, () => void] {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return [value ? debounced : "", () => setDebounced(value)];
}

function GifTiles({ gifs, label, start, active, reduced, onPick, onFocusTile }: { gifs: Gif[]; label: string; start: number; active: number; reduced: boolean; onPick: (gif: Gif) => void; onFocusTile: (index: number) => void }) {
  // A GIF Giphy did not deliver (offline, blocked) keeps its tile with the word in it, never an empty box.
  const [broken, setBroken] = useState<ReadonlySet<string>>(() => new Set());
  return (
    <div className={styles.gifGrid} role="group" aria-label={label}>
      {gifs.map((gif, at) => {
        const index = start + at;
        const preview = gif.preview_url ?? gif.url;
        const still = reduced ? gifStill(preview) ?? gifStill(gif.url) : null;
        const source = still ?? preview;
        return (
          <button
            key={`${gif.provider_id}-${index}`}
            type="button"
            className={styles.gifTile}
            // It draws its own hover ring (::after), so its curve is the native one (DESIGN.md: data-sq-native).
            data-sq-native=""
            data-gif-tile=""
            tabIndex={index === active ? 0 : -1}
            aria-label={gif.title?.trim() || "GIF"}
            onFocus={() => onFocusTile(index)}
            onClick={() => onPick(gif)}
          >
            {(reduced && !still) || broken.has(source) ? (
              <span className={styles.gifTileWord} aria-hidden="true">GIF</span>
            ) : (
              <img src={source} alt="" loading="lazy" decoding="async" draggable={false} onError={() => setBroken(known => new Set(known).add(source))} />
            )}
          </button>
        );
      })}
    </div>
  );
}

function GifPanel({ initialQuery, onPick }: { initialQuery: string; onPick: (gif: Gif) => void }) {
  const [query, setQuery] = useState(initialQuery);
  const [active, setActive] = useState(0);
  const typed = query.trim().slice(0, 50);
  const [search, searchNow] = useDebounced(typed, SEARCH_DEBOUNCE_MS);
  const grid = useRef<HTMLDivElement>(null);
  // Enter pressed before the results for what was typed arrived: the first of them is picked when they do.
  const pickWhenShown = useRef<string | null>(null);
  const reduced = !!useReducedMotion();

  const trending = useQuery({ queryKey: chatKeys.gifs("trending"), queryFn: ({ signal }) => trendingGifs(signal), staleTime: 10 * 60_000, enabled: !search, meta: { toast: false } });
  const recent = useQuery({ queryKey: chatKeys.gifs("recent"), queryFn: ({ signal }) => recentGifs(signal), staleTime: 30_000, enabled: !search, retry: false, meta: { toast: false } });
  const found = useQuery({ queryKey: chatKeys.gifs("search", search), queryFn: ({ signal }) => searchGifs(search, signal), staleTime: 5 * 60_000, enabled: !!search, placeholderData: previous => previous, meta: { toast: false } });

  useEffect(() => {
    reportActivity("searching_gifs");
  }, [query]);

  const recentItems = search ? [] : (recent.data?.items ?? []).slice(0, RECENT_SHOWN);
  const main = search ? found.data?.items ?? [] : trending.data?.items ?? [];
  const all = [...recentItems, ...main];
  const loading = search ? found.isPending : trending.isPending;
  const failure = search ? found.error : trending.error;
  // Offline, the queries wait for the connection ("paused") instead of failing: the picker says so.
  const offline = (search ? found.fetchStatus : trending.fetchStatus) === "paused";
  const current = Math.min(active, Math.max(0, all.length - 1));
  // What is on screen answers what was typed (not the trending GIFs, nor the last search still shown meanwhile).
  const shownFor = search === typed && (search ? found.isSuccess && !found.isPlaceholderData : trending.isSuccess) ? typed : null;
  const first = all[0];

  useEffect(() => {
    if (pickWhenShown.current === null || shownFor !== pickWhenShown.current) return;
    pickWhenShown.current = null;
    if (first) onPick(first);
  }, [shownFor, first, onPick]);

  const tiles = () => Array.from(grid.current?.querySelectorAll<HTMLButtonElement>("[data-gif-tile]") ?? []);
  const focusTile = (index: number) => tiles()[index]?.focus();

  const onGridKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    const list = tiles();
    const index = list.indexOf(document.activeElement as HTMLButtonElement);
    if (index < 0) return;
    event.preventDefault();
    const next = nextTile(list.map(tile => tile.getBoundingClientRect()), index, event.key);
    if (next >= 0) focusTile(next);
    else if (event.key === "ArrowUp") document.getElementById("gif-search")?.focus();
  };

  return (
    <div className={styles.gifBody}>
      <SearchField
        id="gif-search"
        label="Search GIFs"
        value={query}
        onValueChange={value => {
          setQuery(value);
          setActive(0);
          pickWhenShown.current = null;
        }}
        placeholder="Search GIPHY"
        autoComplete="off"
        maxLength={50}
        onKeyDown={event => {
          if (event.key === "ArrowDown" && all.length) {
            event.preventDefault();
            focusTile(current);
          } else if (event.key === "Enter") {
            event.preventDefault();
            // Enter picks the first GIF for what was typed: now when it is on screen, else as soon as it arrives.
            if (shownFor === typed && first) onPick(first);
            else {
              pickWhenShown.current = typed;
              searchNow();
            }
          }
        }}
      />
      <div ref={grid} className={styles.gifScroll} onKeyDown={onGridKey}>
        {failure ? (
          <div className={styles.gifState} role="alert">
            <p>GIFs could not be loaded: {plainFailure(failure).message}</p>
            <Button size="sm" variant="secondary" onClick={() => void (search ? found.refetch() : trending.refetch())}>Try again</Button>
          </div>
        ) : offline && !all.length ? (
          <p className={styles.gifState} role="status">You are offline. GIFs show here when you are back.</p>
        ) : loading && !all.length ? (
          <div className={styles.gifGrid} aria-busy="true" aria-label="Loading GIFs" role="status">
            {Array.from({ length: 9 }, (_, index) => <span key={index} className={styles.gifPlaceholder} data-sq="surface" />)}
          </div>
        ) : !all.length ? (
          <p className={styles.gifState} role="status">{search ? `No GIFs for “${search}”. Try another word.` : "No GIFs to show right now."}</p>
        ) : (
          <>
            {recentItems.length ? <p className={styles.gifSection}>Recent</p> : null}
            {recentItems.length ? <GifTiles gifs={recentItems} label="Recent GIFs" start={0} active={current} reduced={reduced} onPick={onPick} onFocusTile={setActive} /> : null}
            {recentItems.length ? <p className={styles.gifSection}>{search ? "Results" : "Trending"}</p> : null}
            <GifTiles gifs={main} label={search ? `GIFs for ${search}` : "Trending GIFs"} start={recentItems.length} active={current} reduced={reduced} onPick={onPick} onFocusTile={setActive} />
          </>
        )}
      </div>
      <p className={styles.gifCredit}>Powered by GIPHY</p>
    </div>
  );
}

export interface GifPickerProps {
  /** null while closed; the search it opens with otherwise ("/gif cats" opens with "cats"). */
  open: { query: string } | null;
  onClose: () => void;
  onPick: (gif: Gif) => void;
  /** Where the keyboard goes back to when it closes (the field). */
  onClosed: () => void;
  /** The anchor (<GifAnchor asChild>…</GifAnchor>) and whatever sits around it. */
  children: ReactNode;
}

export function GifPicker({ open, onClose, onPick, onClosed, children }: GifPickerProps) {
  // The panel keeps its content while it fades out (it never shrinks to an empty box); each opening starts afresh.
  // (`open` is a new object for every opening: the composer makes one each time.)
  const [shown, setShown] = useState<{ opening: object; query: string; round: number } | null>(null);
  if (open && shown?.opening !== open) setShown({ opening: open, query: open.query, round: (shown?.round ?? 0) + 1 });
  return (
    <Popover open={!!open} onOpenChange={next => { if (!next) onClose(); }}>
      {children}
      <PopoverContent
        side="top"
        align="start"
        sideOffset={8}
        className={styles.gifPanel}
        aria-label="Choose a GIF"
        onOpenAutoFocus={event => {
          event.preventDefault();
          requestAnimationFrame(() => document.getElementById("gif-search")?.focus());
        }}
        // Esc: the keyboard goes back to the field at once, not after the panel has faded.
        onEscapeKeyDown={() => requestAnimationFrame(onClosed)}
        onCloseAutoFocus={event => {
          event.preventDefault();
          // After a click elsewhere the keyboard stays where the Carbon put it; only a lost focus goes back to the field.
          const active = document.activeElement;
          if (!active || active === document.body) onClosed();
        }}
      >
        {shown ? <GifPanel key={shown.round} initialQuery={shown.query} onPick={onPick} /> : null}
      </PopoverContent>
    </Popover>
  );
}
