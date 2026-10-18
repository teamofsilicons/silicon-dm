/**
 * The empty field's words, "Message <name>", fitted to the room the field has (./composer.tsx measures it): the name
 * whole when it fits, else cut at a word (one long word: at a character) with an ellipsis. A browser draws a textarea's
 * placeholder cut at its edge, through a letter, whatever `text-overflow` says, so the cut is made here. The field's
 * name for screen readers stays whole (its aria-label).
 */

/** The characters a person sees (a letter with its accents, an emoji with its modifiers), so a cut never splits one. */
function graphemes(text: string): string[] {
  if (typeof Intl.Segmenter !== "function") return [...text];
  return Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text), part => part.segment);
}

/** A cut at a word keeps at least this many characters (else the name is cut at a character). */
const WORD_CUT_MIN = 4;

/**
 * The longest `${prefix}${name}` that `measure` draws within `room` pixels: the name whole, else its longest start that
 * fits with "…" after it, taken back to a word's end when one is far enough in. Room unknown (0): the words whole.
 */
export function fitPlaceholder(prefix: string, name: string, room: number, measure: (text: string) => number): string {
  const whole = `${prefix}${name}`;
  if (!(room > 0) || measure(whole) <= room) return whole;
  const parts = graphemes(name);
  const cut = (count: number) => parts.slice(0, count).join("").trimEnd();
  const fits = (count: number) => measure(`${prefix}${cut(count)}…`) <= room;
  // The widest start that fits (the width grows with every character, so a binary search finds it).
  let low = 0;
  let high = parts.length - 1;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (fits(middle)) low = middle;
    else high = middle - 1;
  }
  let head = cut(low);
  const space = head.lastIndexOf(" ");
  if (space >= WORD_CUT_MIN && parts[low] !== " ") head = head.slice(0, space).trimEnd();
  return head ? `${prefix}${head}…` : `${prefix.trimEnd()}…`;
}
