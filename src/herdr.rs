//! Thin client for the Herdr Unix socket: newline-delimited JSON, one request
//! per connection. Protocol shapes come from `herdr api schema --json`.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn socket_path() -> String {
    std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config/herdr/herdr.sock")
    })
}

/// Send one request, return its `result`.
pub async fn call(method: &str, params: Value) -> Result<Value> {
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("connect to herdr socket at {path}"))?;
    let mut conn = BufReader::new(stream);

    let request = json!({ "id": "herdr-remote", "method": method, "params": params });
    conn.get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await?;

    let mut line = String::new();
    conn.read_line(&mut line).await?;
    let response: Value = serde_json::from_str(&line)
        .with_context(|| format!("herdr {method} returned non-JSON: {line}"))?;

    if let Some(error) = response.get("error") {
        bail!("herdr {method} failed: {error}");
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
    /// Panes usually carry no label; fall back to the terminal title, then the id.
    fn label(&self) -> String {
        [&self.title, &self.terminal_title_stripped]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
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

/// `None` when no such pane exists, so the caller can answer 404.
pub async fn pane_has_agent(pane_id: &str) -> Result<Option<bool>> {
    Ok(snapshot()
        .await?
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .map(|pane| pane.agent.is_some()))
}

/// Agent panes take a prompt; a plain shell needs the text plus a newline.
pub async fn prompt(pane_id: &str, text: &str, has_agent: bool) -> Result<()> {
    if has_agent {
        call("agent.prompt", json!({ "target": pane_id, "text": text })).await?;
    } else {
        call(
            "pane.send_input",
            json!({ "pane_id": pane_id, "text": text, "keys": ["enter"] }),
        )
        .await?;
    }
    Ok(())
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
                { "pane_id": "w1:p3", "tab_id": "w1:tX", "agent": null,
                  "agent_status": "unknown" }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn groups_panes_under_their_tab() {
        let session = to_session(snapshot_fixture());

        // The empty tab is dropped, and so is the pane whose tab is not listed.
        assert_eq!(session.tabs.len(), 1);
        let tab = &session.tabs[0];
        assert_eq!(tab.id, "w1:t1");
        assert_eq!(tab.panes.len(), 2);

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
    }
}
