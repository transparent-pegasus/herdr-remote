# Artful-Simplicity Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use pane-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the 14 findings of the artful-simplicity audit — close the agent-only enforcement gap, unstale the session, fold the duplicated helpers, and cover the socket framing with tests.

**Architecture:** The agent-only invariant moves server-side (403 for shells, 404 for unknown panes) with the client gate kept for UX; the session is refreshed by one steady single-flight 5-second poll on every page, replacing the failure-only retry timer; env reads move to `main()` so `allowed_hosts` and `exchange` become pure and testable without `unsafe`.

**Tech Stack:** Rust (axum, tokio) + TypeScript (Astro, Vitest, Biome). `aube` is the only package manager.

**Spec:** `.tmp/audit-artful-simplicity.md` (audit findings A1-A5, F1-F4, D1-D2, P1-P2, T1) — decisions: A1 = server + client gates; A2 = 5 s session poll on all pages. Both independent plan reviews are resolved into this revision (`.tmp/plan-review-codex.md`, `.tmp/plan-review-grok.md`).

## Global Constraints

- `aube` only — never `npm`/`pnpm`/`yarn`. TypeScript `any` is prohibited.
- Every task ends green through `make check` (test → lint → format; logs in `.tmp/`). Run `make format` before `make check` on freshly written code — formatting runs last, so unformatted code trips the lint stage.
- **Starting tree:** `main` at `06e7851`, clean (only `docs/` is untracked). The Enter-on-empty feature already landed **ungated** as `6747abb`; Task 1 commits the gate delta only. `.herdrpowers/config.yaml` is untracked and gitignored as of `6433c8b` — it is edited on disk, never `git add`ed.
- Smoke checks run the freshly built debug binary on an explicit loopback address and a unique port, independent of `.env` (which may be absent in a worktree and whose `BIND_ADDR` alias may not exist after a reboot). Never `pkill` by name — kill the exact PID you started.
- Reference code by function name, not pre-change line numbers: earlier tasks shift lines.
- Commit messages follow the repo's existing style: `type: lower-case summary`, body explains the why, `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

## Tracks

| Track | Goal | Tasks | Owned files | Depends on |
|---|---|---|---|---|
| `gate` | Enforce the agent-only invariant for bare keys on both sides | 1 | `src/herdr.rs`, `src/main.rs`, `web/src/pages/index.astro` | — |
| `rust` | Pure, tested Rust internals: env params, framing tests, dead derive | 2-3 | `src/main.rs`, `src/herdr.rs` | `gate` |
| `web` | Fresh session everywhere; CSS/markup honesty; helper folds | 4-6 | `web/src/pages/index.astro`, `web/src/lib/api.ts` | `gate` |
| `docs` | README matches the API; config booleans are booleans | 7 | `README.md`, `.herdrpowers/config.yaml` (disk edit only) | `gate` |

Owned files overlap between `gate` and the later tracks only across the dependency edge — `gate` finishes and commits before any other track starts, so no two tracks ever write a file concurrently. `rust`, `web`, `docs` are mutually disjoint and may run in parallel worktrees. Tasks 2 and 3 touch disjoint files but stay one sequential track: a third worktree buys ~nothing on tasks this small.

**Post-integration follow-ups:** run `make check` once at repo root after all tracks merge; hand-test on the phone (stop/go/zap/send enabling, list staleness, composer dim).

**Smoke-server recipe** (used by Tasks 1 and 4 — copy verbatim; `jq` is available on this host):

```bash
make web && cargo build
BIN=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/debug/herdr-remote
BIND_ADDR=127.0.0.1 PORT=18799 "$BIN" & SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
sleep 2
B=http://127.0.0.1:18799
```

---

### Task 1: Gate the bare-key routes to agent panes, on both sides

**Track:** `gate`

**Files:**
- Modify: `src/herdr.rs` (extract `pane_is_agent`, reuse in `prompt`)
- Modify: `src/main.rs` (`interrupt`/`enter` fold into one gated path; pure `key_gate` + unit test)
- Modify: `web/src/pages/index.astro` (`syncSend` gates empty-Enter to agent panes)

**Interfaces:**
- Produces: `herdr::pane_is_agent(pane_id: &str) -> Result<Option<bool>>` — `Some(true)` agent pane, `Some(false)` shell, `None` no such pane.
- Produces: HTTP semantics `POST /api/panes/{id}/interrupt` and `/enter` → 204 agent, 403 shell (`"not an agent pane"`), 404 unknown (`"no such pane"`). Task 7 documents them.

- [ ] **Step 1: Write the failing gate test in `src/main.rs`'s `mod tests`**

```rust
    #[test]
    fn bare_keys_reach_agents_only() {
        assert_eq!(key_gate(Some(true)), Ok(()));
        assert_eq!(
            key_gate(Some(false)),
            Err((StatusCode::FORBIDDEN, "not an agent pane"))
        );
        assert_eq!(key_gate(None), Err((StatusCode::NOT_FOUND, "no such pane")));
    }
```

Run: `cargo test bare_keys` — expected: FAIL to compile, `key_gate` not found.

- [ ] **Step 2: Extract the pane lookup in `src/herdr.rs`**

Replace the lookup inside `prompt` with a shared helper directly above it (`press` stays as committed):

```rust
/// `Some(true)` = agent pane, `Some(false)` = plain shell, `None` = no such
/// pane. One snapshot answers both "does it exist" and "who is listening".
pub async fn pane_is_agent(pane_id: &str) -> Result<Option<bool>> {
    Ok(snapshot()
        .await?
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .map(|pane| pane.agent.is_some()))
}

/// Agent panes take a prompt; a plain shell needs the text plus a newline.
/// `None` when no such pane exists, so the caller can answer 404.
pub async fn prompt(pane_id: &str, text: &str) -> Result<Option<()>> {
    let Some(has_agent) = pane_is_agent(pane_id).await? else {
        return Ok(None);
    };

    if has_agent {
        call("agent.prompt", json!({ "target": pane_id, "text": text })).await?;
    } else {
        call(
            "pane.send_input",
            json!({ "pane_id": pane_id, "text": text, "keys": ["enter"] }),
        )
        .await?;
    }
    Ok(Some(()))
}
```

- [ ] **Step 3: Gate the key endpoints in `src/main.rs`**

Replace the two ungated handlers (`async fn interrupt`, `async fn enter`) with:

```rust
/// The three-way answer for a bare-key route: who is listening decides.
fn key_gate(pane: Option<bool>) -> Result<(), (StatusCode, &'static str)> {
    match pane {
        None => Err((StatusCode::NOT_FOUND, "no such pane")),
        Some(false) => Err((StatusCode::FORBIDDEN, "not an agent pane")),
        Some(true) => Ok(()),
    }
}

/// The two bare keys the phone needs: esc stops an agent's turn, enter answers
/// the question it is showing. Both are agent-only — a shell pane would treat
/// the key as terminal input and execute whatever sits on its command line, so
/// the server refuses rather than trusting the UI's gate.
async fn press(pane_id: &str, key: &str, what: &'static str) -> ApiResult<StatusCode> {
    key_gate(herdr::pane_is_agent(pane_id).await.map_err(failed(what))?)?;
    herdr::press(pane_id, key).await.map_err(failed(what))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn interrupt(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "esc", "could not interrupt the pane").await
}

async fn enter(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "enter", "could not send enter to the pane").await
}
```

Routes are unchanged. Run: `cargo test bare_keys` — expected: PASS.

- [ ] **Step 4: Gate empty-Enter client-side in `web/src/pages/index.astro`**

Replace `syncSend`:

```ts
/** With nothing typed the same button sends the Enter key, which only an
 *  agent's own question wants — a shell would execute its half-typed line.
 *  With text, anyone can send: prompt handles shells with send_input. */
function syncSend() {
  const empty = textEl.value.trim() === "";
  sendEl.disabled = textEl.disabled || (empty && !currentPane()?.agent);
  sendEl.classList.toggle("empty", empty);
  const what = empty ? "Enter" : "Send";
  sendEl.title = what;
  sendEl.setAttribute("aria-label", what);
}
```

(`currentPane()` already exists; audit P2 — the name `syncSend` is honest again because the function once more decides `disabled`.)

- [ ] **Step 5: Verify live (server half)**

```bash
make format && make check          # expect exit 0
# Smoke-server recipe from the plan header, then:
SHELL_PANE=$(curl -s $B/api/session | jq -r '[.tabs[].panes[] | select(.agent==null)][0].id')
test -n "$SHELL_PANE" && test "$SHELL_PANE" != null   # a shell pane must exist for this check
curl -s -o /dev/null -w '%{http_code}\n' -X POST "$B/api/panes/$SHELL_PANE/enter"       # 403
curl -s -X POST "$B/api/panes/$SHELL_PANE/interrupt"; echo                               # not an agent pane
curl -s -o /dev/null -w '%{http_code}\n' -X POST "$B/api/panes/w0:none/interrupt"        # 404
kill $SRV
```

Expected: `403` / `not an agent pane` / `404`. (The 204 agent path is exercised from the phone; esc to an idle agent is harmless.)

- [ ] **Step 6: Commit**

```bash
git add src/herdr.rs src/main.rs web/src/pages/index.astro
git commit -m "fix: gate the bare-key routes to agent panes

esc and enter reached any pane: a shell treats the key as terminal
input and executes whatever sits on its command line. The server now
answers 403 for a shell and 404 for an unknown pane instead of
trusting the UI, and the empty-composer Enter is disabled client-side
for shells like the other agent-only buttons.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `allowed_hosts` takes the configured list as a parameter

**Track:** `rust`

**Files:**
- Modify: `src/main.rs` — `fn allowed_hosts`, its call in `main()`, all cases in `mod tests` (find by name; Task 1 shifted line numbers)

**Interfaces:**
- Produces: `fn allowed_hosts(bind: &str, port: &str, extra: Option<&str>) -> Vec<String>` — pure; `main()` alone reads `ALLOWED_HOSTS`, matching how `PORT`/`BIND_ADDR` already flow in.

- [ ] **Step 1: Make the join test honest first (it must fail)**

Rewrite the third test to actually call `allowed_hosts` with a configured list:

```rust
#[test]
fn configured_hosts_join_loopback_and_match_case_insensitively() {
    let allowed = allowed_hosts("127.0.0.1", "8787", Some(" Herdr.Example.COM ,, "));
    assert!(host_allowed("herdr.example.com", &allowed));
    assert!(host_allowed("HERDR.EXAMPLE.COM", &allowed));
    // A suffix is not a match, and local work still reaches the server.
    assert!(!host_allowed("herdr.example.com.evil.net", &allowed));
    assert!(host_allowed("127.0.0.1:8787", &allowed));
}
```

Run: `cargo test configured_hosts` — expected: **compile error** (wrong arity). That is the failing state.

- [ ] **Step 2: Change the signature and the call site**

```rust
/// Loopback is always accepted so `make run` keeps working; a rebinding page
/// sends the attacker's own hostname as `Host` and cannot forge 127.0.0.1, so
/// allowing it costs nothing. The address we bind is accepted for the same
/// reason: reaching it already required a packet arriving on that interface.
/// `extra` (the ALLOWED_HOSTS env var, read by main) adds the tunnel's public
/// hostname on top, comma-separated.
fn allowed_hosts(bind: &str, port: &str, extra: Option<&str>) -> Vec<String> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let own = format!("{}:{port}", bind.trim().to_ascii_lowercase());
    if !hosts.contains(&own) {
        hosts.push(own);
    }
    if let Some(list) = extra {
        hosts.extend(
            list.split(',')
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty()),
        );
    }
    hosts
}
```

In `main()`:

```rust
    let extra = std::env::var("ALLOWED_HOSTS").ok();
    let allowed = Allowed(Arc::new(allowed_hosts(&bind, &port, extra.as_deref())));
```

- [ ] **Step 3: Drop the env mutation from the other two tests**

Delete both `unsafe { std::env::remove_var("ALLOWED_HOSTS") };` lines and their `// SAFETY` comments; pass `None` instead:

```rust
    fn unset_allows_only_loopback() {
        let allowed = allowed_hosts("127.0.0.1", "8787", None);
        ...
    fn the_bound_address_is_accepted_without_extra_config() {
        let allowed = allowed_hosts("10.99.99.1", "8787", None);
        ...
```

(Assertion bodies unchanged.)

- [ ] **Step 4: Verify and commit**

Run: `cargo test` — expected: all pass, no `unsafe` left in `mod tests`. Then `make format && make check` → exit 0.

```bash
git add src/main.rs
git commit -m "refactor: allowed_hosts takes its list as a parameter

main() already owns the PORT and BIND_ADDR reads; ALLOWED_HOSTS was the
one env read hiding in a helper. Passing it in makes the function pure,
deletes both unsafe env mutations from the tests, and lets the join
test exercise the join its name promises.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Cover the socket framing with a fake herdr; drop the dead derive

**Track:** `rust`

**Files:**
- Modify: `src/herdr.rs` (`exchange` takes the socket path; `Output` loses `Serialize`; new `#[cfg(test)]` cases)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `async fn exchange(path: &str, method: &str, params: Value) -> Result<Value>` (private; `call` passes `socket_path()`).

- [ ] **Step 1: Thread the socket path through `exchange`**

```rust
/// Send one request, return its `result`.
pub async fn call(method: &str, params: Value) -> Result<Value> {
    // A herdr that accepts the connection and then never answers must not pin
    // an axum task and a socket descriptor forever.
    timeout(TIMEOUT, exchange(&socket_path(), method, params))
        .await
        .with_context(|| format!("herdr {method} timed out after {TIMEOUT:?}"))?
}

async fn exchange(path: &str, method: &str, params: Value) -> Result<Value> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connect to herdr socket at {path}"))?;
    // body unchanged from here down
```

(Delete the old `let path = socket_path();` line.)

- [ ] **Step 2: Drop `Serialize` from `Output`**

```rust
#[derive(Deserialize)]
pub struct Output {
```

(Repo-wide check: the only consumer is `main.rs::output`, which returns `(headers, String)`; nothing serializes `Output`.)

- [ ] **Step 3: Write the framing tests (Steps 1-3 land together, then run)**

Append inside the existing `mod tests` — no extra `use tokio::io::...` imports: `use super::*` already carries the parent's `AsyncReadExt`/`AsyncWriteExt`, and a duplicate import fails `clippy -D warnings`. Every probe is wrapped in a short timeout so a broken fake hangs the test, not `cargo test`; the spawned server unlinks its own socket.

```rust
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::UnixListener;

    /// A herdr that accepts one connection, reads the request, replies with
    /// fixed bytes, hangs up, and removes its socket.
    fn fake_herdr(reply: &'static [u8]) -> String {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("herdr-remote-test-{}-{n}.sock", std::process::id()));
        let path = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let socket = path.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(reply).await.unwrap();
            drop(stream);
            let _ = std::fs::remove_file(&socket);
        });
        path
    }

    /// exchange() has no timeout of its own (call() adds the 10 s one), so cap
    /// the probe: a broken fake must fail the test, not hang the suite.
    async fn probe(path: &str) -> Result<Value> {
        timeout(Duration::from_secs(2), exchange(path, "ping", json!({})))
            .await
            .expect("probe outlived its 2 s cap")
    }

    #[tokio::test]
    async fn a_clean_reply_returns_its_result() {
        let path = fake_herdr(b"{\"id\":\"herdr-remote\",\"result\":{\"type\":\"ok\"}}\n");
        let result = probe(&path).await.unwrap();
        assert_eq!(result["type"], "ok");
    }

    #[tokio::test]
    async fn eof_before_any_reply_is_an_error() {
        let path = fake_herdr(b"");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("closed before replying"));
    }

    #[tokio::test]
    async fn a_frame_without_its_newline_is_truncated() {
        let path = fake_herdr(b"{\"id\":\"herdr-remote\",\"result\":{}}");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("truncated"));
    }

    #[tokio::test]
    async fn a_herdr_error_is_surfaced() {
        let path =
            fake_herdr(b"{\"id\":\"herdr-remote\",\"error\":{\"code\":\"x\",\"message\":\"y\"}}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("failed"));
    }

    #[tokio::test]
    async fn a_reply_with_a_foreign_id_is_rejected() {
        let path = fake_herdr(b"{\"id\":\"someone-else\",\"result\":{}}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("foreign id"));
    }

    #[tokio::test]
    async fn a_reply_missing_its_result_is_rejected() {
        let path = fake_herdr(b"{\"id\":\"herdr-remote\"}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("no result"));
    }
```

The 10-second timeout branch in `call` stays untested on purpose: proving it means a 10 s test or a timeout knob nothing else needs.

- [ ] **Step 4: Verify and commit**

Run: `cargo test` — expected: all pass (existing + 6 new). Then `make format && make check` → exit 0.

```bash
git add src/herdr.rs
git commit -m "test: cover the socket framing against a fake herdr

exchange() was the densest logic in the repo with zero coverage; its
five failure branches (EOF, truncated frame, herdr error, foreign id,
missing result) now run against a scripted UnixListener. The socket
path travels as a parameter so the tests need no env mutation. Output
loses a Serialize derive nothing used.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: One steady single-flight session poll on every page

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro` (script only: `load`, the retry timer, listeners)

**Interfaces:**
- Consumes: nothing new. Produces: none (behavioral).

- [ ] **Step 1: Replace failure-retry with a steady single-flight poll**

In the script, change the constants/state:

```ts
const POLL_MS = 3000;
/** The session is smaller and changes slower than pane output. */
const SESSION_POLL_MS = 5000;
```

Delete `let retry: ReturnType<typeof setTimeout> | undefined;` and `let loads = 0;`; add:

```ts
let sessionJson = "";
let loading = false;
let sessionFailed = false;
```

Replace `load()`:

```ts
async function load() {
  // Single flight: a request is allowed 15 s, the tick fires every 5 s. Lapping
  // it would mean every slow success arrives pre-superseded and the session
  // never lands on a slow tunnel — the case the poll exists for.
  if (loading) return;
  loading = true;
  try {
    const fresh = await fetchSession(AbortSignal.timeout(15000));
    // Touch only messages this function owns: "Loading…" (boot and manual
    // refresh) and its own earlier failure. watch()'s errors are its business —
    // wiping them here would flicker a genuinely failing pane read every 5 s.
    if (statusEl.textContent === "Loading…") {
      say(fresh.tabs.length ? "" : "No panes.");
    } else if (sessionFailed) {
      say(""); // our own recovery; watch re-asserts its errors within 3 s
    }
    sessionFailed = false;
    // Same bytes, same DOM — the steady poll must not churn the list.
    const json = JSON.stringify(fresh);
    if (json !== sessionJson) {
      sessionJson = json;
      session = fresh;
      render();
    }
  } catch (error) {
    sessionFailed = true;
    complain(error, "Could not load.");
  } finally {
    loading = false;
  }
}
```

Replace the visibilitychange listener and the boot lines at the bottom:

```ts
// Coming back to the app resyncs at once rather than after a poll: the
// phone will have slept, and WARP reconnects on its own schedule.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) load();
});
// The steady poll is also the retry: a failed load is simply stale until
// the next tick, and a backgrounded tab polls nothing.
setInterval(() => {
  if (!document.hidden) load();
}, SESSION_POLL_MS);

refreshEl.addEventListener("click", () => {
  say("Loading…");
  load();
});
say("Loading…");
load();
```

(Also remove the old `const mine = ++loads;` / `if (mine !== loads) return;` pair, the `clearTimeout(retry);` line, the `retry = setTimeout(load, POLL_MS);` line, and the bare `refreshEl.addEventListener("click", load);`.)

- [ ] **Step 2: Verify**

```bash
make format && make check          # exit 0
# Smoke-server recipe from the plan header (make web is part of it — the
# hashed /_astro bundle is stale until rebuilt, and cargo run serves web/dist).
```

In a desktop browser at `http://127.0.0.1:18799/`: open a pane page, start/finish an agent turn in herdr, watch the subtitle flip within ~5 s and the stop button enable/disable with it, with no scroll jump on the list page. `kill $SRV` after.

- [ ] **Step 3: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "fix: poll the session everywhere it is shown

stop's working-only gate and the list subtitles read pane state that
nothing refreshed. One 5-second single-flight poll keeps the session
fresh on every page and replaces the failure-only retry timer; a JSON
compare skips render when nothing changed, the same same-bytes-same-DOM
rule the log already follows.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: CSS and markup honesty

**Track:** `web`

**Files:**
- Modify: `web/src/pages/index.astro` (markup: one attribute; styles only otherwise)

**Interfaces:** none.

- [ ] **Step 1: Dim the composer only when no pane is picked (A3)**

```css
/* Disabled-for-lack-of-a-pane dims the panel; a single disabled button
   (stop on an idle agent, say) is its own 0.35 opacity, not the panel's. */
#composer:has(textarea:disabled) {
  opacity: 0.55;
}
```

(Replaces `#composer:has(:disabled)`.)

- [ ] **Step 2: Give #refresh/#back their own rule instead of an override fight (D2)**

Remove `#refresh,` and `#back` from the shared rule so it reads:

```css
#composer button {
  min-height: 44px;
  padding: 0.75rem;
  border: 1px solid var(--edge);
  border-radius: 0.5rem;
  background: none;
  color: inherit;
  font: inherit;
}
```

and make the icon-button block self-contained (drop `min-height: 0` and its comment; keep the 24px trade comment):

```css
/* 24px is the WCAG 2.2 AA floor (2.5.8), below Apple's 44pt guidance —
   a deliberate trade for a header that does not dominate a phone screen. */
#refresh,
#back {
  box-sizing: border-box;
  display: grid;
  place-items: center;
  height: 24px;
  min-width: 24px;
  padding: 0 0.35rem;
  border: 1px solid var(--edge);
  border-radius: 0.5rem;
  background: none;
  color: inherit;
  font: inherit;
  font-size: 0.85rem;
  line-height: 1;
  text-decoration: none;
}
```

- [ ] **Step 3: Delete the lying `enterkeyhint` (A5)**

Remove `enterkeyhint="send"` from the textarea — Enter inserts a newline there; the keyboard should say what the key does.

- [ ] **Step 4: Verify**

```bash
make format && make check && make web
if grep -q 'enterkeyhint' web/dist/index.html; then echo "still present"; exit 1; fi
grep -oE '#composer:has\([^)]*\)' web/dist/index.html          # textarea:disabled variant only
grep -oE '#refresh,#back\{[^}]*\}' web/dist/index.html         # one block, no min-height:0
```

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/index.astro
git commit -m "style: composer dims only without a pane; standalone icon rule

:has(:disabled) predates the four buttons and dimmed the whole
composer whenever any one of them was legitimately off. Key it off the
textarea, which tracks pane selection. #refresh/#back leave the 44px
shared rule instead of overriding it, and the textarea drops an
enterkeyhint that promised a send Enter does not perform.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Fold the POST helpers and the route lookup

**Track:** `web`

**Files:**
- Modify: `web/src/lib/api.ts` (one `post` helper; exports keep their names and signatures)
- Modify: `web/src/pages/index.astro` (`currentPane` → `current` returning `{ route, tab, pane }`; `render`, `act`, and `syncSend` use it)

**Interfaces:**
- Consumes: Task 1's `syncSend` (updates its `currentPane()` call).
- Produces: unchanged public api.ts exports — `sendPrompt`, `interruptPane`, `pressEnter` keep their exact signatures; only internals fold.

- [ ] **Step 1: Fold api.ts (F1)**

Replace the three POST functions with (no boolean spread — `astro/tsconfigs/strict` rejects `...(cond && {...})` with TS2698):

```ts
const paneUrl = (paneId: string, rest: string) =>
	`/api/panes/${encodeURIComponent(paneId)}/${rest}`;

async function post(url: string, fallback: string, body?: unknown): Promise<void> {
	const response = await fetch(
		url,
		body === undefined
			? { method: "POST" }
			: {
					method: "POST",
					headers: { "content-type": "application/json" },
					body: JSON.stringify(body),
				},
	);
	if (!response.ok) {
		throw new Error(await reason(response, fallback));
	}
}

export const sendPrompt = (paneId: string, text: string): Promise<void> =>
	post(paneUrl(paneId, "prompt"), "could not send", { text });

/** Esc to a pane that is mid-turn. */
export const interruptPane = (paneId: string): Promise<void> =>
	post(paneUrl(paneId, "interrupt"), "could not interrupt");

/** Enter to a pane, for the question an agent is already asking. */
export const pressEnter = (paneId: string): Promise<void> =>
	post(paneUrl(paneId, "enter"), "could not send enter");
```

`fetchOutput` switches its URL to `` `${paneUrl(paneId, "output")}?lines=${lines}&source=${source}` `` — nothing else in it changes.

- [ ] **Step 2: Run the existing tests**

Run: `cd web && aube run test` — expected: all pass unchanged (the exports' behavior is identical; `api.test.ts` needs no edit).

- [ ] **Step 3: Fold the duplicate lookup (F3)**

In index.astro, replace `currentPane` with:

```ts
/** Everything on screen derives from the URL; this is the one place that
 *  resolves it against the session. */
function current(): { route: Route; tab?: Tab; pane?: Pane } {
  const route = parseRoute(location.pathname);
  const tab = session?.tabs.find((candidate) => candidate.id === route.tabId);
  const pane = tab?.panes.find((candidate) => candidate.id === route.paneId);
  return { route, tab, pane };
}
```

Add `type Route` to the `../lib/route` import. In `render()`, replace the four lookup lines with:

```ts
const { route, tab, pane: found } = current();
const pane = found ?? null;
```

In `act()` replace `const pane = currentPane();` with `const pane = current().pane;`, and in `syncSend` replace `currentPane()?.agent` with `current().pane?.agent`.

- [ ] **Step 4: Verify and commit**

```bash
make format && make check    # exit 0; astro check catches any missed rename
```

```bash
git add web/src/lib/api.ts web/src/pages/index.astro
git commit -m "refactor: one POST helper, one route lookup

sendPrompt/interruptPane/pressEnter differed only in path and body;
they now share post() and a paneUrl builder, exports unchanged.
render(), act(), and syncSend() resolve the URL through a single
current() instead of three hand-rolled copies of the same find.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Documentation and configuration truth

**Track:** `docs`

**Files:**
- Modify: `README.md` (API section only — stage nothing else)
- Modify: `.herdrpowers/config.yaml` (disk edit; the file is untracked and gitignored as of `6433c8b` — do **not** `git add` it)

**Interfaces:**
- Consumes: Task 1's HTTP semantics (403/404 on the key endpoints).

- [ ] **Step 1: Bring the README API section up to the code**

Replace the API code block with:

```
GET  /api/health                     # "ok"
GET  /api/session                    # {"tabs":[{"id","label","panes":[{"id","label","agent","state"}]}]}
POST /api/panes/{pane_id}/prompt     # {"text": "..."} -> 204; 404 for an unknown pane
POST /api/panes/{pane_id}/interrupt  # Esc to an agent's turn -> 204; 403 for a shell pane, 404 unknown
POST /api/panes/{pane_id}/enter      # Enter, for the question an agent is showing; same rules
GET  /api/panes/{pane_id}/output     # plain text; ?lines=1..20000 (default 300),
                                     # ?source=scrollback|screen; x-truncated: true when more remains
```

After the existing `prompt` paragraph, add:

```
The bare-key routes are agent-only, enforced server-side: a shell pane would
execute whatever sits on its command line, so the server answers 403 rather
than trusting the UI's disabled buttons.
```

- [ ] **Step 2: Make the review-gate booleans booleans**

In `.herdrpowers/config.yaml`, change both `enabled: disabled` lines (task-review, fix-round-re-review) to `enabled: false`. `disabled` is a truthy YAML string — the opposite of the intent. Local file only; no staging.

- [ ] **Step 3: Verify and commit**

```bash
grep -n 'enabled:' .herdrpowers/config.yaml    # expect exactly: three true, two false
make check                                      # docs task too — the global rule holds
git add README.md
git commit -m "docs: document the key routes and the output window

interrupt/enter and the output parameters (?lines, ?source,
x-truncated) were undocumented, and the agent-only 403 semantics
belong next to the API they guard.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

(The `enabled: false` fix travels with this task but not with this commit — the file is untracked by design.)
