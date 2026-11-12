/** A member mention being typed at the caret. Offsets are textarea UTF-16 offsets. */
export interface MentionQuery { start: number; end: number; query: string }

/** Avoid email addresses and ordinary @ inside a word; replace the whole token when editing its middle. */
export function mentionAt(text: string, start: number, end: number): MentionQuery | null {
  if (start !== end) return null;
  const before = text.slice(0, start);
  const match = /(?:^|[\s([{])@([\p{L}\p{N}_:.-]*)$/u.exec(before);
  if (!match) return null;
  const query = match[1]!;
  const tokenStart = start - query.length - 1;
  const suffix = /^[\p{L}\p{N}_:.-]*/u.exec(text.slice(start))?.[0] ?? "";
  return { start: tokenStart, end: start + suffix.length, query };
}

/** Insert an unambiguous account ID; retain punctuation and the rest of the message. */
export function insertMention(text: string, mention: MentionQuery, id: string): { text: string; caret: number } {
  const after = text.slice(mention.end);
  const space = !after || !/^[\s.,!?;:)\]}]/u.test(after) ? " " : "";
  const inserted = `@${id}${space}`;
  return { text: text.slice(0, mention.start) + inserted + after, caret: mention.start + inserted.length };
}
