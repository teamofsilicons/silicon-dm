/**
 * What Settings → Report a problem sends and checks before sending (pure: tests/details-settings.test.ts).
 *
 * The DM service keeps a report only with a pull request in DM's own repository (src/reporting.rs: https, github.com,
 * /teamofsilicons/silicon-dm/pull/<number>, nothing after it). Checking that here lets the form say what a good link
 * looks like next to the field, instead of a refusal after the send.
 */

/** The website's version, as reports quote it (`web-0.17.1`); the test keeps it equal to package.json. */
export const WEB_VERSION = "0.17.1";

/** DM's repository, where fixes go as pull requests. */
export const PULL_REQUESTS = "https://github.com/teamofsilicons/silicon-dm/pull/";

/** Why a pull request link would be refused, or null when it is fine (or empty: it is optional). */
export function pullRequestProblem(text: string): string | null {
  const value = text.trim();
  if (!value) return null;
  const example = `${PULL_REQUESTS}123`;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return `That is not a link. A pull request link looks like ${example}.`;
  }
  const number = url.pathname.startsWith("/teamofsilicons/silicon-dm/pull/") ? url.pathname.slice("/teamofsilicons/silicon-dm/pull/".length) : null;
  // A "?" or "#" with nothing after it still counts for the service, so the text is checked, not just the parsed URL.
  const ok = url.protocol === "https:" && url.hostname === "github.com" && !url.username && !url.password && !/[?#]/.test(value) && !!number && /^\d+$/.test(number);
  if (ok) return null;
  return `DM takes fixes as pull requests in teamofsilicons/silicon-dm, so the link looks like ${example} (no /files or #part after the number).`;
}
