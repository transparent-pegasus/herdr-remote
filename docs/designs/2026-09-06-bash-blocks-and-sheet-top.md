# `!` commands, a person's own line breaks, and where a sheet starts

**Date:** 2026-09-06
**Branch:** `feature/bash-blocks-and-sheet-top`

Three small corrections to the transcript view, all of them cases where the phone
showed something its writer never typed.

## 1. A `!` command was leaking its tags

Claude Code files a `!` command as two turns:

```
{"type":"user","message":{"content":"<bash-input>gpso main</bash-input>"}}
{"type":"user","message":{"content":"<bash-stdout>…</bash-stdout><bash-stderr></bash-stderr>"}}
```

Neither was handled. `preamble::strip` had no rule for either, so both reached the card
with their tags around them, and the second appeared as a second speaker.

`<bash-input>` now previews as `! <command>` — not an invention, it is the line Claude
Code's own history renderer produces for the same row. The two streams go through
`preamble::command_output`, which already exists for `<local-command-stdout>`, so they
land in `Message.output` and the phone paints them under the command with the `↪` it
already draws. No wire change and no web change for this half.

**Escaping.** The streams are written escaped and the input is not. Verified in the
shipped binary: `u.replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;")`,
applied to `stdout` and `stderr` only. Exactly those three are decoded back, `&amp;`
last — decoding it first would turn an author's own `&amp;lt;` into a `<` they never
wrote. Terminal colour is stripped by the same `without_ansi` the local-command path
uses.

**Two streams, one field.** `Message.output` is one string, and a command that wrote to
both would otherwise run its stderr straight on from its stdout. They are joined by a
`\n---\n` rule. An empty stream contributes nothing, so the common case — stdout only —
carries no rule at all.

`<bash-exit-code>` exists as a tag name in the binary but is never written to a
transcript, so nothing reads it.

## 2. A person's newlines were being folded into spaces

A user turn goes through the same markdown renderer the agent's does, and CommonMark
folds a lone newline into the paragraph. The card and the sheet both showed a typed line
break as a space — while a message still queued locally showed it correctly, because
`sentCard` renders escaped text at `pre-wrap`. The shape changed the moment the agent's
file caught up.

The parse stays; only the break changes. `markdown::to_html_hard_breaks` maps `SoftBreak`
to `HardBreak` and is used for user turns alone: an agent wrapping its prose means one
paragraph, a person pressing the key means two lines. `markdown::plain`, which is the
user's preview and nothing else, keeps newlines and drops blank lines, so the card's
three clamped lines are spent on words. `.preview` becomes `pre-wrap` to show them, and
`sentCard` follows the same rule so a card does not change shape when it settles.

The `↪` output row shares `.preview`, so a command's printed lines now break in the list
as well as in the sheet. That is the same correction, and it is what the two bash streams
need.

## 3. Both sheets covered the header

`#full` and `#screen` were `top: var(--full-gap)` — one gap from the top of the viewport,
over the header. They now start below it, and the veil stops there too: the header and
the composer are the two controls that stay live while a sheet is open, so neither is
dimmed.

The header's height is measured into `--header-h` in `sizeSheets`, beside the
`--composer-h` already taken there, rather than being assumed — it grows with whatever
the workspace is called. A `2.25rem` fallback covers the first paint, before any sheet
has opened.

Verified in Chrome at 390×844 against the built page: header 37px, sheet top 53px
(37 + `--full-gap`), sheet bottom 732px (844 − 96 composer − `--full-gap`), header fully
visible and undimmed above it, `one<br />two` on two lines.
