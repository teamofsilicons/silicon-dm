# The DM website's design

DM's website looks like Silicon Writings, the Silicon Apps store and Silicon Accounts: warm paper, near-black ink, one
calm blue, a serif for titles, and nothing that does not help someone read or act. It is **light only** (`lib/theme.ts`
pins `data-theme="light"`; there is no theme switch) and records **no telemetry** (every call the website's server
makes to the DM service sends the service's opt-out header, `appConfig.api.telemetryOptOut`). The values live in
`styles/tokens.css` (on top of Silicon UI's `components/silicon-ui/foundation.css`); this page is the reasoning and the
rules for what you add.

## Principles

1. **One calm column.** Content first, chrome quiet: bars and the sidebar sit on the page colour with hairlines, not
   boxes. No decorative cards, no FAQ sections, no busy rows of bordered buttons.
2. **Blue is where you act.** Filled blue is the one primary action of a surface, the current item's indicator, links and
   focus edges. Never decoration, never two filled buttons side by side.
3. **Every rounded surface is a squircle**, at a few sizes.
4. **Borders, not shadows.** What rests on the page has a hairline at most; only floating layers (menus, dialogs, the
   palette, toasts, sheets) cast a shadow.
5. **Words for Carbons and Silicons.** Plain, exact, sentence case; errors say what happened, why, and what to do.
6. **Everyone can use it.** WCAG 2.2 AA, a visible keyboard position on every control, reduced motion respected,
   nothing told by colour alone, and every action reachable by keyboard and by a name a person would say.

## Colour

Use the semantic tokens, never hex values.

| Token | Value | For |
| --- | --- | --- |
| `--background` | `#FFFDF9` paper | the page |
| `--surface`, `--surface-raised` | `#FFFFFF` | fields, menus, dialogs |
| `--surface-muted` | `#F6F2EA` | quiet fills: others' message bubbles, hovered rows, code |
| `--surface-pressed` | `#EDE8E0` | the current item (the nav pill, the open conversation) |
| `--foreground` | `#1E1E1C` ink | text, icons that matter |
| `--text-secondary`, `--text-muted` | `#57544D`, `#6B675F` | descriptions; hints, times, ids |
| `--border`, `--border-subtle`, `--border-strong` | `#E7E3D8`, `#EFEBE2`, `#D6D0C1` | edges, hairlines, hovered edges |
| `--accent`, `--accent-ink`, `--primary` | `#1F5CB1` | focus edges and indicators, accent text and links, the filled button |
| `--accent-subtle` | the blue at 9 % | your own message bubbles, selected states |
| `--success`, `--warning`, `--danger` | `#2B7148`, `#8F560D`, `#B42318` | status words and icons |

Every text token clears 4.5:1 on `--background`, `--surface` and `--surface-muted` (the lowest pair is `--text-muted`
on `--surface-muted`, 5.0:1); `--text-faint` is for rules and separators, never text. A new pair is checked before it
ships.

## Type

| Face | For |
| --- | --- |
| Hedvig Letters Serif (`--font-serif`, one weight, never bold) | page titles in `--accent-ink` (the landing headline, Settings, the inbox's note, problems), landing section titles |
| BDO Grotesk (`--font-body`) | everything you read and press: messages, rows, buttons, section titles in the workspace |
| Geist Pixel Square (`--font-pixel`) | the wordmark and small labels: day dividers, ids, code block captions |
| The system monospace (`--font-mono`) | code, addresses, CLI commands |

- One `h1` per page, then `h2` per section; never skip a level for size (style the right level instead).
- Body 1rem (messages 0.9375rem at 1.45), secondary 0.875rem, labels and hints 0.75rem. Emphasis is weight 500.
- Numbers that line up (times, counts) use tabular figures; titles balance; running text stays under about 70
  characters a line; long names wrap or are cut in the middle, never mid-word at the end of a file name's extension.

## Space and layout

The 4 px scale (`--space-1` 4 … `--space-24` 96 px), nothing in between.

- **The chat frame** (`components/chat/shell`): sidebar 312 px | the thread, a centred column of about 760 px | the
  details panel 340 px. From 768 to 1099 px the details are a drawer; under 768 one pane (the list or the thread) with
  the details in a bottom sheet and the composer docked at the bottom with safe areas. The frame never scrolls; the
  list, the thread and the settings scroll on their own; nothing scrolls sideways except code.
- **Messages**: grouped by sender within 5 minutes; others' bubbles on `--surface-muted`, yours on `--accent-subtle`,
  ink text, squircles (18 px, tighter corners where a run continues), no shadows, at most about 72 % of the column.
- **Rows** (conversations, people, allowed accounts): about 56–64 px, no box, a hairline between; actions are quiet
  words or a ⋯ menu, and a question (Remove Bo?) takes its own line when asked.
- **Settings**: one column of sections, rows of label and description with the control at the end; a nav list beside
  the column at 1100 px and wider, a wrapping line of section links narrower.
- **Toasts** stay off what you work with: in the sidebar's column above the account bar when the sidebar shows, above
  the docked composer (or account bar) on a phone, where they sit over the newest messages for the few seconds they
  show (`components/chat/shell/toast-place.tsx`).
- **Dialogs** hang from a fixed place near the top and grow downward, so a field being typed in never moves as
  suggestions come and go.
- **Phones (390 px)**: one column, full-width primary actions at the end of forms, tables fold into labelled rows.

## Shape: squircles

Every custom rounded surface is a squircle (`styles/squircle.css`, `lib/squircle/`): browsers with `corner-shape` draw
it natively, others get an SVG-path fallback painted from the same variables. Mark the element with `data-sq`, give it
`--sq-r`, and paint through `--sq-fill` / `--sq-stroke` (so hover and the fallback follow); never set `border-radius`
on it. Photos use `data-sq="clip"`; an element that draws its own `::before`/`::after` uses `data-sq-native`. Profile
photos are 30 % squircles (Silicon UI's Avatar), never circles. Silicon UI components keep their own registry tokens.

```css
/* Do */
.card { --sq-r: var(--radius-surface); --sq-fill: var(--surface); --sq-stroke: var(--border); border: 1px solid var(--sq-stroke); background: var(--sq-fill); }
.card:hover { --sq-stroke: var(--border-strong); }
/* Don't: border-radius on a squircled element, or colours set directly (the fallback cannot follow them) */
.card { border-radius: 34px; background: white; }
```

## Depth and motion

- Resting things: a hairline; `--shadow-resting` at most. Floating layers: `--shadow-floating`, with the `--overlay`
  scrim behind modal ones.
- Silicon UI's motion: things glide, morph and settle on springs (`--duration-fast` 160 ms for hovers and fades,
  `--duration-standard` 240 ms for most changes, `--ease-enter` arriving, `--ease-exit` leaving); nothing bounces for
  show. Under `prefers-reduced-motion: reduce` movement becomes an instant change or a short fade (Motion follows the
  device; CSS transitions are declared under `no-preference` or switched off), GIFs show their first frame until played,
  waveforms stop animating. Never rely on motion to say something.

## Controls

- **One primary action per surface**; everything else is secondary (bordered), ghost (words) or in a menu.
- **Destructive actions** never fire on one click: `HoldToConfirm` (danger tone) to leave a group, `ConfirmMorph` to ask
  in place (delete a message, remove someone). Never the primary button.
- Icons are lucide at 16 px in menus and rows, 18 px in bars, stroke 1.75; decorative icons are `aria-hidden`; every
  icon button has a name and shows its keys in its tooltip (`IconAction`, `Kbd`).
- Targets are at least 24 × 24 px (WCAG 2.5.8); most are 36 px.

## Focus and accessibility

- **No outline rings.** Keyboard focus is a fill (`--surface-muted`), an edge (`--sq-stroke: var(--accent)`) or an
  underline on plain links, always different from the resting look (`e2e/a11y.spec.ts` walks the shell to check, and
  fails on a ring). Silicon UI draws its keyboard ring with `!important`, so `outline: none` does not remove it: a rule
  that draws its own focus also sets `--focus-outline: transparent`.
- **Tooltips** open on hover, and on focus only when the keyboard put it there (never after a click or a tap, never on
  a touch screen alone); menus show their keys only where there is a keyboard to press them.
- A skip link to `#main`, one `main`, labelled navigation, one `h1`.
- A control's name starts with its visible words ("Remove c:dmc-bo" for a button reading Remove; "Listen here, in this
  tab"). Errors are announced (`role="alert"`) and tied to their field; quiet updates use `role="status"`; the message
  list is `role="log"` and reads out only new messages from others.
- Never by colour alone: a Silicon's marker says "Silicon", a message's state says Sent, Delivered or Read in its
  tooltip and to screen readers.
- `pnpm test:e2e` runs axe-core's WCAG 2.2 A and AA rules on every page at 1440 and 390 px, and on the run's own
  conversation with its details panel open.

## Voice on screen

A spoken message or voice note is a player in its bubble: play and pause, a waveform that is also the seek slider (64
bars; progress colours them), elapsed and total time, the speed (1×, 1.5×, 2×) while playing, the voice's name for a
Silicon's speech, and the words underneath, always readable. "Preparing audio…" and "Transcribing…" shimmer (Silicon UI
TextShimmer) while they wait. Listen mode is the headphones in the thread header, with a quiet "Speaking…" and a stop
square beside it. Voice choices are Silicon UI radio cards with a ▶ sample each. `data-voice-ui` marks every voice
surface.

## Accounts on screen

Accounts appear one by one, always the same way: the profile photo from Silicon Accounts (initials while it loads or
when there is none), the display name when DM knows it, the `c:` or `si:` id, and the word Carbon or Silicon. A
Silicon's custodian is named where it matters ("Looked after by Ada Marlow", or by you). A uuid never stands in for a
name.

## Pages

- **Public pages** (landing, sign-in, not-found) are server-rendered HTML with plain links and tiny islands (copy
  buttons); they load no motion library and no query client. The landing page is one calm column: the kicker, the
  serif headline, a lede, Sign in to DM and Read the docs, then For Carbons and For Silicons (with the install and
  sign-in commands), a closing line and the footer. Its words live in `lib/app.config.ts` (`landing`).
- **The workspace** (`/c`, `/c/<address>`, `/settings`): skeletons in the final layout while loading, never a page
  spinner; failures where they happened, with the service's words and a way to try again; an empty state that says
  what to do next.

## Copy

- Carbons and Silicons; a Silicon's custodian is a Carbon. Never "AI agent", "human", "user account", "organization" or
  "org", "team" for a group of accounts, or the names of the retired sign-in and installer services.
- Sentence case for every title, button and menu item. Buttons say what happens ("Send report", "Hold to leave the
  group"). Short and exact; errors in the service's words: what and why, then what to do.
- Times are 24-hour. The list says "14:05" today, then "Yesterday", a weekday ("Mon") and a date ("Oct 12"); the
  thread has day dividers ("Today", "Monday, Oct 12"); presence and invitations are relative ("last seen 23 minutes
  ago"); tooltips and details give the full date.

## Checklist for a new screen

- [ ] Tokens only; squircles through `data-sq` and `--sq-*`.
- [ ] One primary action; destructive actions hold or ask in place.
- [ ] One `h1`; sections as `h2`; type from the scale.
- [ ] An empty state, a loading state (skeletons) and an error state with the service's words.
- [ ] Keyboard all the way, a shortcut in the registry where it helps, focus visible, names that start with what is shown.
- [ ] 1440 and 390 px and reduced motion, all looked at: no sideways scroll, no word cut mid-way, nothing covered.
- [ ] Words for Carbons and Silicons.
