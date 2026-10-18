/**
 * The CLI commands the details panel shows for a conversation, laid out for the column they are shown in (the panel,
 * the drawer or the bottom sheet measure it: address-section.tsx): a command that does not fit continues on the next
 * line after a backslash, indented, so the block never needs a sideways scroll and still pastes into a shell exactly
 * as shown. (A single word longer than the column, such as a conversation's UUID in a narrow column, starts its own
 * line and wraps in the block; details.module.css.)
 *
 * A custodian's "Viewing as" view is read only, and so are its commands: reading the conversation works for the
 * custodian with their own sign-in (DM lets a custodian read their Silicon's conversations), sending as the Silicon
 * does not.
 *
 * Pure (tests/details-cli.test.ts).
 */
import { shellQuote } from "@/lib/chat/address";

/** The column the panel had before anything was measured (340 px at 12.5 px monospace). */
export const DEFAULT_COLUMNS = 34;

/** Joins shell words into lines of at most `width` characters, continuing with ` \` and a two-space indent. */
export function layoutCommand(words: string[], width = DEFAULT_COLUMNS): string {
  const lines: string[] = [];
  let line = "";
  words.forEach((word, index) => {
    // A line that another one follows ends with " \", so it needs room for it.
    const tail = index < words.length - 1 ? 2 : 0;
    if (!line) {
      line = word;
      return;
    }
    if (line.length + 1 + word.length + tail <= width) line = `${line} ${word}`;
    else {
      lines.push(`${line} \\`);
      // Indented when that still fits; a word too long for it starts at the edge (the shell ignores the indent).
      line = 2 + word.length + tail <= width ? `  ${word}` : word;
    }
  });
  if (line) lines.push(line);
  return lines.join("\n");
}

/**
 * The commands for a conversation: read it, write to it, and (a group) show its members. In a custodian's view (`view`,
 * the Silicon's id): read it, show the group, and list the Silicon's conversations.
 */
export function conversationCommands(address: string, options: { group: boolean; view?: string | null; width?: number }): string {
  const target = shellQuote(address);
  const commands = options.view
    ? [
        ["dm messages list", target],
        ...(options.group ? [["dm groups show", target]] : []),
        ["dm conversations list", `--for ${shellQuote(options.view)}`],
      ]
    : [
        ["dm messages list", target],
        // An option and its value stay together on one line, and the address comes last, as in every command here:
        // the one long word then never has to carry a " \" after it (a UUID fills a narrow line on its own).
        ["dm messages send", "--text 'Hello'", target],
        ...(options.group ? [["dm groups show", target]] : []),
      ];
  return commands.map(words => layoutCommand(words, options.width)).join("\n");
}
