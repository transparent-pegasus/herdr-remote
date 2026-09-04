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
    (!text.trim().is_empty()).then_some(Message {
        seq,
        role,
        text,
        output: None,
    })
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
