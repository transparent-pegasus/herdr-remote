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

    (!text.trim().is_empty()).then_some(Message { seq, role, text })
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
