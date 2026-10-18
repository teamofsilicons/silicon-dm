/**
 * Giving the keyboard back after a question asked in place closes ("Remove Bo?", Silicon UI's ConfirmMorph): to the
 * control that asked it, but only when focus has nowhere better to be. Cancel or Esc takes the question's buttons away
 * (focus falls to the page), so focus goes back to Remove; a click into another field also closes the question, and
 * then focus stays in that field. Taking it back there made the next spaces typed ask again and confirm: an account
 * came off a Silicon's allow-list while the Carbon was typing a report.
 *
 * It is checked in the next frame, after the browser has moved focus for the press that closed the question (the
 * question closes on pointerdown, before the field takes focus).
 */
export function refocusIfLost(within: HTMLElement | null, target: () => HTMLElement | null): () => void {
  const frame = requestAnimationFrame(() => {
    const active = document.activeElement;
    const lost = !active || active === document.body || active === document.documentElement || !active.isConnected;
    if (lost || (within?.isConnected && within.contains(active))) target()?.focus();
  });
  return () => cancelAnimationFrame(frame);
}
