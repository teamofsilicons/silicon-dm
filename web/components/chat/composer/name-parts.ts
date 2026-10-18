/**
 * How a long file name is cut on screen: in its middle, so its beginning and its end with the extension stay
 * readable (Finder's way). Pure: tests/composer-slash.test.ts checks it; tray-view.tsx draws the two parts.
 */

const graphemes = (text: string): string[] => {
  if (typeof Intl !== "undefined" && "Segmenter" in Intl) return [...new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)].map(part => part.segment);
  return [...text];
};

/**
 * A name in two parts for cutting in the middle: everything but its end, which may be cut with an ellipsis, and its
 * last four characters with the extension, which never are ("note-07-…tesa.txt": names that differ early, as most
 * do, still differ on screen). Short names are one part.
 */
export function nameParts(name: string, tail = 4): { head: string; end: string } {
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 && name.length - dot <= 9 ? name.slice(dot) : "";
  const stem = graphemes(name.slice(0, name.length - extension.length));
  if (stem.length <= tail + 4) return { head: name, end: "" };
  return { head: stem.slice(0, stem.length - tail).join(""), end: `${stem.slice(stem.length - tail).join("")}${extension}` };
}
