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

fn socket_path() -> String {
    std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config/herdr/herdr.sock")
    })
}

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
    tabs: Vec<TabInfo>,
    panes: Vec<PaneInfo>,
}

#[derive(Deserialize)]
struct TabInfo {
    tab_id: String,
    label: String,
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
}

// --- what we expose, so a herdr schema change need not reach the phone ---

#[derive(Serialize, Debug, PartialEq)]
pub struct Session {
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
    let tabs = snapshot
        .tabs
        .iter()
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
        .collect();
    Session { tabs }
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

/// A bare keypress, straight to the terminal — the two keys the phone needs
/// that a prompt cannot express: esc to stop a turn, enter to answer a prompt
/// the agent is already showing. Callers pick the key; nothing else does.
pub async fn press(pane_id: &str, key: &str) -> Result<()> {
    call(
        "pane.send_keys",
        json!({ "pane_id": pane_id, "keys": [key] }),
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
mod tests {
    use super::*;

    fn snapshot_fixture() -> Snapshot {
        serde_json::from_value(json!({
            "tabs": [
                { "tab_id": "w1:t1", "label": "backend" },
                { "tab_id": "w1:t2", "label": "empty" }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "claude",
                  "agent_status": "idle", "title": "orchestrator" },
                { "pane_id": "w1:p2", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "unknown", "terminal_title_stripped": "  " },
                { "pane_id": "w1:p4", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "idle", "label": "renamed",
                  "title": "ignored terminal title" },
                { "pane_id": "w1:p3", "tab_id": "w1:tX", "agent": null,
                  "agent_status": "unknown" }
            ]
        }))
        .unwrap()
    }

    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::UnixListener;

    /// A herdr that accepts one connection, reads the request, replies with
    /// fixed bytes, hangs up, and removes its socket.
    fn fake_herdr(reply: &'static [u8]) -> String {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("herdr-remote-test-{}-{n}.sock", std::process::id()));
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

    #[test]
    fn groups_panes_under_their_tab() {
        let session = to_session(snapshot_fixture());

        // The empty tab is dropped, and so is the pane whose tab is not listed.
        assert_eq!(session.tabs.len(), 1);
        let tab = &session.tabs[0];
        assert_eq!(tab.id, "w1:t1");
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
    }
}
