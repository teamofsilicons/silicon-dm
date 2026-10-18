/**
 * Slash commands at the start of the message field (spec §6.5): `/gif <query>`, `/voice`, `/dictate`, `/upload`,
 * `/link <url>`. Typing "/" opens a small list of the commands that match; ↑/↓ move, Tab or Enter runs one, Esc closes
 * the list and leaves the text, so Enter then sends it as it is ("/shrug" stays a message). A command that takes no
 * words matches only on its own ("/voice please" is a message), and nothing matches once the text has a new line.
 *
 * Pure (no DOM, no React): tests/composer-slash.test.ts covers the parsing and what running each command does, with
 * the microphone's controls faked.
 */
import type { ComposerVoiceControls } from "./voice-bridge";

export type SlashName = "gif" | "voice" | "dictate" | "upload" | "link";

export interface SlashCommand {
  name: SlashName;
  /** How it is written, as the list shows it. */
  usage: string;
  /** What it does, in a few words. */
  description: string;
  /** Whether it takes words after it. */
  argument: "none" | "optional";
  /** The registry shortcut that does the same (its keys show in the list). */
  shortcut?: string;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "gif", usage: "/gif <search>", description: "Find a GIF", argument: "optional" },
  { name: "voice", usage: "/voice", description: "Record a voice message", argument: "none", shortcut: "composer.record" },
  { name: "dictate", usage: "/dictate", description: "Speak, and the words appear here", argument: "none", shortcut: "composer.dictate" },
  { name: "upload", usage: "/upload", description: "Upload a file", argument: "none", shortcut: "composer.upload" },
  { name: "link", usage: "/link <https://…>", description: "Add a link", argument: "optional" },
];

export interface SlashOption {
  command: SlashCommand;
  /** The words after the command ("" when none). */
  argument: string;
}

/**
 * The commands the field's text could mean, best first; [] when it is not a command at all. "/" lists every command,
 * "/g" the ones starting with g, "/gif cats" the GIF search for "cats".
 */
export function slashOptions(text: string): SlashOption[] {
  if (!text.startsWith("/") || /[\r\n]/.test(text)) return [];
  const match = /^\/([a-z]*)(?:(\s+)([\s\S]*))?$/i.exec(text);
  if (!match) return [];
  const word = match[1]!.toLowerCase();
  const spaced = match[2] !== undefined;
  const argument = (match[3] ?? "").trim();
  if (!spaced) return SLASH_COMMANDS.filter(command => command.name.startsWith(word)).map(command => ({ command, argument: "" }));
  const command = SLASH_COMMANDS.find(candidate => candidate.name === word);
  if (!command || (command.argument === "none" && argument)) return [];
  return [{ command, argument }];
}

/** A link as typed: "https://…" as is; "example.com/x" becomes https://example.com/x. Null when it is not an https link. */
export function normalizeLink(raw: string): string | null {
  const value = raw.trim();
  if (!value || /\s/.test(value)) return null;
  const candidate = /^[a-z][a-z0-9+.-]*:/i.test(value) ? value : /^[\w-]+(\.[\w-]+)+(:\d+)?(\/|$|\?|#)/.test(value) ? `https://${value}` : null;
  if (!candidate) return null;
  try {
    const url = new URL(candidate);
    if (url.protocol !== "https:" || !url.hostname || url.username || url.password || url.href.length > 8192) return null;
    return url.href;
  } catch {
    return null;
  }
}

/** What a command can reach: the composer's own actions and the microphone's (when it registered them). */
export interface SlashContext {
  voice: ComposerVoiceControls | null;
  /** Why voice is off here, said when /voice or /dictate cannot start. */
  voiceUnavailable: string;
  openGifPicker: (query: string) => void;
  openFilePicker: () => void;
  /** Uploads are off here: why (the command says it instead of opening the picker). */
  uploadsOff: string | null;
  addLink: (url: string) => void;
  openLinkBar: (prefill: string) => void;
  /** A short line under the field (role=status). */
  say: (words: string) => void;
}

/** Runs a command. The field's text is cleared by the caller before this runs. */
export function runSlash(option: SlashOption, context: SlashContext): void {
  const { command, argument } = option;
  switch (command.name) {
    case "gif":
      context.openGifPicker(argument);
      return;
    case "voice":
      if (context.voice) context.voice.record();
      else context.say(context.voiceUnavailable);
      return;
    case "dictate":
      if (context.voice) context.voice.dictate();
      else context.say(context.voiceUnavailable);
      return;
    case "upload":
      if (context.uploadsOff) context.say(context.uploadsOff);
      else context.openFilePicker();
      return;
    case "link": {
      const url = argument ? normalizeLink(argument) : null;
      if (url) context.addLink(url);
      else context.openLinkBar(argument);
      return;
    }
  }
}
