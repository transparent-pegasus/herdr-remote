//! Cursor's store: a SQLite blob table whose root blob lists the conversation's
//! message ids in order. The ids herdr reports do not address this store, so the
//! caller resolves the directory by cwd and title before handing over the path.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{Message, Role, preamble};

/// Open the path directly: URI parsing decodes `%HH` in path components after
/// the caller's boundary check. Read-only is enough, but `immutable=1` must
/// never come back: a live session keeps a `-wal` file, and an immutable open
/// reads the pre-WAL pages and finds no tables at all.
fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
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
        .query_row("select data from blobs where id = ?1", [id], |row| {
            row.get(0)
        })
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
    (!text.trim().is_empty()).then_some(Message { seq, role, text })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    text.len()
        .is_multiple_of(2)
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
            .execute("insert into meta values ('0', ?1)", [hex(meta.as_bytes())])
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

    #[test]
    fn a_percent_encoded_parent_cannot_escape_the_cursor_root() {
        let home = tempdir();
        let chats = home.join(".cursor/chats");
        let claimed = chats.join("%2e%2e/store.db");
        std::fs::create_dir_all(claimed.parent().unwrap()).unwrap();
        std::fs::write(&claimed, []).unwrap();

        let outside = home.join(".cursor");
        let outside_store = store(&outside, &[("assistant", "outside")]);
        assert_eq!(outside_store, outside.join("store.db"));

        let guarded = super::super::guard("cursor", claimed, &home).unwrap();
        assert!(read(&guarded).is_err());
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
