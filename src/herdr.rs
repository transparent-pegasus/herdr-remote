//! Thin client for the Herdr Unix socket: newline-delimited JSON, one request
//! per connection. Protocol shapes come from `herdr api schema --json`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

/// Our own id, echoed back by herdr; one request per connection, so a fixed
/// value is enough to catch a reply that belongs to someone else.
const REQUEST_ID: &str = "herdr-remote";
const TIMEOUT: Duration = Duration::from_secs(10);
/// A stalled peer must not be able to grow the read buffer without bound.
const MAX_FRAME: u64 = 4 << 20;

#[cfg(test)]
static TEST_SOCKET_PATH: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) struct TestSocketPath(Option<String>);

#[cfg(test)]
impl Drop for TestSocketPath {
    fn drop(&mut self) {
        *TEST_SOCKET_PATH.lock().unwrap() = self.0.take();
    }
}

#[cfg(test)]
pub(crate) fn use_test_socket(path: String) -> TestSocketPath {
    TestSocketPath(TEST_SOCKET_PATH.lock().unwrap().replace(path))
}

fn socket_path() -> String {
    #[cfg(test)]
    if let Some(path) = TEST_SOCKET_PATH.lock().unwrap().clone() {
        return path;
    }
    std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config/herdr/herdr.sock")
    })
}

/// Send one request, return its `result`.
async fn call(method: &str, params: Value) -> Result<Value> {
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
    let mut conn = BufReader::new(stream);

    let request = json!({ "id": REQUEST_ID, "method": method, "params": params });
    conn.get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await?;

    let mut line = String::new();
    let read = (&mut conn)
        .take(MAX_FRAME)
        .read_line(&mut line)
        .await
        .with_context(|| format!("read reply to herdr {method}"))?;
    if read == 0 {
        bail!("herdr {method}: socket closed before replying");
    }
    if !line.ends_with('\n') {
        bail!("herdr {method}: reply was truncated after {read} bytes");
    }

    let response: Value = serde_json::from_str(&line)
        .with_context(|| format!("herdr {method} returned non-JSON: {line}"))?;

    if let Some(error) = response.get("error") {
        bail!("herdr {method} failed: {error}");
    }
    if response.get("id").and_then(Value::as_str) != Some(REQUEST_ID) {
        bail!("herdr {method}: reply carried a foreign id");
    }
    response
        .get("result")
        .cloned()
        .with_context(|| format!("herdr {method} returned no result"))
}

// --- what herdr sends back, trimmed to the fields the UI needs ---

#[derive(Deserialize)]
struct Snapshot {
    workspaces: Vec<WorkspaceInfo>,
    tabs: Vec<TabInfo>,
    panes: Vec<PaneInfo>,
}

#[derive(Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    label: String,
}

#[derive(Deserialize)]
struct TabInfo {
    tab_id: String,
    workspace_id: String,
    label: String,
}

/// What an agent reported about its own session. Self-reported, hence the
/// boundary in `transcript::guard`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AgentSession {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Deserialize)]
struct PaneInfo {
    pane_id: String,
    tab_id: String,
    agent: Option<String>,
    agent_status: String,
    label: Option<String>,
    title: Option<String>,
    terminal_title_stripped: Option<String>,
    agent_session: Option<AgentSession>,
    cwd: Option<String>,
}

// --- what we expose, so a herdr schema change need not reach the phone ---

#[derive(Serialize, Debug, PartialEq)]
pub struct Session {
    pub workspaces: Vec<Workspace>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub label: String,
    pub tabs: Vec<Tab>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub panes: Vec<Pane>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Pane {
    pub id: String,
    pub label: String,
    pub agent: Option<String>,
    pub state: String,
}

impl PaneInfo {
    /// A pane renamed in herdr carries `label`; otherwise fall back to the
    /// terminal title, then to the id so a row is never blank.
    fn label(&self) -> String {
        [&self.label, &self.title, &self.terminal_title_stripped]
            .into_iter()
            .flatten()
            .map(|candidate| candidate.trim())
            .find(|candidate| !candidate.is_empty())
            .unwrap_or(&self.pane_id)
            .to_string()
    }
}

fn to_session(snapshot: Snapshot) -> Session {
    let workspaces = snapshot
        .workspaces
        .iter()
        .map(|workspace| Workspace {
            id: workspace.workspace_id.clone(),
            label: workspace.label.clone(),
            tabs: snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace.workspace_id)
                .map(|tab| Tab {
                    id: tab.tab_id.clone(),
                    label: tab.label.clone(),
                    panes: snapshot
                        .panes
                        .iter()
                        .filter(|pane| pane.tab_id == tab.tab_id)
                        .map(|pane| Pane {
                            id: pane.pane_id.clone(),
                            label: pane.label(),
                            agent: pane.agent.clone(),
                            state: pane.agent_status.clone(),
                        })
                        .collect(),
                })
                .filter(|tab| !tab.panes.is_empty())
                .collect(),
        })
        // A workspace with nothing to open is a dead row on a phone screen, the
        // same reason an empty tab is dropped one level down.
        .filter(|workspace| !workspace.tabs.is_empty())
        .collect();
    Session { workspaces }
}

async fn snapshot() -> Result<Snapshot> {
    let result = call("session.snapshot", json!({})).await?;
    let snapshot = result
        .get("snapshot")
        .context("session.snapshot returned no snapshot")?;
    Ok(serde_json::from_value(snapshot.clone())?)
}

pub async fn session() -> Result<Session> {
    Ok(to_session(snapshot().await?))
}

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

/// A pane rendered into twenty columns destroys anything it draws. Zooming
/// hands it the whole herdr window instead — as wide as the operator keeps that
/// window, which is all this can do and why nothing here expects a width.
pub async fn zoom(pane_id: &str, on: bool) -> Result<()> {
    call("pane.zoom", json!({ "pane_id": pane_id, "zoomed": on })).await?;
    Ok(())
}

/// `truncated` means herdr had more scrollback than `lines` asked for, which is
/// what lets the phone offer to reach further back.
#[derive(Deserialize)]
pub struct Output {
    pub text: String,
    pub truncated: bool,
}

/// The pane's output, ANSI already stripped by herdr. `lines` bounds what a
/// phone has to download and paint.
///
/// `source` matters more than it looks. `recent` is the scrollback a shell pane
/// wants, but for a pane running a full-screen TUI it returns redraw history
/// that churns on every read even when the screen is unchanged — a log that
/// never settles. `visible` returns just the current screen, byte-identical
/// between reads, which is the whole of what such a pane has anyway.
pub async fn read(pane_id: &str, lines: u32, source: &str) -> Result<Output> {
    let result = call(
        "pane.read",
        json!({ "pane_id": pane_id, "source": source, "lines": lines }),
    )
    .await?;
    let read = result.get("read").context("pane.read returned no read")?;
    Ok(serde_json::from_value(read.clone())?)
}

/// A bare keypress for what a prompt cannot express: esc to stop a turn, enter
/// to answer a question the agent is already showing, up and down to move the
/// selection in it.
///
/// Addressed to the *agent*, not to the pane. `pane.send_keys` would deliver
/// the key wherever that pane happens to point by the time it arrives, and a
/// pane whose agent exited in that window is a shell that would execute
/// whatever sits on its command line. herdr resolves an `agent.send_keys`
/// target atomically and answers `agent_not_found` instead, so the caller's
/// own agent check cannot go stale between the check and the key.
pub async fn press(pane_id: &str, key: &str) -> Result<()> {
    call(
        "agent.send_keys",
        json!({ "target": pane_id, "keys": [key] }),
    )
    .await?;
    Ok(())
}

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

#[cfg(test)]
pub(crate) fn fake_herdr(
    replies: Vec<&'static [u8]>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::UnixListener;

    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "herdr-remote-recording-test-{}-{n}.sock",
        std::process::id()
    ));
    let path = path.to_str().unwrap().to_string();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let socket = path.clone();
    let (sent, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for reply in replies {
            let (stream, _) = listener.accept().await.unwrap();
            let mut connection = BufReader::new(stream);
            let mut request = String::new();
            connection.read_line(&mut request).await.unwrap();
            let _ = sent.send(serde_json::from_str(&request).unwrap());
            connection.get_mut().write_all(reply).await.unwrap();
        }
        let _ = std::fs::remove_file(&socket);
    });
    (path, received)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_fixture() -> Snapshot {
        serde_json::from_value(json!({
            "workspaces": [
                { "workspace_id": "w1", "label": "backend" },
                { "workspace_id": "w2", "label": "frontend" },
                { "workspace_id": "w3", "label": "empty workspace" }
            ],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "backend" },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "empty" },
                { "tab_id": "w2:t1", "workspace_id": "w2", "label": "web" },
                { "tab_id": "w3:t1", "workspace_id": "w3", "label": "orphan" }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "claude",
                  "agent_status": "idle", "title": "orchestrator" },
                { "pane_id": "w1:p2", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "unknown", "terminal_title_stripped": "  " },
                { "pane_id": "w1:p4", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "idle", "label": "renamed",
                  "title": "ignored terminal title" },
                { "pane_id": "w2:p1", "tab_id": "w2:t1", "agent": "codex",
                  "agent_status": "working", "title": "ui" },
                { "pane_id": "w1:p3", "tab_id": "w1:tX", "agent": null,
                  "agent_status": "unknown" }
            ]
        }))
        .unwrap()
    }

    /// A herdr that accepts one connection, reads the request, replies with
    /// fixed bytes, hangs up, and removes its socket.
    fn fake_herdr(reply: &'static [u8]) -> String {
        let (path, _) = super::fake_herdr(vec![reply]);
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
        assert!(error.to_string().contains("socket closed before replying"));
    }

    #[tokio::test]
    async fn a_frame_without_its_newline_is_truncated() {
        let path = fake_herdr(b"{\"id\":\"herdr-remote\",\"result\":{}}");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("reply was truncated"));
    }

    #[tokio::test]
    async fn a_herdr_error_is_surfaced() {
        let path =
            fake_herdr(b"{\"id\":\"herdr-remote\",\"error\":{\"code\":\"x\",\"message\":\"y\"}}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("herdr ping failed"));
    }

    #[tokio::test]
    async fn a_reply_with_a_foreign_id_is_rejected() {
        let path = fake_herdr(b"{\"id\":\"someone-else\",\"result\":{}}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("reply carried a foreign id"));
    }

    #[tokio::test]
    async fn a_reply_missing_its_result_is_rejected() {
        let path = fake_herdr(b"{\"id\":\"herdr-remote\"}\n");
        let error = probe(&path).await.unwrap_err();
        assert!(error.to_string().contains("herdr ping returned no result"));
    }

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

    #[test]
    fn groups_panes_under_their_tab_and_workspace() {
        let session = to_session(snapshot_fixture());

        // The workspace whose only tab holds no pane is dropped, as is the
        // empty tab inside a workspace that survives.
        assert_eq!(session.workspaces.len(), 2);
        let backend = &session.workspaces[0];
        assert_eq!(backend.id, "w1");
        assert_eq!(backend.label, "backend");
        assert_eq!(backend.tabs.len(), 1);

        let tab = &backend.tabs[0];
        assert_eq!(tab.id, "w1:t1");
        // The pane whose tab is not listed is still dropped.
        assert_eq!(tab.panes.len(), 3);
        assert_eq!(
            tab.panes[0],
            Pane {
                id: "w1:p1".into(),
                label: "orchestrator".into(),
                agent: Some("claude".into()),
                state: "idle".into(),
            }
        );
        // Blank titles fall through to the pane id rather than rendering empty.
        assert_eq!(tab.panes[1].label, "w1:p2");
        assert_eq!(tab.panes[1].agent, None);
        // A herdr rename wins over the terminal title.
        assert_eq!(tab.panes[2].label, "renamed");

        // A tab belongs to exactly one workspace: w2's tab is not under w1.
        let frontend = &session.workspaces[1];
        assert_eq!(frontend.id, "w2");
        assert_eq!(frontend.tabs.len(), 1);
        assert_eq!(frontend.tabs[0].panes.len(), 1);
        assert_eq!(frontend.tabs[0].panes[0].id, "w2:p1");
    }
}
