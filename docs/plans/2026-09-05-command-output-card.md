# Command Output Card Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use pane-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A slash command's own output joins the card that shows the command, the message sheet stops making the controls inert, and a user's words reach the phone as rendered markdown.

**Architecture:** Claude Code writes a local command's output as its own `user` line whose whole body is `<local-command-stdout>`. The parser stops treating that line as a speaker and hands it to the message before it, so one command is one card with an `output` field beside its text. The phone paints that field as a second bubble under the command's, marked with a corner arrow outside the bubble. The message sheet becomes `show()` like the screen sheet, and user text goes through the same markdown renderer the agent's text already uses.

**Tech Stack:** Rust (serde, pulldown-cmark), TypeScript, Astro, Vitest.

**Spec:** the design agreed in chat on 2026-09-05 (no separate design doc; bounded change to existing flows).

## Global Constraints

- `any` is prohibited in TypeScript.
- `aube` is the only package manager for `web/`; never `npm`/`pnpm`/`yarn`.
- Baseline verification is `make check`; `make run` is the smoke check when `src/` or `web/` changes.
- Keep the implementation to the bare minimum (`.claude/skills/artful-simplicity/SKILL.md`).

## Tracks

| Track | Goal | Tasks | Owned files | Depends on |
|---|---|---|---|---|
| `rust` | The server carries a command's output and renders both speakers as markdown | 1-2 | `src/**` | — |
| `web` | The phone frees its controls and paints the output bubble | 3-4 | `web/**` | — |

The two tracks share no file. Their only contact is the wire shape, fixed here: a card gains an optional `output` string of plain text, and `html` is markdown-rendered for both roles. `web` is written against that shape and needs no `rust` code to test.

**Post-integration follow-ups:** `make check` in the integrated worktree, `make run` smoke check, `CLAUDE.md` review (no contract in it changes, so expected to be a no-op).

---

### Task 1: A local command's output belongs to its command

**Track:** `rust`

**Files:**
- Modify: `src/transcript/preamble.rs`
- Modify: `src/transcript/claude.rs`
- Modify: `src/transcript/mod.rs` (`Message`, `refresh_lines`)

**Interfaces:**
- Produces: `preamble::command_output(&str) -> Option<String>`; `transcript::Parsed { Message(Message), Output(String) }`; `Message.output: Option<String>`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing tests**

In `src/transcript/preamble.rs`:

```rust
    #[test]
    fn a_local_command_writes_its_output_as_its_own_turn() {
        let text = "<local-command-stdout>Set model to \u{1b}[1mOpus 5\u{1b}[22m</local-command-stdout>";
        assert_eq!(command_output(text).unwrap(), "Set model to Opus 5");
    }

    #[test]
    fn prose_that_merely_mentions_the_tag_is_not_output() {
        assert!(command_output("see <local-command-stdout> in the file").is_none());
        assert!(command_output("plain words").is_none());
    }
```

In `src/transcript/claude.rs`:

```rust
    #[test]
    fn a_commands_output_is_the_tail_of_the_command_not_a_turn_of_its_own() {
        let line = r#"{"type":"user","message":{"content":
            "<local-command-stdout>Set model to Opus 5</local-command-stdout>"}}"#;
        let Some(Parsed::Output(output)) = parse_line(line, 3) else {
            panic!("not output")
        };
        assert_eq!(output, "Set model to Opus 5");
    }

    #[test]
    fn an_empty_output_is_nothing_at_all() {
        let line = r#"{"type":"user","message":{"content":
            "<local-command-stdout></local-command-stdout>"}}"#;
        assert!(parse_line(line, 1).is_none());
    }
```

In `src/transcript/mod.rs` `cache_tests`, a test that the two lines become one message (write the command line and the output line to a scratch file, refresh, and assert one message whose `text` is `/model` and whose `output` is the stdout body), and a second that writes the output line in a later refresh and asserts it still lands on the same message.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test transcript`
Expected: FAIL — `command_output` and `Parsed` do not exist.

- [ ] **Step 3: Unwrap the block in `preamble.rs`**

```rust
/// A local command's output is a turn of its own whose whole body is the tag,
/// so only a text that is nothing else is one. Terminal colour comes with it
/// and nothing on the phone renders it.
pub fn command_output(text: &str) -> Option<String> {
    let body = text
        .trim()
        .strip_prefix("<local-command-stdout>")?
        .strip_suffix("</local-command-stdout>")?;
    Some(without_ansi(body).trim().to_string())
}

/// CSI sequences only, which is all a command's own output carries.
fn without_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // A CSI ends at its final byte, which is the first in `@`..=`~`.
            for character in chars.by_ref() {
                if ('@'..='~').contains(&character) {
                    break;
                }
            }
        }
    }
    out
}
```

- [ ] **Step 4: Give a line two shapes in `mod.rs`**

```rust
/// What one transcript line is: a message of its own, or the tail of the one
/// before it. A slash command's output is written as its own line and belongs
/// to the command it answers.
pub enum Parsed {
    Message(Message),
    Output(String),
}
```

and on `Message`:

```rust
    /// A local command's own output, when this message is the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
```

Every `Message { .. }` literal gains `output: None` (`claude.rs:50`, `claude.rs:58`, `codex.rs:54`, `grok.rs:38`, `cursor.rs:116`, `mod.rs:49`, `main.rs:770`, `main.rs:1397`, `main.rs:1403`).

- [ ] **Step 5: Return the new shape from `claude.rs`**

`parse_line` returns `Option<Parsed>`; the two `Message` returns become `Parsed::Message(..)`, and before `preamble::strip`:

```rust
    // Not the user speaking: the command's own answer, which belongs to the
    // card that shows the command rather than to a card of its own.
    if role == Role::User {
        if let Some(output) = preamble::command_output(&joined) {
            return (!output.is_empty()).then_some(Parsed::Output(output));
        }
    }
```

- [ ] **Step 6: Attach it in `refresh_lines`**

```rust
            let parsed = match format {
                Format::Claude => claude::parse_line(&line, next_seq),
                Format::Codex => codex::parse_line(&line, next_seq).map(Parsed::Message),
                Format::Grok => grok::parse_line(&line, next_seq).map(Parsed::Message),
            };
            match parsed {
                Some(Parsed::Message(message)) => {
                    messages.push(message);
                    next_seq += 1;
                }
                // The command it answers can be in a batch already served, and
                // that batch is not in `messages`; carry it out of the loop so
                // a read failure still leaves the snapshot untouched.
                Some(Parsed::Output(output)) => match messages.last_mut() {
                    Some(last) => last.output = Some(output),
                    None => carried = Some(output),
                },
                None => {}
            }
```

with `let mut carried: Option<String> = None;` beside `messages`, and after the loop:

```rust
        if reset {
            self.messages = messages;
        } else {
            self.messages.extend(messages);
            if let Some(output) = carried
                && let Some(last) = self.messages.last_mut()
            {
                last.output = Some(output);
            }
        }
```

(If the edition's let-chains are unavailable, nest the two `if let`s.)

- [ ] **Step 7: Run the tests**

Run: `cargo test transcript`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/transcript
git commit -m "feat: keep a command's output with the command"
```

---

### Task 2: Both speakers reach the phone as markdown

**Track:** `rust`

**Files:**
- Modify: `src/main.rs` (`Card`, `card`, its test)
- Modify: `src/markdown.rs` (delete `escape` and its test)

**Interfaces:**
- Consumes: `Message.output` from Task 1.
- Produces: the wire card `{ seq, role, preview, html, output? }`, `html` markdown-rendered for both roles.

- [ ] **Step 1: Rewrite the card test**

```rust
    #[test]
    fn both_speakers_cards_carry_rendered_markdown() {
        let user = card(&herdr_remote::transcript::Message {
            seq: 0,
            role: herdr_remote::transcript::Role::User,
            text: "1. <b>hi</b>".into(),
            output: Some("done".into()),
        });
        // The list renders, the raw tag does not.
        assert!(user.html.contains("<ol>"));
        assert!(user.html.contains("&lt;b&gt;hi&lt;/b&gt;"));
        assert_eq!(user.output.as_deref(), Some("done"));
    }
```

keeping the existing agent half of the assertion, and dropping `markdown::escape`'s own test.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --bin herdr-remote card`
Expected: FAIL — the user card is escaped text, and `Card` has no `output`.

- [ ] **Step 3: Render both halves the same way**

```rust
#[derive(Serialize)]
struct Card {
    seq: u64,
    role: transcript::Role,
    preview: String,
    html: String,
    /// A local command's output, shown under the command it answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}
```

```rust
fn card(message: &transcript::Message) -> Card {
    Card {
        seq: message.seq,
        role: message.role,
        preview: markdown::preview(&message.text, 300),
        // Both speakers' words are markdown: the renderer escapes raw HTML and
        // gates link and image schemes, so neither half can script the page.
        html: markdown::to_html(&message.text),
        // Command output is not markdown and not anyone's words; it travels as
        // text and the phone gives it a node of its own.
        output: message.output.clone(),
    }
}
```

Delete `markdown::escape` — `card` was its only caller.

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/markdown.rs
git commit -m "feat: render a user's own words as markdown"
```

---

### Task 3: Reading a message no longer freezes the controls

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro` (`openCard`, the expand handler, the Escape handler, the sheet CSS)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks rely on.

- [ ] **Step 1: Make the sheet non-modal**

In `openCard`, `fullEl.showModal()` becomes `fullEl.show()`, its comment saying what the screen sheet's already says: the controls below stay live. Both sheets sit in the same place, so each closes the other as it opens — `screenEl.close()` in `openCard`, `fullEl.close()` in the expand handler.

- [ ] **Step 2: Give it Escape**

```ts
      // Neither sheet is modal, so neither gets Escape handling of its own.
      addEventListener("keydown", (event) => {
        if (event.key !== "Escape") return;
        if (fullEl.open) fullEl.close();
        if (screenEl.open) screenEl.close();
      });
```

- [ ] **Step 3: Drop the dim**

Delete the `body:has(#full[open]) #composer { opacity: 0.55 }` rule and the `#full::backdrop` rule (a non-modal dialog paints no backdrop), and correct the comment above `#full, #screen` that explains the modal/non-modal split.

- [ ] **Step 4: Check it in the browser**

Run: `make run`, open a pane with history, tap a clipped card, and confirm the composer still types and sends with the sheet open, that Escape closes it, and that opening the screen sheet closes the message sheet.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "feat: leave the controls live while a message is open"
```

---

### Task 4: The output gets its own bubble

**Track:** `web`

**Files:**
- Modify: `web/src/lib/transcript.ts` (`Card`, `same`, `sentCard`, `modalContent`)
- Modify: `web/src/lib/transcript.test.ts`
- Modify: `web/src/pages/index.astro` (`cardEl`, `clipped`, `openCard`, transcript CSS)

**Interfaces:**
- Consumes: the wire card from Task 2 — `output?: string`.
- Produces: nothing other tasks rely on.

- [ ] **Step 1: Write the failing tests**

```ts
test("a card whose output changed is not the same card", () => {
	const one: Card = { seq: 1, role: "user", preview: "/model", html: "<p>/model</p>" };
	const two: Card = { ...one, output: "Set model to Opus 5" };
	expect(append([one], [two])).toEqual([two]);
});

test("only a card we are holding for the agent keeps the typed whitespace", () => {
	expect(modalContent(sentCard({ after: 1, text: "a\nb" }, 0)).wrap).toBe(true);
	expect(
		modalContent({ seq: 1, role: "user", preview: "a", html: "<p>a</p>" }).wrap,
	).toBe(false);
});
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd web && aube exec vitest run src/lib/transcript.test.ts`
Expected: FAIL — `output` is not on `Card`, and `modalContent` wraps every user card.

- [ ] **Step 3: Carry the field**

```ts
export type Card = {
	seq: number;
	role: "user" | "assistant";
	preview: string;
	html: string;
	/** A local command's own output, under the command it answers. */
	output?: string;
	/** Text we escaped here rather than markdown the server rendered, which is
	 *  only ever a message we are still holding for the agent. */
	wrap?: true;
};
```

`same` gains `a.output === b.output`; `sentCard` returns `wrap: true`; `modalContent` returns `wrap: card.wrap === true`.

- [ ] **Step 4: Run them and watch them pass**

Run: `cd web && aube exec vitest run src/lib/transcript.test.ts`
Expected: PASS.

- [ ] **Step 5: Paint the bubble**

The card's paint moves off the `li`, which becomes the lane, and onto the bubbles inside it, so a card can hold two. In `index.astro`:

```ts
      /** Lucide's corner-down-right, the mirror of the composer's enter arrow. */
      const FORK = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
        ><polyline points="15 10 20 15 15 20" /><path d="M4 4v7a4 4 0 0 0 4 4h12" /></svg>`;

      function bubble(text: string, clamp: boolean) {
        const paragraph = document.createElement("p");
        paragraph.className = clamp ? "preview bubble" : "bubble";
        // textContent: output is a command's own words, not our markup.
        paragraph.textContent = text;
        return paragraph;
      }

      /** The command's answer, marked with an arrow that sits outside the
       *  bubble and centred against it. */
      function outputRow(text: string, clamp: boolean) {
        const row = document.createElement("div");
        row.className = "out";
        row.innerHTML = FORK;
        row.append(bubble(text, clamp));
        return row;
      }
```

`cardEl` appends `<p class="preview bubble">` for the message and, when `card.output` is set, `outputRow(card.output, true)`. `openCard` appends `outputRow(card.output, false)` after the html it assigns. `clipped` asks every `.preview` in the button rather than the first.

- [ ] **Step 6: Move the paint in CSS**

```css
      #transcript li > button {
        display: flex;
        flex-direction: column;
        /* The same gap the list puts between cards. */
        gap: 0.5rem;
        width: 100%;
        border: 0;
        padding: 0;
        background: transparent;
        ...
      }
      .bubble {
        margin: 0;
        border-radius: 10px;
        padding: 0.5rem 0.7rem;
      }
      #transcript li[data-role="user"] .bubble {
        background: var(--card-user);
      }
      #transcript li[data-role="assistant"] .bubble {
        border: 1px solid var(--edge);
      }
      /* The arrow rides outside the bubble, a gap of its own to the left of
         it, and the bubble gives up that much width so both right edges line
         up. Centred, because a command's answer is a line or two. */
      .out {
        display: flex;
        align-items: center;
        gap: 0.5rem;
      }
      .out .bubble {
        flex: 1;
        min-width: 0;
      }
      .out svg {
        flex: none;
        width: 1rem;
        height: 1rem;
        color: var(--muted);
      }
      #full .body .bubble {
        white-space: pre-wrap;
      }
```

with the `background`, `border`, `border-radius`, and `padding` of the old `li` and `li > button` rules removed.

- [ ] **Step 7: Check it in the browser**

Run: `make run`, open a pane whose history holds a `/model` turn, and confirm the command and its output are two bubbles with the arrow between them, that the output bubble is narrower by the arrow and gap, and that the sheet shows both.

- [ ] **Step 8: Commit**

```bash
git add web/src/lib/transcript.ts web/src/lib/transcript.test.ts web/src/pages/index.astro
git commit -m "feat: show a command's output under the command"
```
