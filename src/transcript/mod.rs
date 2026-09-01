//! An agent's own transcript, which holds what the terminal threw away: a pane
//! running on the alternate screen keeps no scrollback, so the file is the only
//! place its finished answers still exist.

mod claude;
mod codex;
mod cursor;
mod grok;
mod preamble;

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

use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

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

/// What a transcript file for one agent is allowed to be called.
type Shape = fn(&str) -> bool;

/// Where each agent's transcript is allowed to be, and what it must be called.
/// Pairing the root with the shape is what stops a reported path from naming an
/// arbitrary file under someone else's root and having it read as this agent's
/// history.
fn allowed(agent: &str) -> Option<(&'static str, Shape)> {
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
    let real = within(&home.join(root), path)?;
    let name = real.file_name().and_then(|name| name.to_str())?;
    shaped(name).then_some(real)
}

fn within(root: &Path, path: PathBuf) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let real = path.canonicalize().ok()?;
    real.starts_with(root).then_some(real)
}

/// Claude names a project directory after its cwd, with `/` and `.` replaced.
fn claude_slug(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

pub fn resolve(pane: &PaneRef, home: &Path) -> Option<Source> {
    if home.as_os_str().is_empty() {
        return None;
    }
    if pane.session_kind == Some("path") {
        let path = guard(pane.agent, PathBuf::from(pane.session_value?), home)?;
        return source_for(pane.agent, path);
    }
    match pane.agent {
        "claude" => {
            let projects = home.join(".claude/projects");
            let by_cwd = projects.join(claude_slug(pane.cwd));
            let path = match pane.session_value {
                Some(id) => {
                    guard("claude", by_cwd.join(format!("{id}.jsonl")), home).or_else(|| {
                        let wanted = format!("{id}.jsonl");
                        files(&projects, 2)
                            .into_iter()
                            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some(&wanted))
                            .and_then(|path| guard("claude", path, home))
                    })
                }
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
                        .filter(|path| {
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|name| name.ends_with(&wanted))
                        })
                        .max()
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
        "claude" => Some(Source::Lines {
            path,
            format: Format::Claude,
        }),
        "codex" => Some(Source::Lines {
            path,
            format: Format::Codex,
        }),
        "grok" => Some(Source::Lines {
            path,
            format: Format::Grok,
        }),
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

fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
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
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
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
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
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
        .filter(|path| real_dir(path))
        .max_by_key(|path| modified(path))
}

/// Read one `meta.json`, or `None` when it is missing or unreadable.
fn cursor_meta(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// A reported Cursor id is a directory name one level below a project. Join
/// that exact name under each project instead of walking every chat file.
fn cursor_created_at(chats: &Path, id: &str) -> Option<u64> {
    let mut components = Path::new(id).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return None;
    }
    std::fs::read_dir(chats)
        .ok()?
        .flatten()
        .map(|project| project.path())
        .filter(|project| real_dir(project))
        .map(|project| project.join(id))
        .filter(|dir| real_dir(dir))
        .find_map(|dir| {
            let meta = within(chats, dir.join("meta.json"))?;
            cursor_meta(&meta)?
                .get("createdAtMs")
                .and_then(|value| value.as_u64())
        })
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
    let floor = match session {
        Some(id) => cursor_created_at(&chats, id)?,
        None => 0,
    };

    let mut best: Option<(bool, u64, PathBuf)> = None;
    for project in std::fs::read_dir(&chats).ok()?.flatten() {
        let project = project.path();
        if !real_dir(&project) {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(project) else {
            continue;
        };
        for session in sessions.flatten() {
            let dir = session.path();
            if !real_dir(&dir) {
                continue;
            }
            let Some(meta) = cursor_meta(&dir.join("meta.json")) else {
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
            let store = dir.join("store.db");
            if !store.is_file() {
                continue;
            }
            let titled = title.is_some() && meta.get("title").and_then(|v| v.as_str()) == title;
            let candidate = (titled, updated, store);
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
    fn directory_walks_do_not_follow_symlinked_directories() {
        let home = scratch();
        let root = home.join("root");
        let inside = root.join("real/inside.jsonl");
        let outside = home.join("outside/secret.jsonl");
        touch(&inside);
        touch(&outside);
        std::os::unix::fs::symlink(outside.parent().unwrap(), root.join("linked")).unwrap();

        let walked = files(&root, 2);
        assert!(walked.contains(&inside));
        assert!(
            !walked
                .iter()
                .any(|path| path.starts_with(root.join("linked")))
        );
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
                session_kind: None,
                session_value: None,
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
    fn a_reported_cursor_id_without_readable_meta_fails_closed() {
        for meta in [None, Some("not json")] {
            let home = scratch();
            let candidate = home.join(".cursor/chats/p1/candidate");
            touch(&candidate.join("store.db"));
            std::fs::write(
                candidate.join("meta.json"),
                serde_json::json!({ "cwd": "/repo", "hasConversation": true,
                                    "updatedAtMs": 100u64 })
                .to_string(),
            )
            .unwrap();
            if let Some(meta) = meta {
                let reported = home.join(".cursor/chats/p1/reported");
                std::fs::create_dir_all(&reported).unwrap();
                std::fs::write(reported.join("meta.json"), meta).unwrap();
            }

            assert!(resolve(&pane("cursor", Some("reported"), "/repo"), &home).is_none());
        }
    }

    #[test]
    fn cursor_ids_cannot_escape_or_add_path_components() {
        let home = scratch();
        let chats = home.join(".cursor/chats");
        let project = chats.join("p1");

        let candidate = project.join("conversation");
        touch(&candidate.join("store.db"));
        std::fs::write(
            candidate.join("meta.json"),
            serde_json::json!({ "cwd": "/repo", "hasConversation": true,
                                "updatedAtMs": 2u64 })
            .to_string(),
        )
        .unwrap();

        let escaped = chats.join("escape");
        std::fs::create_dir_all(&escaped).unwrap();
        std::fs::write(
            escaped.join("meta.json"),
            serde_json::json!({ "createdAtMs": 1u64 }).to_string(),
        )
        .unwrap();

        let outside = home.join("outside/b");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("meta.json"),
            serde_json::json!({ "createdAtMs": 1u64 }).to_string(),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.parent().unwrap(), project.join("a")).unwrap();

        for id in ["../escape", "/etc", "a/b"] {
            assert!(
                resolve(&pane("cursor", Some(id), "/repo"), &home).is_none(),
                "accepted cursor id {id:?}"
            );
        }
    }

    #[test]
    fn cursor_skips_a_ranked_candidate_without_a_store() {
        let home = scratch();
        for (name, updated, store) in [("missing", 99_u64, false), ("real", 10, true)] {
            let dir = home.join(".cursor/chats/p1").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if store {
                touch(&dir.join("store.db"));
            }
            std::fs::write(
                dir.join("meta.json"),
                serde_json::json!({ "cwd": "/repo", "hasConversation": true,
                                    "updatedAtMs": updated })
                .to_string(),
            )
            .unwrap();
        }

        match resolve(&pane("cursor", None, "/repo"), &home).unwrap() {
            Source::CursorDb { path } => {
                assert!(path.to_string_lossy().contains("/real/"), "{path:?}")
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
    fn a_codex_id_resolves_to_its_newest_matching_rollout() {
        let home = scratch();
        let id = "same-id";
        let older = home
            .join(".codex/sessions/2026/08/31")
            .join(format!("rollout-2026-08-31T23-59-00-{id}.jsonl"));
        let newer = home
            .join(".codex/sessions/2026/09/01")
            .join(format!("rollout-2026-09-01T00-01-00-{id}.jsonl"));
        touch(&older);
        touch(&newer);

        match resolve(&pane("codex", Some(id), "/repo"), &home).unwrap() {
            Source::Lines { path, format } => {
                assert_eq!(format, Format::Codex);
                assert_eq!(path, newer.canonicalize().unwrap());
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

        // Parse into local state so an open, seek, or read failure leaves the
        // last successful snapshot intact and the same bytes eligible to retry.
        let reset = len < self.offset;
        let mut offset = if reset { 0 } else { self.offset };
        let mut next_seq = if reset { 0 } else { self.next_seq };
        let mut messages = Vec::new();

        let mut file =
            std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        file.seek(SeekFrom::Start(offset))?;
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
            offset += read as u64;
            let parsed = match format {
                Format::Claude => claude::parse_line(&line, next_seq),
                Format::Codex => codex::parse_line(&line, next_seq),
                Format::Grok => grok::parse_line(&line, next_seq),
            };
            if let Some(message) = parsed {
                messages.push(message);
                next_seq += 1;
            }
        }
        if reset {
            self.messages = messages;
        } else {
            self.messages.extend(messages);
        }
        self.offset = offset;
        self.next_seq = next_seq;
        self.len = len;
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

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
    fn a_failed_read_retries_the_same_bytes() {
        let path = scratch_file();
        append(&path, &user("first"));
        let mut transcript = Transcript::open(Source::Lines {
            path: path.clone(),
            format: Format::Claude,
        });
        transcript.refresh().unwrap();

        append(&path, &user("second"));
        let readable = std::fs::metadata(&path).unwrap().permissions();
        let mut unreadable = readable.clone();
        unreadable.set_mode(0o000);
        std::fs::set_permissions(&path, unreadable).unwrap();
        assert!(transcript.refresh().is_err());
        std::fs::set_permissions(&path, readable).unwrap();

        transcript.refresh().unwrap();
        assert_eq!(
            transcript
                .messages()
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
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
