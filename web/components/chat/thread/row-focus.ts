/**
 * Which message's controls are in the Tab order. The thread is one stop for Tab (the log); ↑/↓ choose a message, and
 * Tab then walks the chosen message's own controls (names, links, attachments, a code block that scrolls, the voice
 * bubble, the toolbar) before leaving the thread. Without this, Tab would stop at every link of every message on the way
 * to the composer, and scroll the thread to each of them.
 *
 * It is one pass over a row's own subtree after each of its renders, so it also covers controls drawn by other parts
 * (the voice area's bubble, Silicon UI's copy buttons) without them knowing: each control's own tabindex is kept in
 * `data-row-tab` while the row is not selected and put back when it is. A part that adds a control later, while its row
 * is not selected, writes it in that same form (rich-text.tsx's code blocks do). An element marked
 * `data-row-keep-tab` is left alone.
 */
const FOCUSABLE = "a[href], button, input, select, textarea, audio[controls], video[controls], [tabindex]:not([data-row-keep-tab])";

export function applyRowTabbing(row: HTMLElement | null, tabbable: boolean): void {
  if (!row) return;
  for (const element of row.querySelectorAll<HTMLElement>(FOCUSABLE)) {
    if (element === row) continue;
    const saved = element.getAttribute("data-row-tab");
    if (!tabbable) {
      if (saved === null) element.setAttribute("data-row-tab", element.getAttribute("tabindex") ?? "");
      if (element.getAttribute("tabindex") !== "-1") element.setAttribute("tabindex", "-1");
    } else if (saved !== null) {
      element.removeAttribute("data-row-tab");
      if (saved === "") element.removeAttribute("tabindex");
      else element.setAttribute("tabindex", saved);
    }
  }
}
