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

    (!text.trim().is_empty()).then_some(Message { seq, role, text })
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
    fn a_task_notification_is_not_something_the_person_said() {
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"<task-notification>worker finished</task-notification>"}]}}"#;
        assert!(parse_line(line, 1).is_none());
    }

    #[test]
    fn a_task_notification_does_not_hide_real_user_text() {
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"keep this\n<task-notification>worker finished</task-notification>"}]}}"#;
        assert_eq!(parse_line(line, 1).unwrap().text, "keep this");
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
