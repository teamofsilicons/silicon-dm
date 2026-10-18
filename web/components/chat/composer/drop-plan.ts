/**
 * What the composer does with a drop (./composer.tsx takeDrop), decided apart from React so tests can pin it down
 * (tests/upload-drop.test.ts). Links attach at once. Files dropped one by one go in at once: the Carbon chose each of
 * them. The files found in dropped folders go in at once too, unless the folders bring more than ASK_ABOVE_FILES: then
 * they wait for a yes, because nobody saw a folder's contents (a drop of a whole projects folder should not start a
 * thousand uploads). The question counts the folders' files only, and says what the walk left out (empty files, ones
 * this browser could not read), so Cancel takes back exactly what it names. Everything else the drop has to say (a
 * folder that could not be read, one with nothing to send, uploads being off) is said at once without a question, and
 * once the question is answered with one, so the question never hides it.
 */
import { foldersWords, leftOutWords, type Dropped } from "@/lib/upload/drop";
import { ASK_ABOVE_FILES, TRAY_MAX_ITEMS } from "./tray";

export interface DropQuestion {
  /** What is asked, in one sentence. */
  words: string;
  /** The yes button's words ("Upload 25 files"). */
  action: string;
  /** What a yes adds. */
  files: File[];
}

export interface DropPlan {
  /** https links to attach. */
  links: string[];
  /** Files to add now. */
  add: File[];
  /** The folders' files, waiting for a yes; null when nothing is asked. */
  question: DropQuestion | null;
  /** What to say: at once without a question, else once it is answered. */
  sayings: string[];
}

export interface DropPlanOptions {
  /** How many more items the message takes (TRAY_MAX_ITEMS less what the tray already holds). */
  room: number;
  /** Why files cannot be uploaded here (links still go), or null. */
  uploadsOff: string | null;
}

export function planDrop(dropped: Dropped, { room, uploadsOff }: DropPlanOptions): DropPlan {
  const sayings = [...dropped.problems];
  const empty = dropped.folders.filter(folder => folder.files === 0);
  if (empty.length) sayings.push(`${foldersWords(empty)} ${empty.length === 1 ? "has" : "have"} no files to send${empty.some(folder => folder.hidden || folder.empty) ? " (hidden and empty files are left out)" : ""}.`);
  const full = dropped.folders.filter(folder => folder.files > 0);
  const plan: DropPlan = { links: dropped.links, add: [], question: null, sayings };
  if (uploadsOff) {
    if (dropped.loose.length || dropped.inFolders.length) sayings.push(uploadsOff);
    return plan;
  }
  plan.add = [...dropped.loose];
  if (dropped.inFolders.length <= ASK_ABOVE_FILES) {
    plan.add.push(...dropped.inFolders);
    const left = leftOutWords(full);
    if (left) sayings.push(left);
    return plan;
  }
  // Room after the loose files, which go in first.
  const count = Math.min(Math.max(0, room - plan.add.length), dropped.inFolders.length);
  if (!count) {
    sayings.push(`This message already carries ${TRAY_MAX_ITEMS} attachments, the most it takes. Send it first.`);
    return plan;
  }
  const total = dropped.inFolders.length.toLocaleString("en-US");
  const many = full.some(folder => folder.truncated) ? `more than ${total}` : total;
  const left = leftOutWords(full, "inline");
  const what = `${foldersWords(full)} ${full.length === 1 ? "has" : "have"} ${many} files${left ? ` (${left})` : ""}`;
  plan.question = {
    words: count < dropped.inFolders.length ? `${what}, and a message carries up to ${TRAY_MAX_ITEMS} attachments. Upload the first ${count}?` : `${what}. Upload all of them?`,
    action: `Upload ${count} files`,
    files: dropped.inFolders.slice(0, count),
  };
  return plan;
}
