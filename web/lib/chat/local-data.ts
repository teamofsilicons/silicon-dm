/**
 * What the chat keeps of a Carbon's own words in this browser: unsent messages (`dm:outbox:v1:<account>`), drafts
 * (`dm:draft:v1:<account>:<address>`), the files and links waiting in a composer's tray (`dm:tray:v1:<account>:…`), which
 * conversation each tab shows (`dm:screen:v1:<account>`), the shapes of the photos seen (`dm:photo-shapes:v1`, keyed
 * by their links) and the sync position of each tab. It belongs to the account signed in: signing out removes it (after
 * saying how many messages have not gone), and an account signing in removes what another account left here, as the
 * voice area does with recordings. Otherwise the next person at a shared computer could read it, and messages left
 * waiting went out the next time their Carbon signed in here, maybe days later.
 */
import { freezeStorage, keysStartingWith, removeKeys } from "./storage";

const PER_ACCOUNT = ["dm:outbox:v1:", "dm:draft:v1:", "dm:tray:v1:", "dm:screen:v1:"];
const SHARED = ["dm:photo-shapes:v1"];
const SESSION = ["dm:sync-cursor:v1:"];

/** The account a per-account key belongs to (`dm:draft:v1:<account>:<address>` → `<account>`). */
function ownerOf(key: string): string | null {
  const prefix = PER_ACCOUNT.find(entry => key.startsWith(entry));
  if (!prefix) return null;
  const rest = key.slice(prefix.length);
  const end = rest.indexOf(":");
  return end < 0 ? rest : rest.slice(0, end);
}

/** Signing out: every account's words and positions leave this browser, and the page writes nothing more. */
export function forgetChatData(): void {
  freezeStorage();
  removeKeys(keysStartingWith([...PER_ACCOUNT, ...SHARED]));
  removeKeys(keysStartingWith(SESSION, "session"), "session");
}

/** `account` signed in: what any other account left in this browser goes (its own stays). */
export function forgetOtherAccounts(account: string): void {
  removeKeys(keysStartingWith(PER_ACCOUNT).filter(key => ownerOf(key) !== account));
}
