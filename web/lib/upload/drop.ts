/**
 * What a drop brings, read the way each source gives it:
 *
 * - files from the desktop or another app: the files themselves;
 * - folders: every file inside them, through the entries API (`webkitGetAsEntry`, in every current browser), with hidden
 *   files (".DS_Store", ".git/…") and empty ones left out (each counted, and said); the composer asks before adding more
 *   than a few (a folder's contents were never seen by the Carbon);
 * - a picture or a link dragged from a web page: usually only its address (text/uri-list) and some HTML, not its
 *   bytes. An https address is attached as a link, never fetched by this website (fetching an address someone else
 *   chose from DM's server would let a page make DM's server call anywhere); a picture inlined in the page (a data:
 *   address) is already here, so it becomes a file to upload;
 * - files dragged from an app that hands a page only their addresses on disk (file://…) cannot be read: said so;
 * - anything else is said in words (`problems`), so a drop is never silently nothing.
 *
 * `snapshotDrop` must run inside the drop event (the browser empties the DataTransfer when the event ends);
 * `readDrop` then walks folders without hurry. Pure apart from those browser objects, which tests fake
 * (tests/upload-drop.test.ts).
 */

/** The parts of the entries API this reads (FileSystemEntry and its file and directory kinds). */
export interface EntryLike {
  isFile: boolean;
  isDirectory: boolean;
  name: string;
  file?: (success: (file: File) => void, failure?: (error: unknown) => void) => void;
  createReader?: () => { readEntries: (success: (entries: EntryLike[]) => void, failure?: (error: unknown) => void) => void };
}

/** The parts of a DataTransferItem this reads. */
export interface ItemLike {
  kind: string;
  getAsFile: () => File | null;
  webkitGetAsEntry?: () => EntryLike | null;
}

/** The parts of a DataTransfer this reads. */
export interface DataTransferLike {
  items?: ArrayLike<ItemLike>;
  files: ArrayLike<File>;
  types: ArrayLike<string>;
  getData: (format: string) => string;
}

/** What `snapshotDrop` keeps of a drop (the browser forgets the rest when the event ends). */
export interface DropSnapshot {
  files: File[];
  folders: EntryLike[];
  uriList: string;
  html: string;
  text: string;
}

export interface DroppedFolder {
  name: string;
  /** Files taken from it (all of them, unless `truncated`). */
  files: number;
  /** Hidden files and folders left out. */
  hidden: number;
  /** Empty files (0 bytes) left out: nothing to send, and a folder of code has many (an empty __init__.py). */
  empty?: number;
  /** Files (or folders inside it) the browser could not read, left out (a broken link, no permission). */
  unreadable?: number;
  /** It held more than `limit` files and only that many were read. */
  truncated: boolean;
}

export interface Dropped {
  /** The files, in the order dropped (a folder's files by their path). */
  files: File[];
  folders: DroppedFolder[];
  /** https addresses to attach as links. */
  links: string[];
  /** What could not be taken, each in a sentence a Carbon reads. */
  problems: string[];
}

/** Takes what the drop carries while the event lasts. */
export function snapshotDrop(data: DataTransferLike): DropSnapshot {
  const files: File[] = [];
  const folders: EntryLike[] = [];
  const items = data.items ? Array.from(data.items) : [];
  const fileItems = items.filter(item => item.kind === "file");
  if (fileItems.length) {
    for (const item of fileItems) {
      const entry = item.webkitGetAsEntry?.() ?? null;
      if (entry?.isDirectory) folders.push(entry);
      else {
        const file = item.getAsFile();
        if (file) files.push(file);
      }
    }
  } else {
    files.push(...Array.from(data.files));
  }
  const read = (format: string) => {
    try {
      return Array.from(data.types).includes(format) ? data.getData(format) : "";
    } catch {
      return "";
    }
  };
  return { files, folders, uriList: read("text/uri-list"), html: read("text/html"), text: read("text/plain") };
}

const fileOf = (entry: EntryLike) =>
  new Promise<File>((resolve, reject) => {
    if (!entry.file) return reject(new Error("not a file"));
    entry.file(resolve, reject);
  });

/** Every entry of a folder: the reader answers in batches until an empty one. */
async function entriesOf(folder: EntryLike): Promise<EntryLike[]> {
  const reader = folder.createReader?.();
  if (!reader) return [];
  const all: EntryLike[] = [];
  for (;;) {
    const batch = await new Promise<EntryLike[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (!batch.length) return all;
    all.push(...batch);
  }
}

const hidden = (name: string) => name.startsWith(".");

interface WalkCounts {
  hidden: number;
  empty: number;
  unreadable: number;
  truncated: boolean;
}

/**
 * The files of a folder and its folders, by path, stopping after `limit`. Hidden and empty files are left out and
 * counted; a file or an inner folder the browser cannot read is counted and the rest still taken (the dropped folder
 * itself unreadable is the caller's to say).
 */
async function walk(folder: EntryLike, out: File[], counts: WalkCounts, limit: number): Promise<void> {
  const entries = (await entriesOf(folder)).sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
  for (const entry of entries) {
    if (out.length >= limit) {
      counts.truncated = true;
      return;
    }
    if (hidden(entry.name)) {
      counts.hidden += 1;
      continue;
    }
    if (entry.isDirectory) {
      try {
        await walk(entry, out, counts, limit);
      } catch {
        counts.unreadable += 1;
      }
    } else if (entry.isFile) {
      let file: File;
      try {
        file = await fileOf(entry);
      } catch {
        counts.unreadable += 1;
        continue;
      }
      if (file.size === 0) counts.empty += 1;
      else out.push(file);
    }
  }
}

/** An https address as it would be attached (credentials and other schemes refused). */
export function httpsLink(raw: string): string | null {
  const value = raw.trim();
  if (!value || /\s/.test(value)) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" || !url.hostname || url.username || url.password || url.href.length > 8192) return null;
    return url.href;
  } catch {
    return null;
  }
}

/** The largest picture taken from a data: address (the whole address is in memory already; this bounds the copy). */
const DATA_PICTURE_MAX = 32 * 1024 * 1024;
const DATA_PICTURE = /^data:(image\/(?:png|jpeg|gif|webp|avif|bmp));base64,([a-z0-9+/=\s]+)$/i;

/** A picture inlined in a page (data:image/…;base64,…) as a file, or null. */
export function pictureFromDataUrl(address: string, now: Date = new Date()): File | null {
  if (address.length > (DATA_PICTURE_MAX * 4) / 3 + 64) return null;
  const match = DATA_PICTURE.exec(address.trim());
  if (!match) return null;
  try {
    const binary = atob(match[2]!.replace(/\s+/g, ""));
    const bytes = Uint8Array.from(binary, character => character.charCodeAt(0));
    const type = match[1]!.toLowerCase();
    const extension = type.split("/")[1]!.replace("jpeg", "jpg");
    const stamp = now.toISOString().slice(0, 19).replace("T", " at ").replace(/:/g, ".");
    return new File([bytes], `Dropped picture ${stamp}.${extension}`, { type });
  } catch {
    return null;
  }
}

/** Where a dragged picture or link points: the picture's own address first (what was dragged), then the link's. */
export function addressesOf(snapshot: Pick<DropSnapshot, "uriList" | "html" | "text">): string[] {
  const out: string[] = [];
  const image = /<img\b[^>]*?\bsrc\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))/i.exec(snapshot.html);
  const src = image ? (image[2] ?? image[3] ?? image[4] ?? "") : "";
  if (src) out.push(src.replace(/&amp;/g, "&"));
  for (const line of snapshot.uriList.split(/\r?\n/)) {
    const value = line.trim();
    if (value && !value.startsWith("#")) out.push(value);
  }
  if (!out.length && snapshot.text.trim() && !/\s/.test(snapshot.text.trim())) out.push(snapshot.text.trim());
  return [...new Set(out)];
}

export interface ReadOptions {
  /** The most files read from folders (a drop of a whole disk stops there). */
  limit?: number;
  now?: Date;
}

/** Walks the folders of a drop and reads the addresses it carried. */
export async function readDrop(snapshot: DropSnapshot, options: ReadOptions = {}): Promise<Dropped> {
  const limit = options.limit ?? 10_000;
  const files = [...snapshot.files];
  const folders: DroppedFolder[] = [];
  const problems: string[] = [];
  for (const folder of snapshot.folders) {
    const found: File[] = [];
    const counts: WalkCounts = { hidden: 0, empty: 0, unreadable: 0, truncated: false };
    try {
      await walk(folder, found, counts, limit);
    } catch {
      problems.push(`The folder ${folder.name} could not be read: this browser was not given access to its files. Drop the files themselves instead.`);
      continue;
    }
    folders.push({ name: folder.name, files: found.length, hidden: counts.hidden, ...(counts.empty ? { empty: counts.empty } : {}), ...(counts.unreadable ? { unreadable: counts.unreadable } : {}), truncated: counts.truncated });
    files.push(...found);
  }
  const links: string[] = [];
  if (!snapshot.files.length && !snapshot.folders.length) {
    const addresses = addressesOf(snapshot);
    for (const address of addresses) {
      if (/^data:/i.test(address)) {
        const picture = pictureFromDataUrl(address, options.now);
        if (picture) files.push(picture);
        else problems.push("The dragged picture could not be taken from its page. Save it, then drop the file.");
        break;
      }
      const link = httpsLink(address);
      if (link) {
        links.push(link);
        break;
      }
    }
    if (!files.length && !links.length) {
      problems.push(
        addresses.some(address => /^file:/i.test(address))
          ? "That app gave only the files' addresses (file://…), which a web page cannot open. Drag them from your file manager, or use +\u00a0→\u00a0Upload a file."
          : addresses.some(address => /^http:/i.test(address))
            ? "Only https links can be attached, and this one is not. Copy it into the message instead."
            : addresses.length
              ? "What was dropped is neither a file nor an https link, so nothing was added. Save it as a file, then drop that."
              : "Nothing that can be sent was dropped: drop files, a folder or an https link.",
      );
    }
  }
  return { files, folders, links, problems };
}

/** "Photos", "Photos and Scans", "3 folders": how a question names the dropped folders. */
export function foldersWords(folders: Pick<DroppedFolder, "name">[]): string {
  if (folders.length === 1) return folders[0]!.name;
  if (folders.length === 2) return `${folders[0]!.name} and ${folders[1]!.name}`;
  return `${folders.length} folders`;
}

/** "One empty file was left out.", "3 empty files and one file this browser could not read were left out.": what a folder walk skipped. */
export function leftOutWords(folders: Pick<DroppedFolder, "empty" | "unreadable">[]): string | null {
  const empty = folders.reduce((sum, folder) => sum + (folder.empty ?? 0), 0);
  const unreadable = folders.reduce((sum, folder) => sum + (folder.unreadable ?? 0), 0);
  const parts = [
    ...(empty ? [empty === 1 ? "one empty file" : `${empty.toLocaleString("en-US")} empty files`] : []),
    ...(unreadable ? [unreadable === 1 ? "one file this browser could not read" : `${unreadable.toLocaleString("en-US")} files this browser could not read`] : []),
  ];
  if (!parts.length) return null;
  const words = parts.join(" and ");
  return `${words[0]!.toUpperCase()}${words.slice(1)} ${empty + unreadable === 1 ? "was" : "were"} left out.`;
}
