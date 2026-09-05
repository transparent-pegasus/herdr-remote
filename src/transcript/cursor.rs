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

/// Read a protobuf varint without accepting truncation or overflowing u64.
fn varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = bytes
            .split_first()
            .context("cursor root blob ends mid-varint")?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            bail!("cursor root blob varint overflows");
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
    }
    bail!("cursor root blob varint overflows")
}

/// Field 1 holds 32-byte message ids. Other fields hold session metadata and
/// are skipped by their wire type, including multibyte tags and lengths.
///
/// Framing that does not parse to the end is an error rather than a shorter
/// list: a truncated read would render as a complete conversation that is
/// quietly missing its most recent messages.
fn blob_ids(root: &[u8]) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut rest = root;
    while !rest.is_empty() {
        let tag = varint(&mut rest)?;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == 0 || field > 0x1fff_ffff || (field == 1 && wire != 2) {
            bail!("cursor root blob has an invalid field");
        }
        let len = match wire {
            0 => {
                varint(&mut rest)?;
                continue;
            }
            1 => 8,
            2 => usize::try_from(varint(&mut rest)?).context("cursor root field is too large")?,
            5 => 4,
            _ => bail!("cursor root blob has an unsupported wire type"),
        };
        let (bytes, tail) = rest
            .split_at_checked(len)
            .context("cursor root blob ends mid-field")?;
        if field == 1 {
            if len != 32 {
                bail!("cursor message id is not 32 bytes");
            }
            ids.push(hex(bytes));
        }
        rest = tail;
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
    (!text.trim().is_empty()).then_some(Message {
        seq,
        role,
        text,
        output: None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let mut chunks = text.as_bytes().chunks_exact(2);
    let decoded = chunks
        .by_ref()
        .map(|pair| {
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    chunks.remainder().is_empty().then_some(decoded)
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

    fn root_with_metadata(first: [u8; 32], second: [u8; 32]) -> Vec<u8> {
        // Synthetic examples of observed fields 5, 8, 10, 18 and 26, plus
        // fixed-width metadata. Only field 1 contains message references.
        let mut root = vec![0x2a, 0xb1, 0x02]; // field 5, 305 bytes
        root.extend([0x6d; 305]);
        root.extend([0x0a, 32]);
        root.extend(first);
        root.push(0x11); // field 2, fixed64
        root.extend([0xff; 8]);
        root.push(0x1d); // field 3, fixed32
        root.extend([0xff; 4]);
        root.extend([0x42, 32]); // field 8, not a message reference
        root.extend([0xee; 32]);
        root.extend([0x50, 0x01]); // field 10, varint 1
        root.extend([0x92, 0x01, 3, 0, 0xff, 0x80]); // field 18, three bytes
        root.extend([0x0a, 32]);
        root.extend(second);
        root.extend([0xd0, 0x01, 0x80, 0xd0, 0x95, 0xff, 0xbc, 0x31]); // field 26, timestamp
        root
    }

    #[test]
    fn root_metadata_preserves_message_reference_order() {
        let root = root_with_metadata([0x22; 32], [0x11; 32]);
        assert_eq!(
            blob_ids(&root).unwrap(),
            vec!["22".repeat(32), "11".repeat(32)]
        );
    }

    #[test]
    fn root_metadata_keeps_visible_messages_when_reading_the_store() {
        let dir = tempdir();
        let path = store(
            &dir,
            &[("user", "first question"), ("assistant", "second answer")],
        );
        let mut second = [0; 32];
        second[31] = 1;
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "update blobs set data = ?1 where id = ?2",
                rusqlite::params![root_with_metadata([0; 32], second), format!("{:064x}", 999)],
            )
            .unwrap();
        drop(connection);

        let (_, messages) = read(&path).unwrap();
        assert_eq!(
            messages,
            vec![
                Message {
                    seq: 0,
                    role: Role::User,
                    text: "first question".into(),
                    output: None
                },
                Message {
                    seq: 1,
                    role: Role::Assistant,
                    text: "second answer".into(),
                    output: None
                },
            ]
        );
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
    fn message_references_require_exactly_32_bytes() {
        for length in [0, 1, 31, 33, 127] {
            let mut root = vec![0x0a, length];
            root.extend(vec![0x11; length as usize]);
            assert!(
                blob_ids(&root).is_err(),
                "accepted a {length}-byte reference"
            );
        }
    }

    #[test]
    fn message_references_reject_other_wire_types() {
        for root in [
            vec![0x08, 1],
            vec![0x09, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![0x0d, 0, 0, 0, 0],
        ] {
            assert!(blob_ids(&root).is_err(), "accepted {root:?}");
        }
    }

    #[test]
    fn malformed_metadata_cannot_silently_shorten_the_conversation() {
        let cases = [
            ("truncated tag", vec![0x80]),
            ("unterminated tag", vec![0x80; 10]),
            ("overflowing tag", [vec![0xff; 9], vec![2]].concat()),
            ("missing varint", vec![0x50]),
            ("truncated varint", vec![0x50, 0x80]),
            ("unterminated varint", [vec![0x50], vec![0x80; 10]].concat()),
            (
                "overflowing varint",
                [vec![0x50], vec![0xff; 9], vec![2]].concat(),
            ),
            ("truncated fixed64", vec![0x11, 0, 0, 0, 0, 0, 0, 0]),
            ("truncated fixed32", vec![0x1d, 0, 0, 0]),
            ("missing length", vec![0x2a]),
            ("truncated length", vec![0x2a, 0x80]),
            (
                "overflowing length",
                [vec![0x2a], vec![0xff; 9], vec![2]].concat(),
            ),
            (
                "truncated long metadata",
                [vec![0x2a, 0x81, 0x01], vec![0; 128]].concat(),
            ),
            ("zero tag", vec![0]),
            ("field zero", vec![0x02, 0]),
            (
                "field number too large",
                vec![0x80, 0x80, 0x80, 0x80, 0x10, 0],
            ),
            ("wire type 6", vec![0x16]),
            ("wire type 7", vec![0x17]),
            ("unsupported group", vec![0x13, 0x14]),
            ("unmatched end group", vec![0x14]),
        ];
        for (label, malformed) in cases {
            let mut root = vec![0x0a, 32];
            root.extend([0x11; 32]);
            root.extend(malformed);
            assert!(blob_ids(&root).is_err(), "accepted {label}");
        }
    }

    #[test]
    fn malformed_and_non_ascii_hex_is_refused() {
        for value in ["aéa", "éé", "0g", "f", "💣"] {
            assert_eq!(decode_hex(value), None, "{value:?}");
        }
    }

    #[test]
    fn malformed_meta_returns_an_error_without_unwinding() {
        let dir = tempdir();
        let path = store(&dir, &[("user", "hello")]);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("update meta set value = 'aéa'", [])
            .unwrap();
        drop(connection);

        let outcome = std::panic::catch_unwind(|| read(&path));
        assert!(matches!(outcome, Ok(Err(_))));
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
