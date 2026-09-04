//! Claude Code's transcript: one JSON object per line under
//! `~/.claude/projects/<slug>/<session>.jsonl`.

use serde_json::Value;

use super::{Message, Parsed, Role, preamble};

/// `None` for every line that is not a speaker: tool calls, tool results,
/// thinking, the attachments that are not a message, and the session's
/// bookkeeping rows.
pub fn parse_line(line: &str, seq: u64) -> Option<Parsed> {
    let value: Value = serde_json::from_str(line).ok()?;
    // `isMeta` marks what the harness injected under the user's name: the
    // local-command caveat, a slash command's expanded body, a nudge to
    // continue. None of it is anyone speaking.
    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let role = match value.get("type")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        // Words typed while the agent was mid-turn are never a `user` row:
        // Claude Code files them as a `queued_command` attachment at the point
        // the agent picked them up. Skipping it drops the message from the
        // history and leaves the phone's own copy of what it sent waiting for
        // a turn the file will never hold.
        "attachment" => return queued(value.get("attachment")?, seq).map(Parsed::Message),
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
    // A local command's output is not the user speaking: it is the answer to
    // the command above it, and belongs to the card that shows the command.
    if role == Role::User
        && let Some(output) = preamble::command_output(&joined)
    {
        return (!output.is_empty()).then_some(Parsed::Output(output));
    }
    // Only a user turn carries injected blocks. An assistant that quotes
    // `<system-reminder>` in its prose keeps every character.
    let text = if role == Role::User {
        preamble::strip(&joined)
    } else {
        joined
    };

    (!text.trim().is_empty()).then_some(Parsed::Message(Message {
        seq,
        role,
        text,
        output: None,
    }))
}

fn queued(attachment: &Value, seq: u64) -> Option<Message> {
    if attachment.get("type")?.as_str()? != "queued_command" {
        return None;
    }
    let text = preamble::strip(attachment.get("prompt")?.as_str()?);
    (!text.trim().is_empty()).then_some(Message {
        seq,
        role: Role::User,
        text,
        output: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test but the output ones is about a speaker's own message.
    fn message(line: &str, seq: u64) -> Option<Message> {
        match parse_line(line, seq) {
            Some(Parsed::Message(message)) => Some(message),
            _ => None,
        }
    }

    #[test]
    fn a_string_user_turn_is_a_message() {
        let line = r#"{"type":"user","message":{"content":"fix the wrap"}}"#;
        let message = message(line, 7).unwrap();
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
        let message = message(line, 1).unwrap();
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.text, "first\nsecond");
    }

    #[test]
    fn a_tool_result_turn_is_not_a_message() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"x","content":"output"}]}}"#;
        assert!(parse_line(line, 1).is_none());
    }

    /// The queue is where a message sent from the phone mid-turn lands, and
    /// its position in the file is where the agent took it — which is where it
    /// belongs in the history.
    #[test]
    fn a_queued_command_is_the_user_speaking() {
        let line = r#"{"type":"attachment","attachment":{
            "type":"queued_command","prompt":"and centre the sheet"}}"#;
        let message = message(line, 4).unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.text, "and centre the sheet");
        assert_eq!(message.seq, 4);
    }

    #[test]
    fn bookkeeping_rows_are_skipped() {
        for line in [
            r#"{"type":"attachment","sessionId":"s"}"#,
            r#"{"type":"attachment","attachment":{"type":"selected_lines","prompt":"x"}}"#,
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":
                "<local-command-caveat>Caveat: …</local-command-caveat>"}}"#,
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
            message(line, 1).unwrap().text,
            "write <system-reminder>x</system-reminder> literally"
        );
    }

    /// The caveat row is dropped; the row after it is the command itself.
    #[test]
    fn a_slash_command_is_the_user_speaking() {
        let line = r#"{"type":"user","message":{"content":
            "<command-name>/clear</command-name>\n  <command-args></command-args>"}}"#;
        let message = message(line, 2).unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.text, "/clear");
    }

    /// The command's own answer is written as a turn, but it is the harness
    /// speaking, and it belongs to the card that shows the command.
    #[test]
    fn a_commands_output_is_the_tail_of_the_command_not_a_turn_of_its_own() {
        let line = r#"{"type":"user","message":{"content":
            "<local-command-stdout>Set model to Opus 5</local-command-stdout>"}}"#;
        let Some(Parsed::Output(output)) = parse_line(line, 3) else {
            panic!("not output")
        };
        assert_eq!(output, "Set model to Opus 5");
    }

    #[test]
    fn an_empty_output_is_nothing_at_all() {
        let line = r#"{"type":"user","message":{"content":
            "<local-command-stdout></local-command-stdout>"}}"#;
        assert!(parse_line(line, 1).is_none());
    }

    #[test]
    fn a_user_turn_loses_its_reminders() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"text","text":"do it<system-reminder>noise</system-reminder>"}]}}"#;
        assert_eq!(message(line, 1).unwrap().text, "do it");
    }
}
