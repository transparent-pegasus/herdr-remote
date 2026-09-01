# Transcript View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use pane-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show an agent pane's own transcript file as a conversation-only history with three-line cards and a full-text modal, keep the live terminal screen as a small separate band, and stop losing history to the alternate screen and to pane-width wrapping.

**Architecture:** A new `transcript` module resolves a pane to its agent's transcript file (four formats), parses it incrementally into `{seq, role, text}`, and caches by byte offset. A `markdown` module renders the agent's text to HTML on the server with `pulldown-cmark` so no parser ships to the phone. A `live` module extracts the composer's first line from a screen snapshot. `main.rs` exposes `/transcript`, `/live`, `/open`, `/close` and holds the single zoomed-pane slot. The front end renders two lanes of clamped cards with a native `<dialog>`, plus a live band carrying an inline animated `gavel` and a `maximize-2` button.

**Tech Stack:** Rust (axum 0.8, tokio, serde), `pulldown-cmark` 0.13, `rusqlite` 0.40 (bundled), TypeScript, Astro 7, Vitest 4, Biome.

**Spec:** `docs/designs/2026-09-01-transcript-view.md`

## Global Constraints

- `any` is prohibited in TypeScript (repo rule, `CLAUDE.md`).
- Follow `.claude/skills/artful-simplicity/SKILL.md`: the bare minimum that works.
- `aube` is the only package manager for `web/`. Never `npm`/`pnpm`/`yarn`.
- No new JavaScript dependency. Markdown renders on the server; the modal, the clamp, and the animation are native HTML/CSS.
- No status text authored by us. Pane state is an icon or an animation, never a sentence.
- History carries user and assistant text only — no tool calls, tool output, thinking, or separators of any kind.
- Transcript fixtures are hand-written. Real transcripts carry the user's work and must not enter the repository.
- Rust tests live in `#[cfg(test)]` modules beside the code; TypeScript tests are colocated `*.test.ts`.
- Targeted tests: `cargo test <filter>` and `cd web && aube exec vitest run <path>`.
- Design and plan documents are coordination artifacts and are never committed.

## Tracks

| Track | Goal | Tasks | Owned files | Depends on |
|---|---|---|---|---|
| `rust-core` | Transcript parsing for four formats, markdown rendering, composer extraction | 1-9 | `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/transcript/**`, `src/markdown.rs`, `src/live.rs` | — |
| `web` | Cards, modal, live band, icons, polling | 10-14 | `web/src/lib/**`, `web/src/pages/index.astro` | — |
| `api` | herdr client extensions, routes, zoom slot, the end-to-end pass | 15-20 | `src/herdr.rs`, `src/main.rs` | `rust-core` |

`rust-core` and `web` start immediately and share no files. `api` consumes `rust-core`'s
types, so it runs after that track lands. `src/lib.rs` exists so `rust-core`'s modules
compile and test without touching `src/main.rs`, which `api` owns; `main.rs` reaches them
through `use herdr_remote::…`.

**Post-integration follow-ups:** update `README.md` and `CLAUDE.md` if the route list or the
dependency set is described there; run `make check` and the `make run` smoke check in the
coordination worktree; final branch review.

---

## Task 1: Crate skeleton and normalized types

**Track:** `rust-core`

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/transcript/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `herdr_remote::transcript::{Message, Role}`; `Message { seq: u64, role: Role, text: String }`, `Role::{User, Assistant}` serializing as `"user"` / `"assistant"`.

- [ ] **Step 1: Add the two dependencies**

`Cargo.toml`, in `[dependencies]`, keeping the existing alphabetical order:

```toml
pulldown-cmark = { version = "0.13.4", default-features = false, features = ["html"] }
rusqlite = { version = "0.40.2", features = ["bundled"] }
```

`bundled` compiles SQLite in, so the server does not depend on a system libsqlite3.
`default-features = false` drops pulldown-cmark's `getopts` dependency, which exists for
its command-line binary and not for `push_html`.

- [ ] **Step 2: Create the library root**

`src/lib.rs`:

```rust
//! The parts `main.rs` composes: reading an agent's transcript, rendering it,
//! and reading the one line a terminal screen still owns.

pub mod transcript;
```

Task 9 adds `live` and `markdown` when it creates those files. Declaring a module before
its file exists fails the build, so this task's own test could not run.

- [ ] **Step 3: Write the types and their test**

`src/transcript/mod.rs`:

```rust
//! An agent's own transcript, which holds what the terminal threw away: a pane
//! running on the alternate screen keeps no scrollback, so the file is the only
//! place its finished answers still exist.

use serde::Serialize;

/// Only the two speakers survive normalization. Tool calls, tool output,
/// thinking, and system preambles are dropped where they are parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// `seq` is the message's position in its file, and doubles as the cursor the
/// phone sends back as `before=` when it reaches further into the past.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub seq: u64,
    pub role: Role,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_serialize_in_the_shape_the_phone_reads() {
        let message = Message {
            seq: 3,
            role: Role::Assistant,
            text: "done".into(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(json, r#"{"seq":3,"role":"assistant","text":"done"}"#);
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test transcript::tests::roles_serialize`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/transcript/mod.rs
git commit -m "feat: add transcript module skeleton"
```

---

## Task 2: Preamble stripping

**Track:** `rust-core`

**Files:**
- Create: `src/transcript/preamble.rs`
- Modify: `src/transcript/mod.rs` (add `mod preamble;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn strip(text: &str) -> String` — used by all four parsers on every user message.

Every agent staples environment blocks onto the first user turn. Without this, each user
card previews as `<user_info>OS Version: linux`.

- [ ] **Step 1: Write the module and its tests**

`src/transcript/preamble.rs`:

```rust
//! The environment blocks every agent staples onto a user turn. What the person
//! actually typed is either inside `<user_query>` or is what remains once the
//! injected blocks are removed.

/// `<user_query>` wins when present: cursor and grok wrap the real prompt in it
/// and surround it with several kilobytes of environment.
pub fn strip(text: &str) -> String {
    if let Some(inner) = between(text, "<user_query>", "</user_query>") {
        return inner.trim().to_string();
    }
    let mut out = text.to_string();
    for (open, close) in [
        ("<system-reminder>", "</system-reminder>"),
        ("<user_info>", "</user_info>"),
        ("<timestamp>", "</timestamp>"),
    ] {
        out = drop_blocks(&out, open, close);
    }
    out.trim().to_string()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Every occurrence, not just the first: a turn can carry several reminders.
fn drop_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        match after.find(close) {
            Some(end) => rest = &after[end + close.len()..],
            // An unterminated block swallows the rest of the turn; dropping it
            // is better than pasting a half-open tag into the card.
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_query_block_is_the_whole_message() {
        let text = "<user_info>OS: linux</user_info>\n<user_query>\nfix the wrap\n</user_query>";
        assert_eq!(strip(text), "fix the wrap");
    }

    #[test]
    fn injected_blocks_are_removed_when_there_is_no_user_query() {
        let text = "read this\n<system-reminder>ignore me</system-reminder>\nand that";
        assert_eq!(strip(text), "read this\n\nand that");
    }

    #[test]
    fn every_occurrence_goes_not_just_the_first() {
        let text = "<system-reminder>a</system-reminder>keep<system-reminder>b</system-reminder>";
        assert_eq!(strip(text), "keep");
    }

    #[test]
    fn an_unterminated_block_does_not_leak_its_tag() {
        let text = "keep this\n<system-reminder>truncated…";
        assert_eq!(strip(text), "keep this");
    }

    #[test]
    fn plain_text_survives_untouched() {
        assert_eq!(strip("  just a prompt  "), "just a prompt");
    }
}
```

- [ ] **Step 2: Register the module**

In `src/transcript/mod.rs`, above the `use serde::Serialize;` line:

```rust
mod preamble;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test transcript::preamble`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/transcript/preamble.rs src/transcript/mod.rs
git commit -m "feat: strip agent preambles from user turns"
```

---

## Task 3: Claude parser

**Track:** `rust-core`

**Files:**
- Create: `src/transcript/claude.rs`
- Modify: `src/transcript/mod.rs` (add `mod claude;`)

**Interfaces:**
- Consumes: `preamble::strip`, `Message`, `Role`.
- Produces: `pub fn parse_line(line: &str, seq: u64) -> Option<Message>`.

Shape confirmed by reading real files: `{"type":"assistant","message":{"content":[{"type":"text"|"tool_use"|"thinking"}]}}`;
a user turn's `content` is either a bare string or a parts array; `attachment`, `system`,
`mode`, and the other bookkeeping types are not messages.

- [ ] **Step 1: Write the module and its tests**

`src/transcript/claude.rs`:

```rust
//! Claude Code's transcript: one JSON object per line under
//! `~/.claude/projects/<slug>/<session>.jsonl`.

use serde_json::Value;

use super::{Message, Role, preamble};

/// `None` for every line that is not a speaker: tool calls, tool results,
/// thinking, attachments, and the session's bookkeeping rows.
pub fn parse_line(line: &str, seq: u64) -> Option<Message> {
    let value: Value = serde_json::from_str(line).ok()?;
    let role = match value.get("type")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let content = value.get("message")?.get("content")?;

    let joined = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    // Only a user turn carries injected blocks. An assistant that quotes
    // `<system-reminder>` in its prose keeps every character.
    let text = if role == Role::User {
        preamble::strip(&joined)
    } else {
        joined
    };

    (!text.trim().is_empty()).then(|| Message { seq, role, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_user_turn_is_a_message() {
        let line = r#"{"type":"user","message":{"content":"fix the wrap"}}"#;
        let message = parse_line(line, 7).unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.text, "fix the wrap");
        assert_eq!(message.seq, 7);
    }

    #[test]
    fn assistant_text_parts_join_and_tool_parts_drop() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"thinking","thinking":""},
            {"type":"text","text":"first"},
            {"type":"tool_use","name":"Bash","input":{}},
            {"type":"text","text":"second"}]}}"#;
        let message = parse_line(line, 1).unwrap();
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.text, "first\nsecond");
    }

    #[test]
    fn a_tool_result_turn_is_not_a_message() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"x","content":"output"}]}}"#;
        assert!(parse_line(line, 1).is_none());
    }

    #[test]
    fn bookkeeping_rows_are_skipped() {
        for line in [
            r#"{"type":"attachment","sessionId":"s"}"#,
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"file-history-snapshot"}"#,
            "not json at all",
        ] {
            assert!(parse_line(line, 1).is_none(), "{line}");
        }
    }

    /// Stripping is a user-turn rule. An assistant explaining the tag keeps it.
    #[test]
    fn an_assistant_that_quotes_a_reminder_keeps_every_character() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"write <system-reminder>x</system-reminder> literally"}]}}"#;
        assert_eq!(
            parse_line(line, 1).unwrap().text,
            "write <system-reminder>x</system-reminder> literally"
        );
    }

    #[test]
    fn a_user_turn_loses_its_reminders() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"text","text":"do it<system-reminder>noise</system-reminder>"}]}}"#;
        assert_eq!(parse_line(line, 1).unwrap().text, "do it");
    }
}
```

- [ ] **Step 2: Register the module**

In `src/transcript/mod.rs`:

```rust
mod claude;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test transcript::claude`
Expected: 6 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/transcript/claude.rs src/transcript/mod.rs
git commit -m "feat: parse claude transcripts"
```

---

## Task 4: Codex parser

**Track:** `rust-core`

**Files:**
- Create: `src/transcript/codex.rs`
- Modify: `src/transcript/mod.rs` (add `mod codex;`)

**Interfaces:**
- Consumes: `preamble::strip`, `Message`, `Role`.
- Produces: `pub fn parse_line(line: &str, seq: u64) -> Option<Message>`.

Shape confirmed by reading a real rollout: messages are
`{"type":"response_item","payload":{"type":"message","role":…,"content":[{"type":"input_text"|"output_text","text":…}]}}`.
`role: "developer"` carries skills instructions and the full AGENTS.md and is dropped.
`event_msg` rows are duplicates of the same content (95 of 237 lines in the sample) and
are dropped.

- [ ] **Step 1: Write the module and its tests**

`src/transcript/codex.rs`:

```rust
//! Codex's rollout file: one JSON object per line under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<stamp>-<session>.jsonl`.

use serde_json::Value;

use super::{Message, Role, preamble};

/// Only `response_item` messages count. `event_msg` rows repeat the same
/// content as UI events, and `developer` messages are injected instructions.
pub fn parse_line(line: &str, seq: u64) -> Option<Message> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "response_item" {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "message" {
        return None;
    }
    let role = match payload.get("role")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };

    // Only the two text part types. A future reasoning or tool part that also
    // carries `text` must not leak into a history that shows two speakers.
    let joined = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text")
            )
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let text = if role == Role::User {
        preamble::strip(&joined)
    } else {
        joined
    };

    // Codex opens a session — and reopens a resumed one — by sending the
    // repository's instruction file as a user turn. It is wrapped in no tag, so
    // nothing else drops it, and it would otherwise be shown as something the
    // person said. Measured: turns 1 and 9 of a ten-turn rollout.
    if role == Role::User && text.starts_with("# AGENTS.md instructions for") {
        return None;
    }

    (!text.trim().is_empty()).then(|| Message { seq, role, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_message_survives_its_environment_block() {
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"<user_info>linux</user_info><user_query>review it</user_query>"}]}}"#;
        let message = parse_line(line, 2).unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.text, "review it");
    }

    /// Delimited `r##"…"##` because the fixture contains `"#`, which closes a
    /// single-hash raw string. Keep the heading to one `#`: `"##` would close
    /// this delimiter in turn.
    #[test]
    fn an_assistant_message_is_kept_verbatim() {
        let line = r##"{"type":"response_item","payload":{"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"# Verdict\nlooks fine"}]}}"##;
        assert_eq!(parse_line(line, 1).unwrap().text, "# Verdict\nlooks fine");
    }

    /// Measured: turns 1 and 9 of a ten-turn rollout were this, wrapped in no
    /// tag, so nothing else would have dropped them.
    #[test]
    fn the_injected_instruction_file_is_not_something_the_person_said() {
        let line = r##"{"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"# AGENTS.md instructions for /repo\n\nrules"}]}}"##;
        assert!(parse_line(line, 1).is_none());
    }

    #[test]
    fn a_part_that_is_not_a_text_part_is_ignored_even_carrying_text() {
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"assistant",
            "content":[{"type":"reasoning_text","text":"thought"},
                       {"type":"output_text","text":"answer"}]}}"#;
        assert_eq!(parse_line(line, 1).unwrap().text, "answer");
    }

    #[test]
    fn developer_messages_and_events_are_dropped() {
        for line in [
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"AGENTS.md"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage"}}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"shell"}}"#,
            r#"{"type":"session_meta","payload":{"id":"x"}}"#,
        ] {
            assert!(parse_line(line, 1).is_none(), "{line}");
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `src/transcript/mod.rs`:

```rust
mod codex;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test transcript::codex`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/transcript/codex.rs src/transcript/mod.rs
git commit -m "feat: parse codex rollouts"
```

---

## Task 5: Grok parser

**Track:** `rust-core`

**Files:**
- Create: `src/transcript/grok.rs`
- Modify: `src/transcript/mod.rs` (add `mod grok;`)

**Interfaces:**
- Consumes: `preamble::strip`, `Message`, `Role`.
- Produces: `pub fn parse_line(line: &str, seq: u64) -> Option<Message>`.

Shape confirmed by reading a real `chat_history.jsonl`: flat rows typed
`system` / `user` / `assistant` / `reasoning` / `tool_result` / `backend_tool_call`.
An assistant row's `content` is a plain string and its tool calls sit beside it in
`tool_calls`. A user row's `content` is a parts array. `reasoning` carries readable
`summary[].text` — grok is the only one of the four that does — and is dropped anyway,
because the history shows the two speakers only.

- [ ] **Step 1: Write the module and its tests**

`src/transcript/grok.rs`:

```rust
//! Grok's chat history: one JSON object per line under
//! `~/.grok/sessions/<encoded cwd>/<session>/chat_history.jsonl`.

use serde_json::Value;

use super::{Message, Role, preamble};

/// Assistant rows carry a string; user rows carry parts. `reasoning` is
/// readable here, unlike the other three formats, and is dropped regardless.
pub fn parse_line(line: &str, seq: u64) -> Option<Message> {
    let value: Value = serde_json::from_str(line).ok()?;
    let role = match value.get("type")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };

    let text = match value.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = if role == Role::User {
        preamble::strip(&text)
    } else {
        text
    };

    // Grok also files pure environment injections as user rows — measured: the
    // first three "user" rows of a session were an environment block and two
    // reminder dumps with no human text in them. Stripping empties those, and
    // an empty message is not a message.
    (!text.trim().is_empty()).then(|| Message { seq, role, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_assistant_row_carries_a_bare_string() {
        let line = r#"{"type":"assistant","content":"I'll confirm the directory first.",
            "tool_calls":[{"id":"call-1","name":"run_terminal_command"}]}"#;
        let message = parse_line(line, 4).unwrap();
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.text, "I'll confirm the directory first.");
    }

    #[test]
    fn a_user_row_carries_parts_and_loses_its_environment() {
        let line = r#"{"type":"user","content":[{"type":"text",
            "text":"<user_info>linux</user_info>\n<user_query>sweep the docs</user_query>"}]}"#;
        assert_eq!(parse_line(line, 1).unwrap().text, "sweep the docs");
    }

    /// Measured: grok's first three "user" rows were an environment block and
    /// two reminder dumps carrying no human text at all.
    #[test]
    fn an_environment_only_user_row_is_not_a_message() {
        let line = r#"{"type":"user","content":[{"type":"text",
            "text":"<system-reminder>skills available…</system-reminder>"}]}"#;
        assert!(parse_line(line, 1).is_none());
    }

    #[test]
    fn a_part_that_is_not_a_text_part_is_ignored() {
        let line = r#"{"type":"user","content":[{"type":"image","text":"alt text"},
                                                {"type":"text","text":"real prompt"}]}"#;
        assert_eq!(parse_line(line, 1).unwrap().text, "real prompt");
    }

    #[test]
    fn reasoning_tool_results_and_system_rows_are_dropped() {
        for line in [
            r#"{"type":"reasoning","summary":[{"type":"summary_text","text":"visible thought"}]}"#,
            r#"{"type":"tool_result","tool_call_id":"c","content":"output"}"#,
            r#"{"type":"system","content":"You are Grok"}"#,
            r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search"}}"#,
        ] {
            assert!(parse_line(line, 1).is_none(), "{line}");
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `src/transcript/mod.rs`:

```rust
mod grok;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test transcript::grok`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/transcript/grok.rs src/transcript/mod.rs
git commit -m "feat: parse grok chat history"
```

---

## Task 6: Cursor parser

**Track:** `rust-core`

**Files:**
- Create: `src/transcript/cursor.rs`
- Modify: `src/transcript/mod.rs` (add `mod cursor;`)

**Interfaces:**
- Consumes: `preamble::strip`, `Message`, `Role`.
- Produces: `pub fn read(db: &Path) -> Result<(String, Vec<Message>)>` — the root blob id (the cache's version token) and the conversation in order.

Shape confirmed by opening a real store: `meta.value` is hex-encoded JSON holding
`latestRootBlobId`; that blob is protobuf carrying a repeated 32-byte id list, each entry
framed as `0x0a 0x20` followed by the id, in conversation order; each referenced blob is a
JSON message in Vercel AI SDK shape (`role: system|user|assistant|tool`, `content` either
a string or parts with `type: text | reasoning | tool-call`).

Opening mode matters: `file:<path>?mode=ro` reads a live database with a `-wal` beside it,
while adding `immutable=1` fails with "no such table: blobs". Verified on a running cursor
session.

- [ ] **Step 1: Write the module and its tests**

`src/transcript/cursor.rs`:

```rust
//! Cursor's store: a SQLite blob table whose root blob lists the conversation's
//! message ids in order. The ids herdr reports do not address this store, so the
//! caller resolves the directory by cwd and title before handing over the path.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{Message, Role, preamble};

/// Read-only, but never `immutable`: a live session keeps a `-wal` file, and an
/// immutable open reads the pre-WAL pages and finds no tables at all. Measured
/// against a running cursor session.
fn open_ro(db: &Path) -> Result<Connection> {
    let uri = format!("file:{}?mode=ro", db.display());
    Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open cursor store at {}", db.display()))
}

fn root_of(connection: &Connection) -> Result<String> {
    let meta: String = connection
        .query_row("select value from meta limit 1", [], |row| row.get(0))
        .context("cursor store has no meta row")?;
    let meta: Value = serde_json::from_slice(&decode_hex(&meta).context("meta is not hex")?)
        .context("meta is not JSON")?;
    Ok(meta
        .get("latestRootBlobId")
        .and_then(Value::as_str)
        .context("meta has no latestRootBlobId")?
        .to_string())
}

/// The version token on its own. A poll asks for this and stops there unless it
/// changed, so an unchanged conversation costs one row read rather than a walk
/// of every blob in the store.
pub fn root_id(db: &Path) -> Result<String> {
    root_of(&open_ro(db)?)
}

/// Returns the root blob id and the messages it points at, in order.
pub fn read(db: &Path) -> Result<(String, Vec<Message>)> {
    let connection = open_ro(db)?;
    let root = root_of(&connection)?;

    // `seq` counts kept messages, exactly as the line formats do, so the
    // phone's `before=` cursor means the same thing for every agent.
    let mut messages: Vec<Message> = Vec::new();
    for id in blob_ids(&blob(&connection, &root)?)? {
        // A referenced blob that is not there means the store is damaged. A
        // conversation with a silent hole in it looks real and is not; the
        // caller turns this into the raw-output fallback instead.
        let bytes = blob(&connection, &id)?;
        if let Some(message) = parse_blob(&bytes, messages.len() as u64) {
            messages.push(message);
        }
    }
    Ok((root, messages))
}

fn blob(connection: &Connection, id: &str) -> Result<Vec<u8>> {
    connection
        .query_row("select data from blobs where id = ?1", [id], |row| row.get(0))
        .with_context(|| format!("cursor blob {id} is missing"))
}

/// The root blob is protobuf: a repeated length-delimited field, every entry a
/// 32-byte id. Reading the two framing bytes is cheaper and more predictable
/// than pulling in a protobuf runtime for one shape.
///
/// Framing that does not parse to the end is an error rather than a shorter
/// list: a truncated read would render as a complete conversation that is
/// quietly missing its most recent messages.
fn blob_ids(root: &[u8]) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut rest = root;
    while !rest.is_empty() {
        let (tag, len) = match rest {
            [tag, len, ..] => (*tag, *len as usize),
            _ => bail!("cursor root blob ends mid-frame"),
        };
        if tag != 0x0a || rest.len() < 2 + len {
            bail!("cursor root blob has unexpected framing");
        }
        ids.push(hex(&rest[2..2 + len]));
        rest = &rest[2 + len..];
    }
    Ok(ids)
}

fn parse_blob(bytes: &[u8], seq: u64) -> Option<Message> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let role = match value.get("role")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let text = match value.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = if role == Role::User {
        preamble::strip(&text)
    } else {
        text
    };
    (!text.trim().is_empty()).then(|| Message { seq, role, text })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    (text.len() % 2 == 0)
        .then(|| {
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
                .collect::<Option<Vec<u8>>>()
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path, messages: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.join("store.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("create table blobs (id text primary key, data blob)", [])
            .unwrap();
        connection
            .execute("create table meta (key text primary key, value text)", [])
            .unwrap();

        let mut root = Vec::new();
        for (index, (role, text)) in messages.iter().enumerate() {
            let id = format!("{index:064x}");
            let body = serde_json::json!({ "role": role, "content": text }).to_string();
            connection
                .execute(
                    "insert into blobs values (?1, ?2)",
                    rusqlite::params![id, body.as_bytes()],
                )
                .unwrap();
            root.push(0x0a);
            root.push(32);
            root.extend(decode_hex(&id).unwrap());
        }
        let root_id = format!("{:064x}", 999);
        connection
            .execute(
                "insert into blobs values (?1, ?2)",
                rusqlite::params![root_id, root],
            )
            .unwrap();
        let meta = serde_json::json!({ "latestRootBlobId": root_id }).to_string();
        connection
            .execute(
                "insert into meta values ('0', ?1)",
                [hex(meta.as_bytes())],
            )
            .unwrap();
        path
    }

    #[test]
    fn the_root_blob_orders_the_conversation() {
        let dir = tempdir();
        let path = store(
            &dir,
            &[
                ("system", "you are an assistant"),
                ("user", "<user_query>sweep the docs</user_query>"),
                ("assistant", "on it"),
                ("tool", "output"),
            ],
        );
        let (root, messages) = read(&path).unwrap();
        assert_eq!(root.len(), 64);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text, "sweep the docs");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].text, "on it");
    }

    /// A hole in the conversation must not render as a whole conversation.
    #[test]
    fn a_missing_blob_refuses_rather_than_leaving_a_hole() {
        let dir = tempdir();
        let path = store(&dir, &[("user", "one"), ("assistant", "two")]);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("delete from blobs where id = ?1", [format!("{:064x}", 0)])
            .unwrap();
        drop(connection);
        assert!(read(&path).is_err());
    }

    #[test]
    fn a_truncated_root_blob_refuses() {
        assert!(blob_ids(&[0x0a, 32, 0, 0]).is_err());
        assert!(blob_ids(&[0x12, 32]).is_err());
        assert!(blob_ids(&[]).unwrap().is_empty());
    }

    /// A scratch directory. The test process removes it on the way in rather
    /// than on the way out, so a failed run leaves its evidence behind.
    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-remote-cursor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
```

- [ ] **Step 2: Register the module**

In `src/transcript/mod.rs`:

```rust
mod cursor;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test transcript::cursor`
Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/transcript/cursor.rs src/transcript/mod.rs
git commit -m "feat: parse cursor blob stores"
```

---

## Task 7: Resolution and the path boundary

**Track:** `rust-core`

**Files:**
- Modify: `src/transcript/mod.rs`

**Interfaces:**
- Consumes: the four parsers.
- Produces:
  - `pub struct PaneRef<'a> { pub agent: &'a str, pub session_kind: Option<&'a str>, pub session_value: Option<&'a str>, pub cwd: &'a str, pub title: Option<&'a str> }`
  - `pub enum Source { Lines { path: PathBuf, format: Format }, CursorDb { path: PathBuf } }`
  - `pub enum Format { Claude, Codex, Grok }`
  - `pub fn resolve(pane: &PaneRef) -> Option<Source>`
  - `pub fn guard(path: PathBuf) -> Option<PathBuf>`

`agent_session` is self-reported through `pane.report_agent_session`, so any process that
can reach the herdr socket can name a path. `guard` canonicalizes and requires one of four
roots; anything else resolves to "no transcript" and the pane falls back to raw output.

It is also frequently absent. Measured with all four agents running in one tab: the grok
pane reported no session, and both codex panes reported none until they had taken a turn.
Every agent therefore has a cwd-based path to its file, and the id is only a shortcut.
Cursor additionally needs the `createdAtMs` floor described in `cursor_store`: a cwd can
hold months of unrelated chats, and the newest of those must not be shown as this pane's
history.

- [ ] **Step 1: Write the module and its tests**

Append to `src/transcript/mod.rs`:

```rust
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// What resolution needs from a pane, without depending on herdr's wire types.
pub struct PaneRef<'a> {
    pub agent: &'a str,
    pub session_kind: Option<&'a str>,
    pub session_value: Option<&'a str>,
    pub cwd: &'a str,
    pub title: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Claude,
    Codex,
    Grok,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// One JSON object per line, appended as the session runs.
    Lines { path: PathBuf, format: Format },
    /// A blob store with no byte offset to resume from.
    CursorDb { path: PathBuf },
}

/// The operator's home, read once per call rather than captured, so tests can
/// hand in a scratch tree instead of mutating the process environment.
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Where each agent's transcript is allowed to be, and what it must be called.
/// Pairing the root with the shape is what stops a reported path from naming an
/// arbitrary file under someone else's root and having it read as this agent's
/// history.
fn allowed(agent: &str) -> Option<(&'static str, fn(&str) -> bool)> {
    match agent {
        "claude" => Some((".claude/projects", |name| name.ends_with(".jsonl"))),
        "codex" => Some((".codex/sessions", |name| name.ends_with(".jsonl"))),
        "grok" => Some((".grok/sessions", |name| name == "chat_history.jsonl")),
        "cursor" => Some((".cursor/chats", |name| name == "store.db")),
        _ => None,
    }
}

/// `agent_session` is self-reported, so a path arrives as a claim. Canonicalize
/// both sides — a symlinked `~/.claude` must still match — then require this
/// agent's own root and this agent's own file shape.
pub fn guard(agent: &str, path: PathBuf, home: &Path) -> Option<PathBuf> {
    let (root, shaped) = allowed(agent)?;
    let root = home.join(root).canonicalize().ok()?;
    let real = path.canonicalize().ok()?;
    let name = real.file_name().and_then(|name| name.to_str())?;
    (shaped(name) && real.starts_with(&root)).then_some(real)
}

/// Claude names a project directory after its cwd, with `/` and `.` replaced.
fn claude_slug(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

pub fn resolve(pane: &PaneRef, home: &Path) -> Option<Source> {
    if pane.session_kind == Some("path") {
        let path = guard(pane.agent, PathBuf::from(pane.session_value?), home)?;
        return source_for(pane.agent, path);
    }
    match pane.agent {
        "claude" => {
            let projects = home.join(".claude/projects");
            let by_cwd = projects.join(claude_slug(pane.cwd));
            let path = match pane.session_value {
                Some(id) => guard("claude", by_cwd.join(format!("{id}.jsonl")), home).or_else(|| {
                    let wanted = format!("{id}.jsonl");
                    files(&projects, 2)
                        .into_iter()
                        .find(|path| path.file_name().and_then(|n| n.to_str()) == Some(&wanted))
                        .and_then(|path| guard("claude", path, home))
                }),
                // No id reported: the newest transcript in this cwd's own
                // project directory is this pane's, or there is none.
                None => newest(files(&by_cwd, 0)).and_then(|path| guard("claude", path, home)),
            }?;
            Some(Source::Lines {
                path,
                format: Format::Claude,
            })
        }
        "codex" => {
            let sessions = home.join(".codex/sessions");
            // A codex pane reports no session id until it has taken a turn, so
            // the id is an optimization; the rollout's own `session_meta` names
            // the cwd and is the fallback. A pane that never ran has no rollout
            // at all, which correctly resolves to no transcript.
            let path = match pane.session_value {
                Some(id) => {
                    let wanted = format!("-{id}.jsonl");
                    files(&sessions, 4)
                        .into_iter()
                        .find(|path| {
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|name| name.ends_with(&wanted))
                        })
                }
                None => newest_rollout_for(&sessions, pane.cwd),
            }
            .and_then(|path| guard("codex", path, home))?;
            Some(Source::Lines {
                path,
                format: Format::Codex,
            })
        }
        "grok" => {
            let dir = home.join(".grok/sessions").join(encode_cwd(pane.cwd));
            let session = match pane.session_value {
                Some(id) => dir.join(id),
                None => newest_child(&dir)?,
            };
            let path = guard("grok", session.join("chat_history.jsonl"), home)?;
            Some(Source::Lines {
                path,
                format: Format::Grok,
            })
        }
        "cursor" => {
            let path = guard(
                "cursor",
                cursor_store(pane.cwd, pane.title, pane.session_value, home)?,
                home,
            )?;
            Some(Source::CursorDb { path })
        }
        _ => None,
    }
}

fn source_for(agent: &str, path: PathBuf) -> Option<Source> {
    match agent {
        "claude" => Some(Source::Lines { path, format: Format::Claude }),
        "codex" => Some(Source::Lines { path, format: Format::Codex }),
        "grok" => Some(Source::Lines { path, format: Format::Grok }),
        "cursor" => Some(Source::CursorDb { path }),
        _ => None,
    }
}

/// grok names its session directories after the percent-encoded cwd.
fn encode_cwd(cwd: &str) -> String {
    cwd.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Every file under `dir`, at most `depth` directories deep. One walker, so the
/// four agents differ in what they filter for rather than in how they search.
fn files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                out.extend(files(&path, depth - 1));
            }
        } else {
            out.push(path);
        }
    }
    out
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn newest(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.into_iter().max_by_key(|path| modified(path))
}

/// grok's sessions are directories; a stray file beside them is not one.
fn newest_child(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .max_by_key(|path| modified(path))
}

/// Read one `meta.json`, or `None` when it is missing or unreadable.
fn cursor_meta(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join("meta.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Cursor's reported session id does not address its store: the id herdr sees
/// belongs to a directory that may hold nothing but a prompt history, while the
/// conversation lives under a different id created when the chat began.
///
/// Matching on cwd alone is not enough either — a directory can hold months of
/// unrelated chats, and the newest of those would be shown as this pane's
/// history. The reported session's own `createdAtMs` is the floor: the real
/// conversation cannot predate the session that is asking for it.
fn cursor_store(
    cwd: &str,
    title: Option<&str>,
    session: Option<&str>,
    home: &Path,
) -> Option<PathBuf> {
    let chats = home.join(".cursor/chats");
    let floor = session
        .and_then(|id| {
            files(&chats, 2)
                .into_iter()
                .find(|path| {
                    path.file_name().and_then(|n| n.to_str()) == Some("meta.json")
                        && path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
                            == Some(id)
                })
                .and_then(|path| path.parent().map(Path::to_path_buf))
        })
        .and_then(|dir| cursor_meta(&dir))
        .and_then(|meta| meta.get("createdAtMs").and_then(|v| v.as_u64()))
        .unwrap_or_default();

    let mut best: Option<(bool, u64, PathBuf)> = None;
    for project in std::fs::read_dir(&chats).ok()?.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let dir = session.path();
            let Some(meta) = cursor_meta(&dir) else {
                continue;
            };
            if meta.get("cwd").and_then(|v| v.as_str()) != Some(cwd)
                || meta.get("hasConversation").and_then(|v| v.as_bool()) != Some(true)
            {
                continue;
            }
            let updated = meta
                .get("updatedAtMs")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            if updated < floor {
                continue;
            }
            let titled = title.is_some() && meta.get("title").and_then(|v| v.as_str()) == title;
            let candidate = (titled, updated, dir.join("store.db"));
            if best.as_ref().is_none_or(|seen| candidate > *seen) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, path)| path)
}

/// Codex names its rollouts after a timestamp and a session id, never after the
/// directory it ran in; the cwd lives in the file's first line.
fn newest_rollout_for(sessions: &Path, cwd: &str) -> Option<PathBuf> {
    let mut rollouts: Vec<PathBuf> = files(sessions, 4)
        .into_iter()
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    // The name starts with an ISO timestamp, so sorting by it is sorting by
    // start time without opening anything.
    rollouts.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    rollouts.into_iter().find(|path| {
        first_line(path)
            .and_then(|line| {
                let meta: serde_json::Value = serde_json::from_str(&line).ok()?;
                Some(meta.get("payload")?.get("cwd")?.as_str()? == cwd)
            })
            .unwrap_or(false)
    })
}

/// `session_meta` is the first line, and a rollout can reach tens of megabytes;
/// reading the whole file to look at its head would undo the cache this feeds.
fn first_line(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    /// A scratch home per test. Nothing mutates the process environment, so the
    /// suite still runs in parallel with everything else.
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-remote-resolve-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn pane<'a>(agent: &'a str, value: Option<&'a str>, cwd: &'a str) -> PaneRef<'a> {
        PaneRef {
            agent,
            session_kind: value.map(|_| "id"),
            session_value: value,
            cwd,
            title: None,
        }
    }

    #[test]
    fn a_claude_session_resolves_through_its_cwd_slug() {
        let home = scratch();
        let path = home.join(".claude/projects/-mnt-ssd1-repos-herdr-remote/abc.jsonl");
        touch(&path);
        let source = resolve(
            &pane("claude", Some("abc"), "/mnt/ssd1/repos/herdr-remote"),
            &home,
        )
        .unwrap();
        assert_eq!(
            source,
            Source::Lines {
                path: path.canonicalize().unwrap(),
                format: Format::Claude
            }
        );
    }

    #[test]
    fn a_claude_pane_without_an_id_takes_the_newest_transcript_in_its_project() {
        let home = scratch();
        let project = home.join(".claude/projects/-repo");
        touch(&project.join("older.jsonl"));
        touch(&project.join("newer.jsonl"));
        // mtime, not name, decides; make the intended winner unambiguously newer.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::open(project.join("newer.jsonl"))
            .unwrap()
            .set_modified(later)
            .unwrap();

        match resolve(&pane("claude", None, "/repo"), &home).unwrap() {
            Source::Lines { path, .. } => {
                assert!(path.to_string_lossy().ends_with("newer.jsonl"), "{path:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reported_path_outside_the_allowed_roots_is_refused() {
        let home = scratch();
        touch(&home.join(".claude/projects/-repo/real.jsonl"));
        let outside = home.join("elsewhere/secrets.jsonl");
        touch(&outside);
        assert!(
            resolve(
                &PaneRef {
                    agent: "claude",
                    session_kind: Some("path"),
                    session_value: outside.to_str(),
                    cwd: "/repo",
                    title: None,
                },
                &home
            )
            .is_none()
        );
    }

    #[test]
    fn a_symlink_out_of_an_allowed_root_is_refused() {
        let home = scratch();
        let outside = home.join("elsewhere/secrets.jsonl");
        touch(&outside);
        let inside = home.join(".claude/projects/-repo/link.jsonl");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &inside).unwrap();
        assert!(guard("claude", inside, &home).is_none());
    }

    #[test]
    fn a_file_inside_a_root_that_is_not_a_transcript_is_refused() {
        let home = scratch();
        let path = home.join(".claude/projects/-repo/settings.json");
        touch(&path);
        assert!(guard("claude", path, &home).is_none());
    }

    /// Each agent's root belongs to that agent. A file under claude's root is
    /// not a cursor store just because someone reported it as one.
    #[test]
    fn one_agents_file_is_not_another_agents_transcript() {
        let home = scratch();
        let claude_side = home.join(".claude/projects/-repo/store.db");
        touch(&claude_side);
        assert!(guard("cursor", claude_side.clone(), &home).is_none());
        assert!(guard("claude", claude_side, &home).is_none());

        let grok_side = home.join(".grok/sessions/-repo/s1/notes.jsonl");
        touch(&grok_side);
        assert!(guard("grok", grok_side, &home).is_none());
    }

    #[test]
    fn cursor_prefers_the_store_whose_title_is_the_panes_own() {
        let home = scratch();
        for (dir, title, updated) in [
            ("chats/p1/older", "Review Steering Digest", 10_u64),
            ("chats/p1/newer", "Something Else", 99),
        ] {
            let base = home.join(".cursor").join(dir);
            touch(&base.join("store.db"));
            std::fs::write(
                base.join("meta.json"),
                serde_json::json!({
                    "cwd": "/repo", "hasConversation": true,
                    "title": title, "updatedAtMs": updated
                })
                .to_string(),
            )
            .unwrap();
        }
        let source = resolve(
            &PaneRef {
                agent: "cursor",
                session_kind: Some("id"),
                session_value: Some("an-id-that-addresses-nothing"),
                cwd: "/repo",
                title: Some("Review Steering Digest"),
            },
            &home,
        )
        .unwrap();
        match source {
            Source::CursorDb { path } => {
                assert!(path.to_string_lossy().contains("older"), "{path:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_chat_older_than_the_session_is_not_this_panes_history() {
        let home = scratch();
        // The pane's own cursor session: no conversation of its own.
        let mine = home.join(".cursor/chats/p1/mine");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(
            mine.join("meta.json"),
            serde_json::json!({ "cwd": "/repo", "hasConversation": false,
                                "createdAtMs": 500u64, "updatedAtMs": 500u64 })
            .to_string(),
        )
        .unwrap();
        // Yesterday's unrelated chat in the same directory.
        let stale = home.join(".cursor/chats/p1/stale");
        touch(&stale.join("store.db"));
        std::fs::write(
            stale.join("meta.json"),
            serde_json::json!({ "cwd": "/repo", "hasConversation": true,
                                "title": "Slow Count", "createdAtMs": 100u64,
                                "updatedAtMs": 200u64 })
            .to_string(),
        )
        .unwrap();

        assert!(resolve(&pane("cursor", Some("mine"), "/repo"), &home).is_none());
    }

    #[test]
    fn a_codex_pane_without_an_id_matches_its_rollout_by_cwd() {
        let home = scratch();
        let sessions = home.join(".codex/sessions/2026/09/01");
        std::fs::create_dir_all(&sessions).unwrap();
        for (name, cwd) in [
            ("rollout-2026-09-01T01-00-00-aaa.jsonl", "/other"),
            ("rollout-2026-09-01T02-00-00-bbb.jsonl", "/repo"),
        ] {
            std::fs::write(
                sessions.join(name),
                format!(
                    "{}\n",
                    serde_json::json!({ "type": "session_meta",
                                        "payload": { "cwd": cwd } })
                ),
            )
            .unwrap();
        }
        match resolve(&pane("codex", None, "/repo"), &home).unwrap() {
            Source::Lines { path, format } => {
                assert_eq!(format, Format::Codex);
                assert!(path.to_string_lossy().ends_with("-bbb.jsonl"), "{path:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unknown_agent_has_no_transcript() {
        let home = scratch();
        assert!(resolve(&pane("devin", Some("x"), "/repo"), &home).is_none());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test transcript::resolution_tests`
Expected: 10 tests PASS. Each test builds its own scratch home and passes it in, so
nothing mutates the process environment and the suite still runs in parallel.

- [ ] **Step 3: Commit**

```bash
git add src/transcript/mod.rs
git commit -m "feat: resolve panes to transcripts behind a path boundary"
```

---

## Task 8: Incremental cache

**Track:** `rust-core`

**Files:**
- Modify: `src/transcript/mod.rs`

**Interfaces:**
- Consumes: `Source`, `Format`, the parsers.
- Produces: `pub struct Transcript`, `Transcript::open(Source) -> Transcript`, `Transcript::refresh(&mut self) -> Result<()>`, `Transcript::version(&self) -> String`, `Transcript::messages(&self) -> &[Message]`.

The largest transcript on this host is 44 MB. Walking it on every three-second poll is
not an option, so a line source remembers its byte offset and parses only what was
appended; a shrinking file means truncation or rotation and is re-read from the start.

- [ ] **Step 1: Write the module and its tests**

Append to `src/transcript/mod.rs`:

```rust
use std::io::{Seek, SeekFrom};

/// A parsed transcript plus the bookkeeping that lets a poll skip the file
/// entirely when nothing was appended.
pub struct Transcript {
    source: Source,
    /// Bytes already parsed, for a line source. Unused for a blob store.
    offset: u64,
    /// The file length last observed. It can sit ahead of `offset` while an
    /// agent is midway through writing a line; remembering it is what stops
    /// every later poll from reopening the file to re-read that same fragment.
    len: u64,
    /// Next message's position, which keeps growing across refreshes.
    next_seq: u64,
    /// The blob store's root id, which changes when the conversation grows.
    root: String,
    messages: Vec<Message>,
}

impl Transcript {
    pub fn open(source: Source) -> Self {
        Self {
            source,
            offset: 0,
            len: 0,
            next_seq: 0,
            root: String::new(),
            messages: Vec::new(),
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// What the response's ETag carries. Same value means the phone already has
    /// this conversation and the body can be skipped.
    pub fn version(&self) -> String {
        match &self.source {
            Source::Lines { .. } => self.offset.to_string(),
            Source::CursorDb { .. } => self.root.clone(),
        }
    }

    pub fn refresh(&mut self) -> Result<()> {
        match self.source.clone() {
            Source::Lines { path, format } => self.refresh_lines(&path, format),
            Source::CursorDb { path } => {
                // One row read answers "did anything change"; walking every
                // blob is reserved for when it did.
                let root = cursor::root_id(&path)?;
                if root != self.root {
                    let (root, messages) = cursor::read(&path)?;
                    self.root = root;
                    self.messages = messages;
                }
                Ok(())
            }
        }
    }

    fn refresh_lines(&mut self, path: &Path, format: Format) -> Result<()> {
        let len = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        if len == self.len {
            return Ok(());
        }
        self.len = len;
        if len < self.offset {
            // Truncated or rotated: the offset now points past the end, so the
            // only honest reading is from the start.
            self.offset = 0;
            self.next_seq = 0;
            self.messages.clear();
        }

        let mut file = std::fs::File::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line)?;
            if read == 0 {
                break;
            }
            // A line still being written has no terminator yet; leave it for the
            // next refresh rather than parsing half an object.
            if !line.ends_with('\n') {
                break;
            }
            self.offset += read as u64;
            let parsed = match format {
                Format::Claude => claude::parse_line(&line, self.next_seq),
                Format::Codex => codex::parse_line(&line, self.next_seq),
                Format::Grok => grok::parse_line(&line, self.next_seq),
            };
            if let Some(message) = parsed {
                self.messages.push(message);
                self.next_seq += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::io::Write;

    fn scratch_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-remote-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.jsonl")
    }

    fn append(path: &Path, line: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn user(text: &str) -> String {
        serde_json::json!({ "type": "user", "message": { "content": text } }).to_string()
    }

    #[test]
    fn an_append_parses_only_the_new_bytes() {
        let path = scratch_file();
        append(&path, &user("first"));
        let mut transcript = Transcript::open(Source::Lines {
            path: path.clone(),
            format: Format::Claude,
        });
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 1);
        let after_first = transcript.version();

        append(&path, &user("second"));
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 2);
        assert_eq!(transcript.messages()[1].seq, 1);
        assert_ne!(transcript.version(), after_first);
    }

    /// The point of the offset: a line already parsed is never read again. If
    /// the refresh silently reparsed from the start, the corrupted first line
    /// would take the message it produced with it.
    #[test]
    fn an_already_parsed_line_survives_being_corrupted_afterwards() {
        let path = scratch_file();
        append(&path, &user("first"));
        let mut transcript = Transcript::open(Source::Lines {
            path: path.clone(),
            format: Format::Claude,
        });
        transcript.refresh().unwrap();

        // Same byte length, so only the content changes and the offset stays
        // meaningful; the line now types as something the parser drops.
        let corrupted = std::fs::read_to_string(&path)
            .unwrap()
            .replacen("\"user\"", "\"brok\"", 1);
        std::fs::write(&path, corrupted).unwrap();
        append(&path, &user("second"));
        transcript.refresh().unwrap();

        assert_eq!(transcript.messages().len(), 2);
        assert_eq!(transcript.messages()[0].text, "first");
        assert_eq!(transcript.messages()[1].text, "second");
    }

    #[test]
    fn an_unchanged_file_leaves_the_version_alone() {
        let path = scratch_file();
        append(&path, &user("only"));
        let mut transcript = Transcript::open(Source::Lines {
            path,
            format: Format::Claude,
        });
        transcript.refresh().unwrap();
        let version = transcript.version();
        transcript.refresh().unwrap();
        assert_eq!(transcript.version(), version);
        assert_eq!(transcript.messages().len(), 1);
    }

    #[test]
    fn a_shrunk_file_is_read_from_the_start() {
        let path = scratch_file();
        append(&path, &user("first"));
        append(&path, &user("second"));
        let mut transcript = Transcript::open(Source::Lines {
            path: path.clone(),
            format: Format::Claude,
        });
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 2);

        std::fs::write(&path, format!("{}\n", user("only one now"))).unwrap();
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.messages()[0].text, "only one now");
        assert_eq!(transcript.messages()[0].seq, 0);
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let path = scratch_file();
        append(&path, &user("complete"));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{}", user("still being written")).unwrap();
        drop(file);

        let mut transcript = Transcript::open(Source::Lines {
            path: path.clone(),
            format: Format::Claude,
        });
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 1);

        append(&path, "");
        transcript.refresh().unwrap();
        assert_eq!(transcript.messages().len(), 2);
    }
}
```

`Source` already derives `Clone` in Task 7, and Task 7 already imports
`anyhow::{Context, Result}`; nothing else needs adding.

- [ ] **Step 2: Run the tests**

Run: `cargo test transcript::cache_tests`
Expected: 5 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/transcript/mod.rs
git commit -m "feat: cache transcripts incrementally"
```

---

## Task 9: Markdown rendering, preview, and composer extraction

**Track:** `rust-core`

**Files:**
- Create: `src/markdown.rs`
- Create: `src/live.rs`
- Modify: `src/lib.rs` (add `pub mod live;` and `pub mod markdown;`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces:
  - `pub fn markdown::to_html(md: &str) -> String`
  - `pub fn markdown::escape(text: &str) -> String`
  - `pub fn markdown::preview(md: &str, cap: usize) -> String`
  - `pub fn live::composer(agent: &str, screen: &str) -> Option<String>`

`to_html` escapes raw HTML rather than dropping it, because agent prose contains `<Foo>`
and `<system-reminder>`; comrak deletes those. Link destinations are limited to relative
URLs and `http`/`https`/`mailto`.

`composer` returns the draft's **first line only**. All four boxes were measured on this
host with the agents running side by side:

| agent | box | empty |
|---|---|---|
| claude | `❯ draft` inside `───` rules | `❯` |
| grok | `❯ draft`, no rules | `❯` |
| codex | `› draft` | `› Ask Codex to do an…` |
| cursor | `→ draft` between `▄▄▄` and `▀▀▀` | `→ Plan, search, build anything` or `→ Add a follow-up` |

Claude and grok share one rule — the **last** row beginning with `❯` — which also disposes
of the trap that the same glyph prefixes submitted messages echoed further up: the
composer is always the last one. Not searching for rules is what lets grok's rule-less box
work. Cursor keeps a border lookup because `→` appears in ordinary output. Cursor has two
placeholders, one for a fresh session and one for a session that has answered.

- [ ] **Step 1: Write the markdown module and its tests**

`src/markdown.rs`:

```rust
//! Markdown to HTML on the server, so no parser ships to the phone.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// `ENABLE_GFM` is deliberately absent: in 0.13 it only adds alert blockquotes,
/// which nothing here renders. Tables and task lists are their own flags.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// A destination the page may follow. Anything with a scheme that is not
/// http, https, or mailto is emptied — `javascript:` and `data:` included.
fn safe_url(dest: CowStr<'_>) -> CowStr<'_> {
    let scheme = dest.split(':').next().unwrap_or_default();
    let relative = !dest.contains(':') || scheme.contains('/');
    if relative || ["http", "https", "mailto"].contains(&scheme) {
        dest
    } else {
        "".into()
    }
}

/// Raw HTML is escaped rather than executed, and links and images are limited
/// to relative destinations and the http/https/mailto schemes, so an agent's
/// output cannot script the page that displays it.
pub fn to_html(md: &str) -> String {
    let events = Parser::new_ext(md, options()).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        // An image's src runs through the same gate: `<img src=x onerror=…>`
        // arrives as raw HTML and is escaped, but `![a](javascript:…)` is a
        // parsed image and would otherwise pass straight through.
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        other => other,
    });

    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// A user's own words, as HTML that means exactly what they typed. Both
/// speakers reach the phone through the same `html` field, so the user's half
/// is escaped here rather than left as a second, differently-handled shape.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
    out
}

/// The card's three-line preview is plain text in one node, which is what lets
/// CSS clamp it cleanly; markup would give the clamp block children to trip on.
/// Block ends and soft breaks become single spaces, so a heading does not run
/// into the paragraph beneath it.
pub fn preview(md: &str, cap: usize) -> String {
    let mut out = String::new();
    for event in Parser::new_ext(md, options()) {
        match event {
            // Raw HTML is text here as it is in `to_html`: prose that mentions
            // `<Foo>` must not lose it from the card it is previewed on.
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text) => out.push_str(&text),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                out.push(' ');
            }
            _ => {}
        }
        if out.chars().count() >= cap {
            break;
        }
    }
    // Inline ends (emphasis, code spans) push nothing, so runs of whitespace
    // only come from the source itself; collapse them so the clamp counts real
    // lines rather than blank ones.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
        .chars()
        .take(cap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_survives() {
        let html = to_html("| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n```rust\nlet x = 1;\n```");
        assert!(html.contains("<table>"));
        assert!(html.contains(r#"type="checkbox""#));
        assert!(html.contains(r#"class="language-rust""#));
    }

    #[test]
    fn raw_html_is_escaped_not_executed() {
        let html = to_html("型は <Foo> と書く。\n<script>alert(1)</script>");
        assert!(html.contains("&lt;Foo&gt;"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn a_javascript_link_loses_its_destination() {
        let html = to_html("[click](javascript:alert(1)) [ok](https://herdr.dev) [rel](./a.md)");
        assert!(html.contains(r#"<a href="">click</a>"#));
        assert!(html.contains(r#"href="https://herdr.dev""#));
        assert!(html.contains(r#"href="./a.md""#));
    }

    #[test]
    fn an_image_source_runs_through_the_same_gate() {
        let html = to_html("![a](javascript:alert(1)) ![b](https://x/y.png)");
        assert!(html.contains(r#"<img src="" alt="a" />"#));
        assert!(html.contains(r#"<img src="https://x/y.png" alt="b" />"#));
        assert!(to_html("![c](data:image/svg+xml;base64,AAA)").contains(r#"src="""#));
    }

    #[test]
    fn an_event_handler_attribute_is_escaped_with_its_tag() {
        let html = to_html("<img src=x onerror=alert(1)>");
        assert_eq!(html, "&lt;img src=x onerror=alert(1)&gt;");
    }

    #[test]
    fn preview_is_plain_text_within_its_cap() {
        let text = preview("## 原因1\n\n`read()` は **recent** を渡す。", 300);
        assert_eq!(text, "原因1 read() は recent を渡す。");
    }

    #[test]
    fn a_users_own_words_become_html_that_means_what_they_typed() {
        assert_eq!(
            escape("<script>alert('x' & \"y\")</script>"),
            "&lt;script&gt;alert(&#39;x&#39; &amp; &quot;y&quot;)&lt;/script&gt;"
        );
    }

    /// The card previews what the modal shows. Prose about `<Foo>` keeps it in
    /// both places.
    #[test]
    fn preview_keeps_literal_html_shaped_prose() {
        assert_eq!(preview("型は <Foo> と書く。", 300), "型は <Foo> と書く。");
    }

    #[test]
    fn preview_stops_at_the_cap() {
        assert_eq!(preview(&"あ".repeat(500), 10).chars().count(), 10);
    }
}
```

- [ ] **Step 2: Register both modules**

`src/lib.rs` becomes:

```rust
pub mod live;
pub mod markdown;
pub mod transcript;
```

- [ ] **Step 3: Run the markdown tests**

Run: `cargo test markdown`
Expected: 9 tests PASS. This module's code and every one of these tests was compiled and
run against pulldown-cmark 0.13.4 while the plan was written; the expected strings are
measured output, not guesses.

- [ ] **Step 4: Write the composer module and its tests**

`src/live.rs`:

```rust
//! The one line a transcript cannot hold: what the person has typed into the
//! agent's box but not yet sent.

/// What an empty box renders instead of nothing. Matching these is the only
/// way to tell "the person typed nothing" from "the person typed this".
const PLACEHOLDERS: [&str; 3] = [
    "Ask Codex to do",
    "Plan, search,",
    "Add a follow-up",
];

/// The draft's first line, or `None` when the box is empty, shows a
/// placeholder, or no longer looks the way this agent's box looked. A wrong
/// string labelled as the user's draft is worse than a blank.
pub fn composer(agent: &str, screen: &str) -> Option<String> {
    let rows: Vec<&str> = screen.lines().collect();
    let line = match agent {
        // The same glyph prefixes messages already sent, echoed further up the
        // screen; the composer is always the last one. Claude draws rules around
        // its box and grok draws none, and neither fact matters here.
        "claude" | "grok" => last_starting_with(&rows, '❯')?,
        "codex" => last_starting_with(&rows, '›')?,
        // `→` shows up in ordinary output, so cursor's box is found by its
        // block borders rather than by the glyph.
        "cursor" => between_borders(&rows)?,
        _ => return None,
    };

    let draft = line
        .trim_start()
        .trim_start_matches(['❯', '›', '→'])
        .trim();
    let placeholder = PLACEHOLDERS
        .iter()
        .any(|candidate| draft.starts_with(candidate));
    (!draft.is_empty() && !placeholder).then(|| draft.to_string())
}

fn last_starting_with<'a>(rows: &[&'a str], glyph: char) -> Option<&'a str> {
    rows.iter()
        .rev()
        .find(|row| row.trim_start().starts_with(glyph))
        .copied()
}

/// A border row is three or more of one drawing character and nothing else.
fn border(row: &str, glyph: char) -> bool {
    let trimmed = row.trim();
    trimmed.chars().count() >= 3 && trimmed.chars().all(|c| c == glyph)
}

/// The first non-empty row inside the last box, whose top is `▄` and whose
/// bottom is `▀`. Requiring the orientation keeps a lone border, or two of the
/// same kind, from being read as a box.
fn between_borders<'a>(rows: &[&'a str]) -> Option<&'a str> {
    let bottom = rows.iter().rposition(|row| border(row, '▀'))?;
    let top = rows[..bottom].iter().rposition(|row| border(row, '▄'))?;
    rows[top + 1..bottom]
        .iter()
        .find(|row| !row.trim().is_empty())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = "\
❯ このメッセージは送信済みのエコー
  ⎿  tool output

────────────
❯ 打ちかけの下書き
────────────
  Opus 5 (1M context) xhigh";

    #[test]
    fn claude_takes_the_last_prompt_row_not_the_echoed_message() {
        assert_eq!(composer("claude", CLAUDE).unwrap(), "打ちかけの下書き");
    }

    #[test]
    fn an_empty_claude_box_is_blank() {
        let screen = "────────────\n❯\n────────────\n  Opus 5";
        assert!(composer("claude", screen).is_none());
    }

    /// Grok draws no rules at all, which is why nothing looks for them.
    #[test]
    fn grok_has_no_box_around_its_prompt_row() {
        let screen = "◆ session_start\nminimal · /help\n❯ 下書き\nGrok 4.6 (xhigh) · a";
        assert_eq!(composer("grok", screen).unwrap(), "下書き");
        assert!(composer("grok", "minimal · /help\n❯\nGrok 4.6").is_none());
    }

    #[test]
    fn only_the_first_row_of_a_wrapped_draft_is_taken() {
        let screen = "❯ DRAFT_PROBE_XY 下\n  書き\nGrok 4.6";
        assert_eq!(composer("grok", screen).unwrap(), "DRAFT_PROBE_XY 下");
    }

    #[test]
    fn codex_reads_its_prompt_row_and_ignores_the_placeholder() {
        assert_eq!(composer("codex", "› review the diff").unwrap(), "review the diff");
        assert!(composer("codex", "› Ask Codex to do an").is_none());
    }

    #[test]
    fn cursor_reads_between_its_block_borders_and_knows_both_placeholders() {
        let screen = " ▄▄▄▄▄▄▄▄\n  → 続きを頼む\n ▀▀▀▀▀▀▀▀\n  Cursor Grok 4.6";
        assert_eq!(composer("cursor", screen).unwrap(), "続きを頼む");
        for empty in [
            " ▄▄▄▄▄▄▄▄\n  → Add a follow-up\n ▀▀▀▀▀▀▀▀",
            " ▄▄▄▄▄▄▄▄\n  → Plan, search,\n    build anything\n ▀▀▀▀▀▀▀▀",
        ] {
            assert!(composer("cursor", empty).is_none(), "{empty}");
        }
    }

    #[test]
    fn a_bare_prompt_glyph_is_an_empty_box_for_every_agent() {
        assert!(composer("codex", "  gpt-5.6\n›\n  status").is_none());
        assert!(composer("cursor", " ▄▄▄▄\n  →\n ▀▀▀▀").is_none());
        assert!(composer("grok", "❯").is_none());
    }

    #[test]
    fn a_lone_border_is_not_a_box() {
        assert!(composer("cursor", " ▀▀▀▀\n  → draft").is_none());
    }

    #[test]
    fn an_unknown_agent_and_a_broken_screen_both_blank() {
        assert!(composer("devin", "❯ something").is_none());
        assert!(composer("claude", "no prompt row here at all").is_none());
    }
}
```

- [ ] **Step 5: Run the composer tests**

Run: `cargo test live`
Expected: 9 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/markdown.rs src/live.rs src/lib.rs
git commit -m "feat: render markdown and read the composer line"
```

---

## Task 10: Transcript merge and card presentation

**Track:** `web`

**Files:**
- Create: `web/src/lib/transcript.ts`
- Create: `web/src/lib/transcript.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `type Card = { seq: number; role: "user" | "assistant"; preview: string; html: string }`,
  `type Page = { messages: Card[]; has_more: boolean }`,
  `append(existing, incoming): Card[]`, `prepend(older, existing): Card[]`,
  `modalContent(card): { wrap: boolean; html: string }`,
  `following(scrollTop, clientHeight, scrollHeight): boolean`.

The route contract these mirror: the transcript route returns `{ messages, has_more }`
oldest first, and `before=<seq>` asks for messages whose `seq` is strictly smaller.

`append` returns **the array it was given** when nothing changed. The caller repaints on
identity, not on length: a rewritten card leaves the length alone, and a poll that changed
nothing must not rebuild the list under the reader's finger.

The DOM-shaped decisions live here as pure functions — which lane a card belongs to, what
the modal shows, whether the list is at its tail — because the test environment is plain
node with no DOM. Everything testable is testable; the wiring that remains in the page is
what the browser step covers.

- [ ] **Step 1: Write the failing tests**

`web/src/lib/transcript.test.ts`:

```typescript
import { expect, test } from "vitest";
import {
	append,
	type Card,
	following,
	modalContent,
	prepend,
} from "./transcript";

const card = (seq: number, role: Card["role"] = "assistant"): Card => ({
	seq,
	role,
	preview: `p${seq}`,
	html: `<p>${seq}</p>`,
});

test("append adds only messages the list does not already carry", () => {
	const merged = append([card(1), card(2)], [card(2), card(3)]);
	expect(merged.map((c) => c.seq)).toEqual([1, 2, 3]);
});

test("append returns the same array when the poll changed nothing", () => {
	const existing = [card(1), card(2)];
	expect(append(existing, [card(1), card(2)])).toBe(existing);
	expect(append(existing, [])).toBe(existing);
});

test("append keeps the newer copy of a message that was rewritten", () => {
	const existing = [card(1), card(2)];
	const rewritten = { ...card(2), preview: "edited" };
	const merged = append(existing, [rewritten]);
	expect(merged).not.toBe(existing);
	expect(merged[1].preview).toBe("edited");
});

test("prepend puts older messages in front, without duplicating the overlap", () => {
	expect(
		prepend([card(1), card(2)], [card(2), card(3)]).map((c) => c.seq),
	).toEqual([1, 2, 3]);
	const existing = [card(2)];
	expect(prepend([], existing)).toBe(existing);
});

test("the modal wraps a user's text and renders an agent's markdown", () => {
	expect(modalContent(card(1, "user"))).toEqual({
		wrap: true,
		html: "<p>1</p>",
	});
	expect(modalContent(card(1, "assistant"))).toEqual({
		wrap: false,
		html: "<p>1</p>",
	});
});

test("the tail is followed only from the tail", () => {
	expect(following(920, 80, 1000)).toBe(true);
	expect(following(0, 80, 1000)).toBe(false);
	// Eight pixels of slop, matching the existing raw-output view.
	expect(following(915, 80, 1000)).toBe(true);
	expect(following(500, 80, 1000)).toBe(false);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && aube exec vitest run src/lib/transcript.test.ts`
Expected: FAIL — cannot resolve `./transcript`.

- [ ] **Step 3: Write the implementation**

`web/src/lib/transcript.ts`:

```typescript
export type Card = {
	seq: number;
	role: "user" | "assistant";
	preview: string;
	html: string;
};

export type Page = { messages: Card[]; has_more: boolean };

const same = (a: Card, b: Card): boolean =>
	a.role === b.role && a.preview === b.preview && a.html === b.html;

/** Merge by `seq`, newest copy winning, oldest first. Polls overlap by design —
 *  the server answers with a window, not a delta — so the common case is a page
 *  that changes nothing, and that case returns the original array. */
function merge(existing: Card[], incoming: Card[]): Card[] {
	if (incoming.length === 0) return existing;
	const bySeq = new Map(existing.map((card) => [card.seq, card]));
	let changed = false;
	for (const card of incoming) {
		const seen = bySeq.get(card.seq);
		if (seen && same(seen, card)) continue;
		bySeq.set(card.seq, card);
		changed = true;
	}
	if (!changed) return existing;
	return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

export const append = (existing: Card[], incoming: Card[]): Card[] =>
	merge(existing, incoming);

export const prepend = (older: Card[], existing: Card[]): Card[] =>
	merge(existing, older);

/** The agent's half is markdown the server rendered; the user's half is text
 *  the server escaped. Both arrive as HTML, and only the user's needs the
 *  whitespace of what they actually typed. */
export const modalContent = (card: Card): { wrap: boolean; html: string } => ({
	wrap: card.role === "user",
	html: card.html,
});

/** Follow the tail only when the reader is already at it. The eight pixels of
 *  slop are the same the raw-output view uses. */
export const following = (
	scrollTop: number,
	clientHeight: number,
	scrollHeight: number,
): boolean => scrollTop + clientHeight >= scrollHeight - 8;
```

- [ ] **Step 4: Run the tests**

Run: `cd web && aube exec vitest run src/lib/transcript.test.ts`
Expected: 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/transcript.ts web/src/lib/transcript.test.ts
git commit -m "feat: merge transcript pages and decide card presentation"
```

---

## Task 11: Transcript, live, and zoom fetchers

**Track:** `web`

**Files:**
- Modify: `web/src/lib/api.ts`
- Modify: `web/src/lib/api.test.ts`

**Interfaces:**
- Consumes: `Card`, `Page` from `./transcript`.
- Produces: `fetchTranscript(paneId, before?, signal?) → Page | "unchanged" | null`,
  `fetchLive(paneId, signal?) → Live`, `openPane(paneId) → Promise<void>`,
  `closePane(paneId) → void`, `forgetPane(paneId) → void`, `type Live = { screen: string; composer: string }`.

Three results, because the route has three answers. A `Page` is new content. `"unchanged"`
is the 304 that makes a 44 MB transcript free to poll — `fetch` treats 304 as `!ok`, so it
has to be handled before the error path or every quiet poll reads as a failure. `null` is
the 404 that means this pane has no transcript, which is the signal to use raw output
rather than an error to show.

The ETag is stored per pane and sent **only** for the newest window. Sending it on a
`before=` request would let the server answer 304 for a page the phone has never held.

- [ ] **Step 1: Write the failing tests**

Add to the import block at the top of `web/src/lib/api.test.ts`: `fetchLive`,
`fetchTranscript`, `forgetPane`. Then append:

```typescript
/** A fetch double with the real signature, so `mock.calls[0]` is a real tuple. */
const stubFetch = (handler: (url: string, init?: RequestInit) => Response) =>
	vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
		handler(String(input), init),
	);

const page = (body: unknown, headers: Record<string, string> = {}) =>
	new Response(JSON.stringify(body), {
		status: 200,
		headers: { "content-type": "application/json", ...headers },
	});

test("a 404 from the transcript route means no transcript, not an error", async () => {
	vi.stubGlobal(
		"fetch",
		stubFetch(() => new Response("no transcript", { status: 404 })),
	);
	try {
		await expect(fetchTranscript("w1:p1")).resolves.toBeNull();
	} finally {
		vi.unstubAllGlobals();
	}
});

test("the newest window carries its stored etag and a 304 reads as unchanged", async () => {
	forgetPane("w1:p1");
	const seen: Array<Record<string, string>> = [];
	const fetchMock = stubFetch((_url, init) => {
		seen.push({ ...((init?.headers ?? {}) as Record<string, string>) });
		return seen.length === 1
			? page({ messages: [], has_more: false }, { etag: '"7-tail-30"' })
			: new Response(null, { status: 304 });
	});
	vi.stubGlobal("fetch", fetchMock);
	try {
		await expect(fetchTranscript("w1:p1")).resolves.toEqual({
			messages: [],
			has_more: false,
		});
		await expect(fetchTranscript("w1:p1")).resolves.toBe("unchanged");
	} finally {
		vi.unstubAllGlobals();
	}
	expect(seen[0]["if-none-match"]).toBeUndefined();
	expect(seen[1]["if-none-match"]).toBe('"7-tail-30"');
});

test("reaching further back asks for a window and never sends the etag", async () => {
	forgetPane("w1:p1");
	const fetchMock = stubFetch((_url, init) => {
		expect(
			(init?.headers as Record<string, string> | undefined)?.["if-none-match"],
		).toBeUndefined();
		return page({ messages: [], has_more: true });
	});
	vi.stubGlobal("fetch", fetchMock);
	try {
		await fetchTranscript("w1:p1", 42);
	} finally {
		vi.unstubAllGlobals();
	}
	expect(fetchMock.mock.calls[0][0]).toBe(
		"/api/panes/w1%3Ap1/transcript?limit=30&before=42",
	);
});

test("a transcript page that is not shaped like one is refused", async () => {
	forgetPane("w1:p1");
	vi.stubGlobal(
		"fetch",
		stubFetch(() => page({ messages: [{ seq: "one" }] })),
	);
	try {
		await expect(fetchTranscript("w1:p1")).rejects.toThrow(
			/unexpected response/,
		);
	} finally {
		vi.unstubAllGlobals();
	}
});

test("live returns the screen and the composer line", async () => {
	vi.stubGlobal(
		"fetch",
		stubFetch(() => page({ screen: "❯ draft", composer: "draft" })),
	);
	try {
		await expect(fetchLive("w1:p1")).resolves.toEqual({
			screen: "❯ draft",
			composer: "draft",
		});
	} finally {
		vi.unstubAllGlobals();
	}
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && aube exec vitest run src/lib/api.test.ts`
Expected: FAIL — `fetchTranscript` is not exported.

- [ ] **Step 3: Add the import**

At the top of `web/src/lib/api.ts`, with the file's other imports:

```typescript
import type { Card, Page } from "./transcript";
```

ESM requires it there; appending an `import` after the file's statements is a syntax
error, and `astro check` refuses the file.

- [ ] **Step 4: Append the implementation**

```typescript
export type Live = { screen: string; composer: string };

/** The newest-window ETag per pane. Cleared when a pane stops having a
 *  transcript, so a later attach starts from a real request. */
const etags = new Map<string, string>();

export function forgetPane(paneId: string): void {
	etags.delete(paneId);
}

function isCard(value: unknown): value is Card {
	const card = value as Card;
	return (
		typeof value === "object" &&
		value !== null &&
		typeof card.seq === "number" &&
		(card.role === "user" || card.role === "assistant") &&
		typeof card.preview === "string" &&
		typeof card.html === "string"
	);
}

function isPage(value: unknown): value is Page {
	const body = value as Page;
	return (
		typeof value === "object" &&
		value !== null &&
		Array.isArray(body.messages) &&
		body.messages.every(isCard) &&
		typeof body.has_more === "boolean"
	);
}

/** `null` means the pane has no transcript — a shell, or an agent whose session
 *  file could not be resolved — and the caller falls back to raw output.
 *  `"unchanged"` is the 304 that keeps a quiet poll free. */
export async function fetchTranscript(
	paneId: string,
	before?: number,
	signal?: AbortSignal,
): Promise<Page | "unchanged" | null> {
	const newest = before === undefined;
	const url = `${paneUrl(paneId, "transcript")}?limit=30${newest ? "" : `&before=${before}`}`;
	const etag = newest ? etags.get(paneId) : undefined;
	const response = await fetch(url, {
		signal,
		headers: etag ? { "if-none-match": etag } : undefined,
	});
	if (response.status === 404) {
		etags.delete(paneId);
		return null;
	}
	if (response.status === 304) return "unchanged";
	if (!response.ok) {
		throw new Error(await reason(response, "could not load the transcript"));
	}
	const tag = response.headers.get("etag");
	if (newest && tag) etags.set(paneId, tag);
	const body: unknown = await response.json();
	if (!isPage(body)) {
		throw new Error("unexpected response from the transcript route");
	}
	return body;
}

function isLive(value: unknown): value is Live {
	const live = value as Live;
	return (
		typeof value === "object" &&
		value !== null &&
		typeof live.screen === "string" &&
		typeof live.composer === "string"
	);
}

/** The terminal's own screen, plus the first line of whatever is typed into the
 *  agent's box. Both are width-dependent; the transcript is not. */
export async function fetchLive(
	paneId: string,
	signal?: AbortSignal,
): Promise<Live> {
	const response = await fetch(paneUrl(paneId, "live"), { signal });
	if (!response.ok) {
		throw new Error(await reason(response, "could not read the pane"));
	}
	const body: unknown = await response.json();
	if (!isLive(body)) {
		throw new Error("unexpected response from the live route");
	}
	return body;
}

/** Zooming the pane is what makes its screen legible on a phone: a picker
 *  rendered into twenty columns is destroyed before it is ever read. */
export const openPane = (paneId: string): Promise<void> =>
	post(paneUrl(paneId, "open"), "could not open the pane");

/** `keepalive`, because this also fires from `pagehide`, when ordinary requests
 *  are cancelled along with the page. A close that is lost anyway is repaired
 *  by the server's next `open`, so nothing here needs to be awaited. */
export function closePane(paneId: string): void {
	void fetch(paneUrl(paneId, "close"), {
		method: "POST",
		keepalive: true,
	}).catch(() => {});
}

export type { Card, Page };
```

- [ ] **Step 5: Run the tests**

Run: `cd web && aube exec vitest run src/lib/api.test.ts`
Expected: every test PASSES, including the five new ones.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/api.ts web/src/lib/api.test.ts
git commit -m "feat: fetch transcripts conditionally, plus live and zoom"
```

---

## Task 12: Cards and the modal

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro`

**Interfaces:**
- Consumes: `fetchTranscript`, `append`, `prepend`, `modalContent`, `following`, `Card`.
- Produces: `#transcript`, `#full`, and `paintCards()` for Task 14.

- [ ] **Step 1: Insert the markup**

**Insert immediately before the existing `<pre id="log" hidden></pre>`. Do not remove or
rename `#log`** — it is still the raw-output view for shell panes and for agent panes whose
transcript does not resolve. The block becomes:

```html
<ol id="transcript" hidden></ol>
<dialog id="full">
  <div class="body"></div>
  <form method="dialog"><button>Close</button></form>
</dialog>
<pre id="log" hidden></pre>
```

- [ ] **Step 2: Extend the script's imports**

The existing `import { … } from "../lib/api";` list gains:

```typescript
	closePane,
	fetchLive,
	fetchTranscript,
	forgetPane,
	type Live,
	openPane,
```

and a second import joins it:

```typescript
import {
	append,
	type Card,
	following,
	modalContent,
	prepend,
} from "../lib/transcript";
```

- [ ] **Step 3: Add the styles**

```css
#transcript {
  flex: 1;
  list-style: none;
  margin: 0;
  padding: 0 0 0.5rem;
  overflow-y: auto;
  overscroll-behavior: contain;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}
/* Older WebViews do not mark the UA `[hidden]` rule `!important`, so a
   `display` of our own would win over it — the same reason `#log[hidden]`
   already exists below. */
#transcript[hidden],
#live[hidden] {
  display: none;
}
#transcript li {
  border-radius: 10px;
  padding: 0.5rem 0.7rem;
  cursor: pointer;
}
#transcript li[data-role="user"] {
  margin-left: 15%;
  background: var(--accent-soft, #26323d);
}
#transcript li[data-role="assistant"] {
  background: var(--panel, #1c1c1c);
}
/* Plain text in one node, which is what lets the clamp cut at three lines and
   append its ellipsis; markup would give it block children to trip on. */
.preview {
  margin: 0;
  line-height: 1.6;
  display: -webkit-box;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 3;
  line-clamp: 3;
  overflow: hidden;
}
#full {
  max-width: min(46rem, 92vw);
  max-height: 82vh;
  overflow: auto;
  border: none;
  border-radius: 12px;
}
#full .body.wrap {
  white-space: pre-wrap;
  font-family: ui-monospace, monospace;
}
#full .body pre,
#full .body table {
  display: block;
  overflow-x: auto;
}
```

- [ ] **Step 4: Render the cards**

```typescript
const transcriptEl = document.querySelector<HTMLOListElement>("#transcript")!;
const fullEl = document.querySelector<HTMLDialogElement>("#full")!;
const fullBodyEl = fullEl.querySelector<HTMLDivElement>(".body")!;

let cards: Card[] = [];
let paintedCards: Card[] = [];

function cardEl(card: Card) {
  const item = document.createElement("li");
  item.dataset.role = card.role;
  item.dataset.seq = String(card.seq);
  const preview = document.createElement("p");
  preview.className = "preview";
  // textContent: a preview is the speaker's words, not our markup.
  preview.textContent = card.preview;
  item.append(preview);
  return item;
}

/** Rebuild only when the merge produced a different array. Thirty nodes is
 *  cheap; yanking the reader's scroll position every three seconds is not. */
function paintCards() {
  if (cards === paintedCards) return;
  const follow = following(
    transcriptEl.scrollTop,
    transcriptEl.clientHeight,
    transcriptEl.scrollHeight,
  );
  const top = transcriptEl.scrollTop;
  transcriptEl.replaceChildren(...cards.map(cardEl));
  paintedCards = cards;
  transcriptEl.scrollTop = follow ? transcriptEl.scrollHeight : top;
}

// The agent's HTML was rendered by the server, which escapes raw HTML and
// strips dangerous link and image schemes; the user's own text arrives escaped
// by the same server, so both are safe to assign and neither is re-escaped.
function openCard(card: Card) {
  const { wrap, html } = modalContent(card);
  fullBodyEl.classList.toggle("wrap", wrap);
  fullBodyEl.innerHTML = html;
  fullEl.showModal();
}

transcriptEl.addEventListener("click", (event) => {
  const item = (event.target as Element | null)?.closest("li");
  const seq = Number(item?.dataset.seq);
  const card = cards.find((candidate) => candidate.seq === seq);
  if (card) openCard(card);
});

// The dialog element itself is the backdrop, so a tap outside the panel closes.
fullEl.addEventListener("click", (event) => {
  if (event.target === fullEl) fullEl.close();
});
```

- [ ] **Step 5: Typecheck**

Run: `cd web && aube run check`
Expected: PASS. `astro check` and Biome both cover the page script, and neither tolerates
an unused binding or an implicit `any`.

- [ ] **Step 6: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "feat: show transcript cards with a full-text modal"
```

---

## Task 13: The live band

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro`

**Interfaces:**
- Consumes: `fetchLive`, `Live`.
- Produces: `#live`, `paintLive(live, state)`, `resetLive()` for Task 14.

- [ ] **Step 1: Add the markup**

Directly below the `</dialog>` from Task 12 and above `#log`:

```html
<div id="live" hidden>
  <span id="draft"></span>
  <span id="beat" hidden>
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
         stroke-linecap="round" stroke-linejoin="round" aria-label="working"><g>
      <path d="m14 13-8.381 8.38a1 1 0 0 1-3.001-3l8.384-8.381"/>
      <path d="m16 16 6-6"/><path d="m21.5 10.5-8-8"/>
      <path d="m8 8 6-6"/><path d="m8.5 7.5 8 8"/>
    </g></svg>
  </span>
  <button id="expand" aria-label="Show the pane's screen">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
         stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M15 3h6v6"/><path d="m21 3-7 7"/>
      <path d="m3 21 7-7"/><path d="M9 21H3v-6"/>
    </svg>
  </button>
</div>
<dialog id="screen"><pre></pre><form method="dialog"><button>Close</button></form></dialog>
```

- [ ] **Step 2: Add the styles**

```css
#live {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.35rem 0.6rem;
  border-top: 1px solid var(--line, #333);
}
/* One line, ellipsised — the treatment .name and .subtitle already use. */
#draft {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: ui-monospace, monospace;
  opacity: 0.75;
}
#beat svg,
#expand svg {
  width: 22px;
  height: 22px;
  overflow: visible; /* the head leaves the 24-unit box when it swings */
}
#beat g {
  transform-origin: 17% 89%; /* the handle's tip */
  animation: strike 1.1s ease-in-out infinite;
}
@keyframes strike {
  0%, 45% { transform: rotate(-32deg); }
  62%     { transform: rotate(6deg); }
  70%     { transform: rotate(-6deg); }
  78%     { transform: rotate(2deg); }
  100%    { transform: rotate(-32deg); }
}
@media (prefers-reduced-motion: reduce) {
  #beat g { animation: none; }
}
#screen pre {
  margin: 0;
  overflow: auto;
  max-height: 80vh;
  font-size: 12px;
  line-height: 1.35;
}
```

- [ ] **Step 3: Wire the band**

```typescript
const liveEl = document.querySelector<HTMLDivElement>("#live")!;
const draftEl = document.querySelector<HTMLSpanElement>("#draft")!;
const beatEl = document.querySelector<HTMLSpanElement>("#beat")!;
const expandEl = document.querySelector<HTMLButtonElement>("#expand")!;
const screenEl = document.querySelector<HTMLDialogElement>("#screen")!;
const screenBodyEl = screenEl.querySelector<HTMLPreElement>("pre")!;

let screenText = "";

function paintLive(live: Live, state: string) {
  draftEl.textContent = live.composer;
  beatEl.hidden = state !== "working";
  screenText = live.screen;
  if (screenEl.open && screenBodyEl.textContent !== live.screen) {
    screenBodyEl.textContent = live.screen;
    screenBodyEl.scrollTop = screenBodyEl.scrollHeight;
  }
}

/** Leaving a pane must not leave its draft, its screen, or its animation behind
 *  for the next one to show for a tick. */
function resetLive() {
  draftEl.textContent = "";
  beatEl.hidden = true;
  screenText = "";
  screenBodyEl.textContent = "";
  if (screenEl.open) screenEl.close();
}

// The screen is where a /model picker lives: it never reaches the transcript,
// and the pane's status stays `idle` while one is open, so the button is
// offered in every state rather than gated on `blocked`.
expandEl.addEventListener("click", () => {
  screenBodyEl.textContent = screenText;
  screenEl.showModal();
  screenBodyEl.scrollTop = screenBodyEl.scrollHeight;
});

screenEl.addEventListener("click", (event) => {
  if (event.target === screenEl) screenEl.close();
});
```

- [ ] **Step 4: Typecheck**

Run: `cd web && aube run check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "feat: add the live band with its screen dialog"
```

---

## Task 14: View switching and polling

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro`

**Interfaces:**
- Consumes: everything from Tasks 10-13.
- Produces: the finished pane view.

Four things the existing loop gets right and this must keep: it polls nothing while the
tab is hidden, it times each request out at fifteen seconds, it reads `current().pane`
rather than a captured object, and it never lets a late response from a pane the reader
has left paint over the pane they are on.

- [ ] **Step 1: Replace `watch()` with a single-flight loop**

```typescript
let watching: string | null = null;
let watches = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

function watch(pane: Pane | null) {
  clearTimeout(timer);
  if (watching && watching !== pane?.id) closePane(watching);
  watching = pane?.id ?? null;
  cards = [];
  paintedCards = [];
  painted = "";
  lines = LINES;
  transcriptEl.replaceChildren();
  transcriptEl.hidden = true;
  logEl.hidden = true;
  liveEl.hidden = true;
  moreEl.hidden = true;
  resetLive();
  if (!pane) return;
  if (pane.id !== watching) return;

  forgetPane(pane.id);
  liveEl.hidden = !pane.agent;
  void openPane(pane.id).catch(() => {});

  const token = ++watches;
  const loop = async () => {
    if (!document.hidden) {
      try {
        await tick(token);
        if (statusEl.classList.contains("error")) say("");
      } catch (error) {
        complain(error, "could not read the pane");
      }
    }
    if (token === watches) timer = setTimeout(loop, POLL_MS);
  };
  void loop();
}

async function tick(token: number) {
  // The session poll replaces the pane object as its state and agent change,
  // and `render()` does not restart a watch whose id is unchanged.
  const pane = current().pane;
  if (!pane || token !== watches) return;
  const signal = AbortSignal.timeout(15000);

  if (pane.agent) {
    const live = await fetchLive(pane.id, signal);
    if (token !== watches) return;
    paintLive(live, pane.state);
  }
  liveEl.hidden = !pane.agent;

  // A shell has no transcript by definition; asking anyway would spend a
  // request per tick to be told so.
  const page = pane.agent ? await fetchTranscript(pane.id, undefined, signal) : null;
  if (token !== watches) return;

  if (page !== null) {
    transcriptEl.hidden = false;
    logEl.hidden = true;
    if (page !== "unchanged") {
      cards = append(cards, page.messages);
      paintCards();
      moreEl.hidden = !page.has_more;
    }
    return;
  }

  // Raw output: an agent pane keeps the visible screen, whose redraw history
  // does not churn, and a shell keeps its scrollback.
  transcriptEl.hidden = true;
  logEl.hidden = false;
  const output = await fetchOutput(
    pane.id,
    lines,
    pane.agent ? "screen" : "scrollback",
    signal,
  );
  if (token !== watches) return;
  paint(output.text);
  moreEl.hidden = !output.truncated;
}

addEventListener("pagehide", () => {
  if (watching) closePane(watching);
});
```

- [ ] **Step 2: Point "earlier" at whichever view is showing**

**Replace** the existing `moreEl` click listener; do not add a second one. The raw-output
branch keeps its `* 4` widening, and neither branch writes a sentence into the status line.

```typescript
moreEl.addEventListener("click", async () => {
  if (!watching) return;
  if (transcriptEl.hidden) {
    lines = Math.min(lines * 4, MAX_LINES);
    moreEl.hidden = true;
    return;
  }
  const oldest = cards[0]?.seq;
  const page = await fetchTranscript(watching, oldest);
  if (page === null || page === "unchanged") return;
  const fromBottom = transcriptEl.scrollHeight - transcriptEl.scrollTop;
  cards = prepend(page.messages, cards);
  paintCards();
  transcriptEl.scrollTop = transcriptEl.scrollHeight - fromBottom;
  moreEl.hidden = !page.has_more;
});
```

- [ ] **Step 3: Stop `render()` from owning the surfaces**

`render()` currently sets `logEl.hidden = pane === null;` on every session update, which
would unhide the raw log underneath a transcript every time the session JSON changes.
Replace that line with `if (!pane) { logEl.hidden = true; transcriptEl.hidden = true; }` —
choosing which surface is visible belongs to `tick`, which is the only code that knows
whether this pane resolved a transcript.

- [ ] **Step 4: Run the whole web suite and the checks**

Run: `cd web && aube run test && aube run check`
Expected: PASS.

- [ ] **Step 5: Verify in the browser**

Run: `make run`. Open a claude pane and a codex pane (transcript view), a cursor and a grok
pane, and a shell pane (raw output). Confirm "earlier" pages the transcript, that leaving
a pane unzooms it on the desktop, and that the log does not flash under the transcript when
a pane changes state.

- [ ] **Step 6: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "feat: switch between transcript and raw output per pane"
```

---

## Task 15: herdr client extensions

**Track:** `api`

**Files:**
- Modify: `src/herdr.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub struct AgentSession { pub source: String, pub agent: String, pub kind: String, pub value: String }`,
  `pub struct PaneContext { pub agent: Option<String>, pub session: Option<AgentSession>, pub cwd: String, pub title: Option<String> }`,
  `pub async fn pane_context(pane_id: &str) -> Result<Option<PaneContext>>`,
  `pub async fn zoom(pane_id: &str, on: bool) -> Result<()>`.

`session.snapshot` already returns `agent_session` and `cwd` for every pane; the current
`PaneInfo` discards both. All four session fields are kept: `source` and `agent` are what
make it possible to notice later that a session was reported by something other than the
agent the pane is running.

- [ ] **Step 1: Extend the wire types**

```rust
/// What an agent reported about its own session. Self-reported, hence the
/// boundary in `transcript::guard`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AgentSession {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}
```

Add to `PaneInfo`:

```rust
    agent_session: Option<AgentSession>,
    cwd: Option<String>,
```

- [ ] **Step 2: Add the accessor and the zoom call**

```rust
/// What the transcript layer needs to find this pane's session file. `title`
/// prefers the stripped terminal title, which is what cursor's own chat titles
/// are matched against.
pub struct PaneContext {
    pub agent: Option<String>,
    pub session: Option<AgentSession>,
    pub cwd: String,
    pub title: Option<String>,
}

pub async fn pane_context(pane_id: &str) -> Result<Option<PaneContext>> {
    Ok(snapshot()
        .await?
        .panes
        .into_iter()
        .find(|pane| pane.pane_id == pane_id)
        .map(|pane| PaneContext {
            agent: pane.agent.clone(),
            session: pane.agent_session.clone(),
            cwd: pane.cwd.clone().unwrap_or_default(),
            title: pane
                .terminal_title_stripped
                .clone()
                .or_else(|| pane.title.clone()),
        }))
}

/// A pane rendered into twenty columns destroys anything it draws; zooming is
/// what makes a picker readable from the phone.
pub async fn zoom(pane_id: &str, on: bool) -> Result<()> {
    call("pane.zoom", json!({ "pane_id": pane_id, "zoomed": on })).await?;
    Ok(())
}
```

- [ ] **Step 3: Test the whole mapping**

```rust
    #[test]
    fn a_pane_carries_its_session_and_cwd() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "workspaces": [], "tabs": [],
            "panes": [{
                "pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "claude",
                "agent_status": "working", "cwd": "/repo",
                "title": "raw", "terminal_title_stripped": "stripped",
                "agent_session": { "source": "herdr:claude", "agent": "claude",
                                   "kind": "id", "value": "abc" }
            }]
        }))
        .unwrap();
        let pane = &snapshot.panes[0];
        assert_eq!(pane.cwd.as_deref(), Some("/repo"));
        let session = pane.agent_session.clone().unwrap();
        assert_eq!(
            session,
            AgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                kind: "id".into(),
                value: "abc".into(),
            }
        );
    }

    /// Most panes report no session at all — measured with four agents running
    /// in one tab, grok reported none and codex reported none until it had
    /// taken a turn — so the field must stay optional all the way through.
    #[test]
    fn a_pane_without_a_session_still_parses() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "workspaces": [], "tabs": [],
            "panes": [{ "pane_id": "w1:p2", "tab_id": "w1:t1",
                        "agent": "grok", "agent_status": "idle" }]
        }))
        .unwrap();
        assert!(snapshot.panes[0].agent_session.is_none());
        assert!(snapshot.panes[0].cwd.is_none());
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test herdr::tests`
Expected: PASS, including the two new tests.

- [ ] **Step 5: Commit**

```bash
git add src/herdr.rs
git commit -m "feat: expose pane session context and zoom"
```

---

## Task 16: Unwrap the raw-output view

**Track:** `api`

**Files:**
- Modify: `src/main.rs` (the `Source::as_herdr` mapping)

**Interfaces:** no signature change.

`recent` returns rows already broken at the pane's width — a measured 400-character line
came back as ten rows in a 44-column pane — while `recent_unwrapped` returns it whole. Raw
output stays the product for shell panes, so the fix belongs here.

- [ ] **Step 1: Change the mapping**

```rust
impl Source {
    fn as_herdr(self) -> &'static str {
        match self {
            // `recent` hands back rows already broken at the pane's width, so a
            // narrow pane's output arrives pre-wrapped and unreadable on a
            // phone; the unwrapped source joins the soft wraps back up.
            Source::Scrollback => "recent_unwrapped",
            Source::Screen => "visible",
        }
    }
}
```

- [ ] **Step 2: Lock it with a test**

This one restates a one-line mapping on purpose: the value is a herdr protocol string, and
the test exists so a future edit back to `recent` fails rather than quietly re-wrapping
every shell pane.

```rust
    #[test]
    fn scrollback_asks_herdr_to_unwrap() {
        assert_eq!(Source::Scrollback.as_herdr(), "recent_unwrapped");
        assert_eq!(Source::Screen.as_herdr(), "visible");
    }
```

- [ ] **Step 3: Run the test**

Run: `cargo test scrollback_asks_herdr`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "fix: ask herdr for unwrapped scrollback"
```

---

## Task 17: The transcript route

**Track:** `api`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `herdr::pane_context`, `transcript::{PaneRef, Source, Transcript, home, resolve}`, `markdown::{escape, preview, to_html}`.
- Produces: `GET /api/panes/{id}/transcript?before=&limit=`, and the `AppState` every later stateful handler extracts.

- [ ] **Step 1: Add the state, the window function, and the handler**

Change the serde import to `use serde::{Deserialize, Serialize};`, and add:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use herdr_remote::transcript::{self, Transcript};
use herdr_remote::{live, markdown};

/// One parsed transcript per pane, keyed with the source it was parsed from so
/// a pane that starts a new session does not keep serving the old one.
type Cache = Arc<Mutex<HashMap<String, (transcript::Source, Transcript)>>>;

/// Axum carries one router state, so the two things handlers need travel
/// together rather than as two `with_state` calls that cannot both exist.
#[derive(Clone, Default)]
struct AppState {
    transcripts: Cache,
    zoomed: Zoomed,
}

#[derive(Serialize)]
struct Card {
    seq: u64,
    role: transcript::Role,
    preview: String,
    html: String,
}

#[derive(Serialize)]
struct TranscriptPage {
    messages: Vec<Card>,
    has_more: bool,
}

#[derive(Deserialize)]
struct TranscriptWindow {
    before: Option<u64>,
    limit: Option<usize>,
}

/// The newest `limit` messages, or the `limit` messages whose `seq` is strictly
/// below `before`. `has_more` says whether anything older remains.
fn window(messages: &[transcript::Message], before: Option<u64>, limit: usize)
    -> (&[transcript::Message], bool)
{
    let end = match before {
        Some(before) => messages.partition_point(|message| message.seq < before),
        None => messages.len(),
    };
    let start = end.saturating_sub(limit);
    (&messages[start..end], start > 0)
}

fn card(message: &transcript::Message) -> Card {
    Card {
        seq: message.seq,
        role: message.role,
        preview: markdown::preview(&message.text, 300),
        html: match message.role {
            transcript::Role::Assistant => markdown::to_html(&message.text),
            // The user's own words are not markup, and reach the phone through
            // the same field, so they arrive as HTML that means what they typed.
            transcript::Role::User => markdown::escape(&message.text),
        },
    }
}

/// Parsing a cold transcript — forty-four megabytes at the extreme — reading
/// SQLite, and rendering markdown are all synchronous. `None` means the source
/// stopped being readable, which is the raw-output fallback rather than an
/// error.
fn take_window(
    cache: Cache,
    key: String,
    source: transcript::Source,
    before: Option<u64>,
    limit: usize,
) -> Option<(String, Vec<transcript::Message>, bool)> {
    let mut map = cache.lock().ok()?;
    let stale = map
        .get(&key)
        .is_none_or(|(cached, _)| *cached != source);
    if stale {
        map.insert(key.clone(), (source.clone(), Transcript::open(source)));
    }
    let (_, transcript) = map.get_mut(&key)?;
    if transcript.refresh().is_err() {
        // The file went away, or the store is damaged. Half a history is worse
        // than none: drop it and let the pane fall back.
        map.remove(&key);
        return None;
    }
    let version = transcript.version();
    let (messages, has_more) = window(transcript.messages(), before, limit);
    Some((version, messages.to_vec(), has_more))
}

/// A pane with no resolvable transcript answers 404: the phone reads that as
/// "use the raw output view", not as a fault.
async fn transcript_route(
    State(state): State<AppState>,
    Path(pane_id): Path<String>,
    Query(query): Query<TranscriptWindow>,
    headers: header::HeaderMap,
) -> ApiResult<Response> {
    let context = herdr::pane_context(&pane_id)
        .await
        .map_err(failed("could not read the herdr session"))?
        .ok_or((StatusCode::NOT_FOUND, "no such pane"))?;
    let home = transcript::home();
    let session = context.session.as_ref();
    let source = transcript::resolve(
        &transcript::PaneRef {
            agent: context.agent.as_deref().unwrap_or_default(),
            session_kind: session.map(|session| session.kind.as_str()),
            session_value: session.map(|session| session.value.as_str()),
            cwd: &context.cwd,
            title: context.title.as_deref(),
        },
        &home,
    )
    .ok_or((StatusCode::NOT_FOUND, "no transcript for this pane"))?;

    let limit = query.limit.unwrap_or(30).clamp(1, 200);
    let before = query.before;
    let cache = state.transcripts.clone();
    let key = pane_id.clone();
    let taken = tokio::task::spawn_blocking(move || take_window(cache, key, source, before, limit))
        .await
        .map_err(|error| {
            eprintln!("transcript task failed: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not read the transcript")
        })?;
    let Some((version, messages, has_more)) = taken else {
        return Err((StatusCode::NOT_FOUND, "no transcript for this pane"));
    };

    // The window is part of the identity: an ETag that named only the file's
    // length would let a `before=` page answer 304 for content the phone has
    // never held.
    let anchor = before.map_or_else(|| "tail".to_string(), |before| before.to_string());
    let etag = format!("\"{version}-{anchor}-{limit}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }

    let page = TranscriptPage {
        messages: messages.iter().map(card).collect(),
        has_more,
    };
    Ok(([(header::ETAG, etag)], Json(page)).into_response())
}
```

Register the route and give the API router its state:

```rust
        .route("/panes/{pane_id}/transcript", get(transcript_route))
        …
        .with_state(AppState::default())
```

- [ ] **Step 2: Test the window, which is where the route's decisions live**

```rust
    fn messages(seqs: &[u64]) -> Vec<herdr_remote::transcript::Message> {
        seqs.iter()
            .map(|seq| herdr_remote::transcript::Message {
                seq: *seq,
                role: herdr_remote::transcript::Role::User,
                text: format!("m{seq}"),
            })
            .collect()
    }

    #[test]
    fn no_cursor_returns_the_newest_window() {
        let all = messages(&[0, 1, 2, 3, 4]);
        let (page, has_more) = window(&all, None, 2);
        assert_eq!(page.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![3, 4]);
        assert!(has_more);
    }

    #[test]
    fn a_cursor_is_exclusive_and_reaches_further_back() {
        let all = messages(&[0, 1, 2, 3, 4]);
        let (page, has_more) = window(&all, Some(3), 2);
        assert_eq!(page.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert!(has_more);
    }

    #[test]
    fn the_oldest_page_reports_nothing_more() {
        let all = messages(&[0, 1, 2]);
        let (page, has_more) = window(&all, Some(2), 30);
        assert_eq!(page.len(), 2);
        assert!(!has_more);
    }

    #[test]
    fn an_empty_transcript_is_an_empty_page_not_an_error() {
        let (page, has_more) = window(&[], None, 30);
        assert!(page.is_empty());
        assert!(!has_more);
    }

    #[test]
    fn a_user_card_carries_escaped_text_and_an_agent_card_carries_html() {
        let user = card(&herdr_remote::transcript::Message {
            seq: 0,
            role: herdr_remote::transcript::Role::User,
            text: "<b>hi</b>".into(),
        });
        assert_eq!(user.html, "&lt;b&gt;hi&lt;/b&gt;");
        let agent = card(&herdr_remote::transcript::Message {
            seq: 1,
            role: herdr_remote::transcript::Role::Assistant,
            text: "**hi**".into(),
        });
        assert!(agent.html.contains("<strong>hi</strong>"));
    }
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --bin herdr-remote window` then `cargo test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: serve pane transcripts"
```

---

## Task 18: The live route

**Track:** `api`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `herdr::read`, `herdr::pane_context`, `live::composer`.
- Produces: `GET /api/panes/{id}/live` → `{ screen, composer }`.

- [ ] **Step 1: Add the handler**

```rust
#[derive(Serialize)]
struct LiveView {
    screen: String,
    composer: String,
}

/// The whole visible screen, not a cropped region: a picker is drawn wherever
/// the agent likes, the pane's status stays `idle` while one is open, and a
/// zoomed screen measured 1,393 bytes. `MAX_OUTPUT_LINES` is the same ceiling
/// the raw-output route uses, and no measured screen approaches it.
async fn live_route(Path(pane_id): Path<String>) -> ApiResult<Json<LiveView>> {
    let context = herdr::pane_context(&pane_id)
        .await
        .map_err(failed("could not read the herdr session"))?
        .ok_or((StatusCode::NOT_FOUND, "no such pane"))?;
    let output = herdr::read(&pane_id, MAX_OUTPUT_LINES, "visible")
        .await
        .map_err(failed("could not read the pane"))?;
    let composer = context
        .agent
        .as_deref()
        .and_then(|agent| live::composer(agent, &output.text))
        .unwrap_or_default();
    Ok(Json(LiveView {
        screen: output.text,
        composer,
    }))
}
```

Register it: `.route("/panes/{pane_id}/live", get(live_route))`.

- [ ] **Step 2: Lock the response shape**

The handler needs a running herdr, but its contract with the phone does not:

```rust
    #[test]
    fn the_live_view_names_its_fields_the_way_the_phone_reads_them() {
        let json = serde_json::to_value(LiveView {
            screen: "❯ draft".into(),
            composer: "draft".into(),
        })
        .unwrap();
        assert_eq!(json["screen"], "❯ draft");
        assert_eq!(json["composer"], "draft");
    }
```

- [ ] **Step 3: Run the suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: serve the pane's live screen and composer line"
```

---

## Task 19: The zoom slot

**Track:** `api`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `herdr::zoom`, `AppState`.
- Produces: `POST /api/panes/{id}/open`, `POST /api/panes/{id}/close`.

One slot, not a timer: a phone that dies without sending `close` is repaired by the next
`open`, because opening a pane releases whatever the slot still holds. The slot moves only
after the effect it describes has happened, and the whole transition is serialized so two
requests cannot interleave their zoom calls.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn opening_a_second_pane_supersedes_the_first() {
        assert_eq!(superseded(&None, "w1:p1"), None);
        assert_eq!(superseded(&Some("w1:p1".into()), "w1:p2"), Some("w1:p1".into()));
        // Re-opening the pane already held is not a transition.
        assert_eq!(superseded(&Some("w1:p1".into()), "w1:p1"), None);
    }
```

- [ ] **Step 2: Add the slot and the handlers**

```rust
/// The single pane this server has zoomed. A `tokio` mutex, because the
/// transition awaits herdr while holding it: two concurrent opens must not
/// trade the slot between each other's zoom calls.
#[derive(Clone, Default)]
struct Zoomed(Arc<tokio::sync::Mutex<Option<String>>>);

/// The pane that must be released before `pane_id` can take the slot.
fn superseded(slot: &Option<String>, pane_id: &str) -> Option<String> {
    slot.clone().filter(|held| held != pane_id)
}

async fn open(
    State(state): State<AppState>,
    Path(pane_id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut slot = state.zoomed.0.lock().await;
    if let Some(previous) = superseded(&slot, &pane_id) {
        // herdr keeps one zoom per tab, so zooming the new pane moves the zoom
        // anyway; a failure here is worth reporting and not worth refusing the
        // pane the phone actually asked for.
        if let Err(error) = herdr::zoom(&previous, false).await {
            eprintln!("could not unzoom {previous}: {error:#}");
        }
    }
    herdr::zoom(&pane_id, true)
        .await
        .map_err(failed("could not zoom the pane"))?;
    *slot = Some(pane_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn close(
    State(state): State<AppState>,
    Path(pane_id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut slot = state.zoomed.0.lock().await;
    if slot.as_deref() != Some(pane_id.as_str()) {
        return Ok(StatusCode::NO_CONTENT);
    }
    herdr::zoom(&pane_id, false)
        .await
        .map_err(failed("could not unzoom the pane"))?;
    // Only after the pane is actually back: a slot cleared on a failed unzoom
    // would leave a zoomed pane that no later `open` knows to release.
    *slot = None;
    Ok(StatusCode::NO_CONTENT)
}
```

Register both routes with `post`.

- [ ] **Step 3: Run the tests**

Run: `cargo test opening_a_second_pane` then `cargo test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: zoom the pane the phone is reading"
```

---

## Task 20: The end-to-end pass

**Track:** `api` (run once, after every track has been integrated)

**Files:** none.

Every measurement this design rests on came from a live herdr, and the parts that cannot
be unit tested — zoom moving a real pane, a real picker becoming legible, a real transcript
appearing on a phone-sized screen — are exactly the parts worth walking once.

- [ ] **Step 1: Start the app**

Run: `make check` then `make run`.
Expected: both succeed; `web/dist` is rebuilt and served.

- [ ] **Step 2: Walk the sequence**

With claude, codex, cursor, and grok panes running in one tab:

1. Open a claude pane. Thirty messages appear, user and agent in different lanes, each
   clamped to three lines.
2. Tap a long agent message. The modal shows the whole thing with its markdown rendered —
   headings, a table, a fenced block — and scrolls horizontally rather than the page.
3. Tap a user message. The modal shows the text with its own line breaks.
4. Tap "earlier". Thirty older messages prepend and the reading position does not jump.
5. While the pane is working, the hammer animates and no sentence describes the state.
6. Type into that pane on the desktop without sending. The draft's first line appears in
   the band, ellipsised.
7. In a codex pane, run the model picker. Tap the maximize button: the picker is fully
   legible, because the pane is zoomed.
8. Press the arrow keys and enter from the phone; the selection moves and commits.
9. Leave the pane. The desktop pane unzooms.
10. Open a shell pane. Raw output appears, and a long line is not broken at the pane's
    width.

- [ ] **Step 3: Record the result**

Write what happened to `.tmp/e2e-transcript-view.md`, including anything that did not
behave as described. This is the artifact the final review reads.

---

## Self-review notes

Checked against the spec:

- Layer table, route list, resolution table, path boundary, normalization, markdown,
  cache, zoom, front end, degradation, and testing all have tasks.
- "History applies to agent panes whose transcript resolved" is Task 14's 404 branch; the
  "empty transcript is still the transcript view" clause holds because resolution
  succeeding is what selects the view, not the message count — Task 17 tests the empty
  window separately.
- All four composer boxes are measured; grok turned out to share claude's rule rather than
  need one of its own.
- Missing `agent_session` is measured too, and Task 7 carries a cwd-based path for every
  agent plus cursor's `createdAtMs` floor.
- No task writes a file owned by another track: `Cargo.toml`, `Cargo.lock`, `src/lib.rs`,
  `src/transcript/**`, `src/markdown.rs`, `src/live.rs` belong to `rust-core`; `web/**` to
  `web`; `src/herdr.rs` and `src/main.rs` to `api`.

Known limitations left in deliberately:

- Placeholder detection is prefix-based, because a narrow pane truncates the placeholder
  itself (`› Ask Codex to do an`). A real draft that begins with one of those prefixes
  reads as empty. Blank is the safe direction; the alternative is presenting a
  placeholder as something the person typed.
- Two codex panes in one directory, neither reporting a session id, resolve to the same
  newest rollout until herdr reports their ids.
