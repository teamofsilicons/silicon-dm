/**
 * Files from a paste or a drop. A paste carries files and text together more often than one would think: a screenshot
 * is a file alone, a picture copied in a browser is a file (and HTML), a file copied in the Finder is the file and its
 * name as text, and cells copied from a spreadsheet are text and a picture of them. So the files win when there is no
 * other text than their names; otherwise the text is pasted as text (what a person copying cells wants).
 *
 * A pasted picture usually arrives as "image.png": it is renamed "Pasted image 2026-10-17 at 15.32.08.png", so the
 * Carbon's Briefcase and the message say what it is. Pure enough to test with plain objects.
 */

export interface ClipboardLike {
  files: ArrayLike<File>;
  getData: (format: string) => string;
}

const GENERIC = /^(image|blob|untitled|clipboard)?\.?(png|jpe?g|gif|webp|heic|tiff?|bmp)$|^(image|blob|untitled|clipboard)$/i;

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/** "Pasted image 2026-10-17 at 15.32.08.png": the local date and time of the paste. */
export function pastedName(file: Pick<File, "name" | "type">, now: Date = new Date()): string {
  const extension = /\.([a-z0-9]{1,8})$/i.exec(file.name)?.[1] ?? file.type.split("/")[1]?.replace("jpeg", "jpg").replace(/[^a-z0-9]/gi, "") ?? "png";
  const stamp = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())} at ${pad(now.getHours())}.${pad(now.getMinutes())}.${pad(now.getSeconds())}`;
  return `Pasted image ${stamp}.${extension.toLowerCase()}`;
}

/** Gives a pasted picture with a generic name a name of its own (other files keep theirs). */
export function namedFile(file: File, now: Date = new Date()): File {
  if (!file.type.startsWith("image/") || (file.name && !GENERIC.test(file.name))) return file;
  return new File([file], pastedName(file, now), { type: file.type, lastModified: file.lastModified });
}

/** The files a paste should attach, or [] when it should paste its text (see the module comment). */
export function filesToAttach(data: ClipboardLike, now: Date = new Date()): File[] {
  const files = Array.from(data.files);
  if (!files.length) return [];
  const text = data.getData("text/plain").trim();
  if (text) {
    const names = new Set(files.map(file => file.name));
    const lines = text.split(/\r?\n/).map(line => line.trim()).filter(Boolean);
    if (!lines.every(line => names.has(line) || names.has(line.split("/").pop() ?? ""))) return [];
  }
  return files.map(file => namedFile(file, now));
}
