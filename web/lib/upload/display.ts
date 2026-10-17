/**
 * The address the browser loads an attachment or a voice note from. Silicon Briefcase answers public bytes with
 * `Cross-Origin-Resource-Policy: same-origin` and a sandbox CSP, so an <img>, <audio> or <video> on this site cannot
 * load them from Briefcase directly: they come through this website's own origin (a proxy route that forwards Range).
 * Any other https link is shown as it is.
 *
 * The composer area owns this module and the proxy route (app/api/uploads/**); the thread and the voice area call
 * `displayUrl` for every attachment and voice note they render. Until the proxy exists it returns the link unchanged.
 */
export function displayUrl(url: string): string {
  return url;
}
