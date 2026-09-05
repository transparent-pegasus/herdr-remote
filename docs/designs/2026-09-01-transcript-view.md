# Transcript View — Design

Status: approved in chat 2026-09-01, section by section.
Not committed (coordination artifact).
Source: the user's two complaints about pane history — soft wraps follow the pane's
width, and a 300-line window returns far fewer lines — plus the investigation that
followed, all measured on this host on 2026-09-01.

## Goal

Replace terminal scraping as the *history* surface for agent panes. Read the agent's
own transcript file, show the conversation only, and keep the live terminal screen as a
separate, smaller surface for the things that exist only on screen.

## Non-goals

- No tool calls, tool output, thinking, or system messages in the history. Not even a
  "12 tools ran" separator: the user asked for the two speakers and nothing else.
- No status text authored by us. State is an icon or an animation, never a sentence.
- No transcript for plain shell panes. They keep the existing raw-output view.
- No new JavaScript dependency. Markdown renders on the server; the modal, the clamp,
  and the animation are native HTML/CSS.
- No streaming, no WebSocket. The existing polling stays.

## What was measured

Every number below came from this host on 2026-09-01 and is the reason a decision went
the way it did.

**Wrapping.** `pane.read source=recent` returns physical rows, already broken at the
pane's width: 400 characters in a 44-column pane came back as `44,44,44,44,44,44,44,44,44,4`.
`source=recent_unwrapped` returned the same content as one 400-character line. The
current server never asks for `recent_unwrapped`.

**The missing lines.** Claude Code runs on the alternate screen, and rows that leave it
never enter herdr's host scrollback, so `lines: 300` cannot return more than the
viewport:

| pane | agent | viewport_rows | max_offset_from_bottom | rows returned | truncated |
| --- | --- | --- | --- | --- | --- |
| w4:pW | claude | 56 | 0 | 57 | false |
| w2:p5Y | claude | 27 | 0 | 28 | false |
| w2:p2 | codex | 13 | 610 | 301 | true |

herdr's own skill documentation says the same thing. A codex pane returns 300; a claude
pane cannot. The transcript file has what the screen lost.

**Transcript size.** The conversation is a rounding error inside the file. This session's
transcript at the time of measurement: 402 KB total, of which text 2,849 B (0.7 %),
tool_use 15,937 B, tool_result 81,485 B, thinking 34,869 B (signatures only, no
readable content), metadata 94,529 B. The largest transcript on this host is 44 MB with
1.3 MB of conversation across 806 messages — the reason the cache is incremental.

**Live screen size.** A zoomed pane's whole visible screen is small: 1,393 B / 26 rows
with a `/model` picker open, 738 B / 15 rows idle. Sending the whole screen costs about
a kilobyte, which removed the only argument for cropping it.

**`blocked` does not fire for pickers.** Opening codex's `/model` picker left
`agent_status` at `idle` before, during, and after. Any design that gates on `blocked`
would never show a picker.

**Zoom works on an unfocused pane.** `pane.zoom` re-rendered a 20-column pane to full
width in about 1.2 s, and unzoom restored it. At 20 columns the picker was destroyed
(`› 1. gpt-5.… Latest / frontier / agentic`); zoomed, it was legible. Zoom removes the
split, not the window: the pane inherits whatever width the herdr window itself has,
which the operator may keep narrow. Nothing in the server or the page derives a column
count from that — the screen is sent as text and scrolls in its own box.

## Layers

Three surfaces, chosen by pane type. No user-facing toggle.

| Layer | Source | Applies to |
| --- | --- | --- |
| History | transcript file (4 formats) | supported agent panes; empty while the source is unavailable |
| Live | `pane.read source=visible` | every agent pane |
| Raw output (existing) | `pane.read` | shell panes and unsupported agents |

The raw-output view stays and its `Source::Scrollback` changes from `recent` to
`recent_unwrapped`, which is the root-cause fix for the wrapping complaint on the
surface where raw output remains the product.

## Routes

```
GET  /panes/{id}/transcript?before=<seq>&limit=30   history; ETag, 304 when unchanged
GET  /panes/{id}/live                               whole visible screen + composer line
GET  /panes/{id}/output                             existing; raw output fallback
POST /panes/{id}/open                               zoom this pane in
POST /panes/{id}/close                              zoom out
```

History and live are separate because their change rates differ by orders of magnitude.
History changes only when the transcript grows, so its ETag makes most polls a
body-less 304 even for a 44 MB transcript. Live changes on every keystroke.

`/transcript` without `before` returns the newest `limit` messages, oldest first, plus
`has_more`. With `before=<seq>` it returns the `limit` messages whose `seq` is strictly
less than that value, which is what the "earlier" button sends. Shells and unsupported
agents answer 404, selecting the raw-output view. Supported agents whose source is not
available return 200 with empty messages and `has_more: false`. An agent that has not
spoken still uses the history view.

`/live` answers `{ screen, composer }`: the whole visible screen as text, and the
extracted first line of the composer, empty when extraction found nothing.

`open` and `close` apply to every pane, including shell panes — the raw-output view
benefits from the same width.

## Transcript resolution

`PaneInfo` gains the `agent_session { source, agent, kind, value }` object that
`session.snapshot` already returns and the current code discards.

| agent | resolution |
| --- | --- |
| claude | `~/.claude/projects/<cwd slug>/<uuid>.jsonl`; the slug replaces `/` and `.` with `-` (`/mnt/ssd1/repos/herdr-remote` → `-mnt-ssd1-repos-herdr-remote`). If absent, search `projects/*/` for `<uuid>.jsonl` |
| codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*-<uuid>.jsonl`, newest matching rollout first. A nonempty reported id or guarded reported path is required; no cwd fallback |
| grok | `~/.grok/sessions/<percent-encoded cwd>/<uuid>/chat_history.jsonl`; without an id, the newest session directory under that cwd |
| cursor | herdr's id does not identify the store. Scan `~/.cursor/chats/*/*/meta.json` for `cwd` match and `hasConversation: true`, keep only entries whose `updatedAtMs` is at or after the reported id's own `createdAtMs`, prefer the entry whose `title` equals the pane's `terminal_title`, else the newest `updatedAtMs` |

`kind: "path"` uses `value` directly.

**`agent_session` is often absent.** Measured on this host with all four agents running in
one tab: the grok pane reported no session at all, and both codex panes reported none
until they had taken a turn. Codex therefore shows empty history until its own identity
is available: another session may already have a rollout in the same cwd. The other
agents retain the discovery rules in the table above.

Cursor's mismatch is measured, not hypothetical: pane `w2:p5` reported session
`c4d9d2bd…`, whose directory holds only `prompt_history.json` with
`hasConversation: false`, while the actual store lived under `d2c00d2d…` with the title
`Review Steering Digest` — the pane's own terminal title. 431 of 576 cursor sessions on
this host carry a `store.db` at all.

The `createdAtMs` floor exists because of a second measurement: pane `w4:p11` reported a
session with no conversation, and the only conversation-bearing cursor chat sharing its
cwd was `Slow Count` from the previous day — an unrelated session that a plain cwd match
would have displayed as this pane's history. The floor rejects it, the pane falls back to
raw output, and the earlier `w2:p5` case still resolves because its real store was both
created and updated after the reported session began.

## Path trust boundary

`agent_session` is self-reported: any process that can reach the herdr socket can call
`pane.report_agent_session` with an arbitrary path. The server therefore canonicalizes
the resolved path and requires it to sit under one of four roots —
`~/.claude/projects`, `~/.codex/sessions`, `~/.cursor/chats`, `~/.grok/sessions` — and
to have the expected file shape (`.jsonl`, or `store.db`). Anything else resolves to
no source, leaving a supported agent's history empty. Canonicalization also closes
symlink escapes. Cloudflare Access guards the tunnel; this is the guard inside the app.

## Normalization

```rust
struct Message {
    seq: u64,        // order within the file; the `before=` cursor
    role: Role,      // User | Assistant
    preview: String, // markdown stripped, plain text, capped at 300 chars
    html: String,    // pulldown-cmark, both roles; a user's newlines hardened
    output: Option<String>, // a `/` or `!` command's own answer
}
```

Everything else is dropped at normalization time: tool calls, tool results, thinking,
reasoning, system prompts, attachments, developer messages. Grok's
`reasoning.summary[].text` is readable plaintext, unlike the other three, and is
dropped anyway.

Per-format shapes, all confirmed by reading real files:

- **claude** — `{"type":"assistant","message":{"content":[{"type":"text"|"tool_use"|"thinking"}]}}`;
  user content is either a string or a parts array.
- **codex** — `{"type":"response_item","payload":{"type":"message","role":"user"|"assistant"|"developer"}}`,
  interleaved with `event_msg` noise (95 of 237 lines in the sample were
  `item_completed` duplicates).
- **cursor** — SQLite. `meta.value` is hex-encoded JSON holding `latestRootBlobId`; that
  blob is protobuf, a repeated 32-byte id list (`0x0a 0x20` + id), in conversation
  order; each referenced blob is a JSON message in Vercel AI SDK shape with
  `role: system|user|assistant|tool`.
- **grok** — flat JSONL: `{"type":"user"|"assistant"|"reasoning"|"tool_result"|"system"}`.

**Preamble stripping.** All four formats prepend environment blocks to the first user
message. Take the contents of `<user_query>` when present; drop `<system-reminder>`,
`<user_info>`, and `<timestamp>` blocks; drop codex's `role: developer` messages
entirely (they carry skills instructions and the full AGENTS.md). Without this, every
user card previews as `<user_info>OS Version: linux`.

**Commands.** A turn that is nothing but its own tags is a command, not prose: a slash
command previews as the name and arguments the person typed, and a `<bash-input>` turn as
`! <command>` — the line the harness itself shows back. Which tag opens a slash command is
not fixed — a plugin's command writes `<command-message>` first — so the turn is
recognized by either opening and the name and arguments are read wherever they sit. The
answer
is filed as the next turn (`<local-command-stdout>`, or `<bash-stdout>` and
`<bash-stderr>` together) and belongs to the card above it rather than being a speaker of
its own. Claude Code escapes `&`, `<` and `>` on the way into the two bash streams and
nothing else, so exactly those three are decoded back — `&amp;` last, or an author's own
`&lt;` would decode twice. Both streams in one turn are joined by a `---` rule, which is
what stops the second reading as more of the first. Quoting a tag in prose is not running
it: only a turn that is the tag and nothing else is read as one.

## Markdown rendering

`pulldown-cmark` 0.13.4 on the server, which pulls 8 crates. Comrak was rejected at 72
crates, and because it deletes raw HTML (`<!-- raw HTML omitted -->`) where
pulldown-cmark can escape it — agent prose contains `<Foo>` and `<system-reminder>`, and
losing those characters is worse than showing them.

Raw HTML events are mapped to text so they escape rather than execute, and link
destinations are limited to relative URLs and the `http`, `https`, `mailto` schemes.
Verified output for a hostile input: `<script>` and `onerror` appear escaped as text,
`[link](javascript:…)` renders as `href=""`, while GFM tables, fenced code with a
language class, task lists, and autolinks all survive. No sanitizer dependency, and no
markdown parser shipped to the phone.

## Cache

Per pane: `{ path, offset, len, Vec<Message> }` held in the process.

- Poll compares file length. Unchanged → 304, no body, no parse.
- Grown → read from `offset`, parse only the appended bytes, append to the vector.
- Shrunk → reparse from the start (truncation or rotation).
- Cursor has no byte offset: `meta.latestRootBlobId` is its revision;
  a new root walks the message references again. Its protobuf root can also carry
  metadata fields, which are skipped by wire type with bounds checks. Field 1 alone
  carries the ordered 32-byte message references.

The ETag includes the source identity, revision, and requested window. The opaque
`x-transcript-id` header identifies the resolved path and format without exposing the
path. A different source clears old cards, queued copies, expanded messages, and pending
pagination results in the client. Delayed send acknowledgements affect only the pane and
conversation where the send began. Input sent while awaiting a source is retained for
the incoming session. A supported-agent empty response has neither header; the client
clears its previous validator. Resolution misses and closes invalidate older request
tickets, including requests still resolving or refreshing; subsequent requests can retry
a miss. Cache hits advance the latest confirmed ticket so an older miss cannot erase a
newer confirmed session.

A 44 MB transcript is walked once, when the pane is first opened.

## Zoom

The server holds one `Option<String>`: the pane it currently has zoomed. `open` unzooms
the previous pane before zooming the new one, so a phone that dies without sending
`close` is repaired by the next `open` rather than by a timer. The front end also sends
`close` on `pagehide`.

The user accepted the desktop-side effect: they do not watch the PC while using this.

## Front end

```
[folder] [back] pane name
┌──────────────────────────────┐
│ ol#transcript      3-line cards  │
└──────────────────────────────┘
┌──────────────────────────────┐
│ ❯ draft line…            [⛶]    │  live band
└──────────────────────────────┘
┌──────────────────────────────┐
│ composer + [esc][↑][↓][enter]   │  existing send controls
└──────────────────────────────┘
```

**Cards.** Two lanes: the user's own messages right-aligned on an accent ground in a
proportional face, the agent's left-aligned and full width. The preview is plain text in
one node, `pre-wrap` so a writer's own line breaks and a command's printed lines survive
the collapse, clamped by CSS:

```css
display: -webkit-box; -webkit-box-orient: vertical;
-webkit-line-clamp: 3; line-clamp: 3; overflow: hidden;
```

Verified rendering: three lines then `…`, and short messages untouched. Tapping a card
opens a native nonmodal `<dialog>` via `show()`, carrying the same rendered HTML the card
previews. Escape and the close button dismiss it while the composer remains
usable.

**Workspaces.** The Lucide folder button sits immediately left of Back and is hidden on
the root workspace selection page. Its named `showModal()` dialog reuses workspace rows;
choosing one navigates to that workspace's tabs. Close, Escape, or tapping the backdrop
dismisses it. Session updates preserve the focused workspace, or move focus to Close if
it disappears. Navigation and browser history close the picker and pane sheets.

**Input.** Shell panes set `autocapitalize="none"`; agent panes set `sentences` before
the composer receives focus, including when an agent attaches to an existing pane.

**Appending.** Reuse the existing `paint()` discipline: identical bytes touch no DOM.
Append only new messages, keep the current follow-the-tail rule (follow when the reader
is at the bottom, hold position when they have scrolled up). "Earlier" reuses the
existing `#more` button, prepends, and preserves scroll position.

**Where the page's inset lives.** Each child of `<body>` carries the 1rem inset itself,
rather than the body carrying it for all of them. On the scrolling children — the pane
list, the card list, the raw log — that is what puts the scrollbar on the page's own edge
instead of 1rem inside it, with the text still inset. `max-width` moved from 34rem to
36rem so the reading column is the width it always was.

**Loading earlier output.** A reload icon at the top of the scrolling list, the header's
own. Where a pointer can drag — `(pointer: coarse)` — it is the drag: `height: var(--pull)`
gives it only what the gesture gives it, so nothing stands between the top of the list and
its oldest card until the reader pulls, and the icon turns as it comes. Past 56px, letting
go loads, and the icon is held at that height until the page comes back so a slow tunnel
does not look like a gesture that did nothing. `overscroll-behavior-y: contain` on the
page keeps the browser's own pull-to-refresh out of the way. A pointer that cannot perform
the drag keeps the icon visible instead — hiding it there would leave no way to reach it —
with `padding-top: 0.5rem` above and the list's `gap` supplying as much below. The button
is in the page either way, so a screen reader finds and presses it in both.

**A message sent mid-turn.** An agent queues what arrives while it is answering, and does
not write that turn to its own file until it takes it — minutes, on a long answer. The
page therefore keeps its own copy of anything sent from here and shows it below every real
card, exactly as a delivered one, which is where it will land: after the answer still
being written. Sending scrolls the list to it.

It settles when a user turn newer than the transcript it was sent against appears. Agents
take queued messages in order, so the oldest unsettled copy is the one that arrived — but
not always one at a time: the same agent will hand a queue over one turn per message on
one occasion and fold the whole of it into a single turn on another, and both were seen.
The arriving turn's own text is what says which, compared on letters and digits alone
because the card carries the markdown parser's rendering — `* one\n* two` comes back as
`one two`. When the text accounts for nothing (a link, an image, or a message past the
preview's 300-character cap) the turn settles exactly one, which is the behaviour that
holds without any text at all.

`settle` runs on every poll that changed anything, so it has to answer the same way twice.
It records a settlement by moving the next queued message's `after` past the turn just
claimed, rather than by consuming that turn: the first version consumed, and a second poll
against the same window ate the next queued message. It also treats a transcript whose
newest message is older than the one a message was sent against as having restarted its
sequence — a cleared context, a new session file — rather than leaving that message
waiting on a number the file will never reach.

**Live band.** One row, always present for agent panes:

- left: the extracted composer line, first line only, single-line ellipsis using the
  `overflow/text-overflow/white-space` pattern already used by `.name` and `.subtitle`.
  Empty and placeholder values render nothing.
- right: an `eye` button in every state, opening the whole visible screen as a
  monospace `<pre>` scrolled to its end. This is what shows `/model` pickers, permission
  prompts, and anything else that exists only on screen — no `blocked` gate, because
  `blocked` does not fire for pickers.
- when `working`: the animated `gavel` occupies the state slot beside the button. No
  words.

Composer extraction, with all four boxes measured on this host:

| agent | box | empty looks like |
| --- | --- | --- |
| claude | `❯ draft` inside a pair of `───` rules | `❯` alone |
| grok | `❯ draft`, no rules at all | `❯` alone |
| codex | `› draft` | `› Ask Codex to do an…` |
| cursor | `→ draft` between `▄▄▄` and `▀▀▀` | `→ Plan, search, build anything` or `→ Add a follow-up` |

Claude and grok collapse into one rule — take the **last** row whose first glyph is `❯` —
which is also what handles the trap: the same glyph prefixes *submitted* messages echoed
further up the screen, and the composer is always the last one. Rules are never searched
for, so grok's rule-less box needs no special case. Cursor keeps its block-border lookup
because its glyph is `→`, which appears in ordinary output. Only the draft's first line is
taken; every box wraps a long draft onto further rows and those are dropped.

Placeholders read as empty, and cursor has two of them — a fresh session says
`Plan, search, build anything`, one that has answered says `Add a follow-up`. When
extraction fails, render nothing: a wrong string labelled as the user's draft is worse
than a blank.

**Animation.** Inline lucide `gavel` (5 paths) with `transform-origin: 17% 89%`, the
handle tip, and a 1.1 s `strike` keyframe: hold lifted at −32°, strike to +6°, recoil to
−6°, settle. Verified by pausing the animation and screenshotting at t=0, 690 ms, and
780 ms. `prefers-reduced-motion` stops it and leaves the static icon; the `aria-label`
carries the state for assistive tech, and nothing visual says it in words. The
`eye` icon is inlined the same way. No icon library is installed —
`lucide-animated` was rejected because it requires React and Motion.

## Colour

Only one lane is filled: `#606b69`, the operator's own choice, 5.52:1 against white text
and past AA. Two derivations from the favicon's hue were tried first — 66,249 chromatic
pixels of the mark average 189.3 degrees, giving `#004340` at the darkness the mark uses —
and were set aside for it.

The agent's lane is the page itself, separated from it by the same `1px solid var(--edge)`
the pane-picker rows use. Two filled lanes were tried first — a matched-lightness
`#393939`, then the first draft's `#1c1c1c` — and both were rejected: the side being read
does not need to announce itself, only the side being answered does. Both sheets keep the
page's own Canvas for the same reason. The rendered page was sampled to confirm the
browser paints exactly those values.

## The two sheets, and who may act while one is open

A message and the pane's screen open into the same shape: fixed, edge to edge between the
header and the composer, `--full-gap` of air above and below, and the same
`1px solid var(--edge)` rule on
the top and bottom that the composer already draws against the page. One continuous
surface: the lucide `x` floats over its foot at `position: absolute`, and the `3rem` of
bottom padding on the text is what keeps the two apart — inside the scroller, so the end
of a long message clears it, and inside the box `fitScreen` measures, so a screen is never
scaled under it. A veil of `rgb(0 0 0 / 0.55)` covers everything the sheets cover and
stops at the two controls that stay live under them — the header above and the composer
below — which no `::backdrop` could do: `showModal()` paints its backdrop over the whole
viewport, controls included. Both edges are measured into `--header-h` and `--composer-h`
rather than assumed: the composer grows with its draft and the header with whatever the
workspace is called.

Both sheets park focus on their text, which carries `tabindex="-1"`. The dialog focusing
steps run for `show()` as well as `showModal()`, and would otherwise ring the close
control on open.

They differ only in who may act:

| sheet | opened with | the controls below | content |
| --- | --- | --- | --- |
| a message | `show()` | live | scrolls |
| the screen | `show()` | live | scaled to fit, never scrolls |

The screen's modality follows from what it is for: a `/model` picker on it is answered with
the arrow keys and Enter in the composer. Reading a message also keeps those controls
available. The folder shortcut sits above both sheets. Its workspace picker is modal;
Escape closes that top dialog without also dismissing the sheet beneath it.

The screen is scaled rather than wrapped because it is a fixed grid of characters whose
boxes wrapping would break: `transform: scale()` on a `width: max-content` `<pre>`, the
factor measured from the view's own content box against the pre's natural size,
recomputed when the text changes and on resize. Measured at a 500 px viewport, a
110-column screen came out at 0.599 and fitted whole. The content box is computed rather
than taken from `clientWidth`, which carries the padding and would clip that much off the
right and the bottom.

## Who says what the pane is

Nothing on the page says a pane's state in words. The heading carries `agent · label` for
an agent pane and the label alone for a shell; the composer's label, which used to repeat
`agent · state` above the box, now says nothing at all once a pane is picked and keeps
only its "Pick a pane first." for the case where none is. The state itself is left to the
indicators — the dot on each list row, the hammer while a turn runs, the stop button's own
hand-or-eraser icon.

## Degradation

- A resolution miss or refused path leaves supported-agent history empty. Shells and
  unsupported agents use raw output. Codex never guesses a session from its cwd.
- Transcript file disappears mid-session → drop the cache entry and show empty history.
- herdr unreachable → the existing error path is unchanged.
- Composer extraction fails → the band's left half is blank; the button still opens the
  full screen.

## Testing

Rust unit tests, fixtures written by hand — real transcripts carry the user's work and
must not enter the repository:

- one parser test per format, including cursor's blob-DAG walk
- preamble stripping: `<user_query>` extraction, reminder/user_info/timestamp removal,
  codex developer messages dropped
- path boundary: a path outside the roots is refused; a symlink escape is refused; a
  refusal yields "no transcript" rather than an error
- cache: append parses only the tail, shrink reparses, equal length yields 304
- markdown: `<script>`, `onerror`, and `javascript:` are neutralized; tables, fenced
  code, and task lists survive
- composer extraction: empty, placeholder, and drafted, for each of the four agents,
  plus the echoed-`❯` trap and cursor's two placeholder strings
- zoom bookkeeping: opening a second pane unzooms the first

Vitest: card role routing, modal open/close, append merge with and without tail
following, `before` pagination prepending. The CSS clamp is not unit tested.

Verification: `make check`, plus `make run` as a smoke check because both `src/` and
`web/` change. Then one real pass on a live pane: open → 30 messages → modal → earlier →
maximize on a `/model` picker → close and confirm the pane unzoomed.

## Known risks

- Cursor's matching can still pick the wrong session when two chats share a cwd, a title,
  and a window of time. The `createdAtMs` floor removes the common case (a stale chat from
  an earlier day) but not this one. It degrades to a wrong-but-real transcript rather than
  to an error, which is the least defensible failure in this design.
- Codex history remains empty until herdr reports its session id or transcript path.
- TUI redraws change the composer box shape between agent versions. The blank-on-failure
  rule bounds the damage.
- The path boundary validates a pathname and then opens it, so a local process able to
  write inside `$HOME` between those two syscalls could swap the file. Closing that window
  needs `openat2` with `RESOLVE_BENEATH` on every open and reopen; for a single-operator
  server behind Cloudflare Access that costs more than the window is worth, and it is
  recorded here rather than implemented. What the boundary does stop is the reported path
  itself: a canonicalized file outside the agent's own root, a symlink out of it, or a file
  of the wrong shape. It also stops SQLite's URI decoding — the store is opened as a plain
  read-only path, never as `file:…?mode=ro`, because a directory literally named `%2e%2e`
  under an allowed root survives canonicalization and is then decoded as `..`. That escape
  was reproduced on this host before it was closed.
- A queued message is shown from our own copy, so an agent that drops it without ever
  taking a turn leaves its card standing until the pane is left. Nothing on the card
  claims it was delivered; it is simply the last thing the reader sent.
- A transcript file replaced atomically by one of exactly the same length is not detected
  by the cache, which compares length. This same-path limitation is distinct from
  switching to a different session file, which changes the source identity and clears
  the previous conversation. Inode identity is not tracked.
- The zoomed pane hides its siblings on the desktop for as long as the phone holds it
  open. Accepted explicitly.
- Zoom is not a legibility guarantee. It gives the pane the herdr window's full width, and
  that window is not always full-screen, so a picker can still be truncated by the terminal
  itself. The remedy is the operator's window, not a number in this code — which is why no
  constant anywhere encodes an expected zoomed width.
