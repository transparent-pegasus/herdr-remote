//! The environment blocks every agent staples onto a user turn. What the person
//! actually typed is either inside `<user_query>` or is what remains once the
//! injected blocks are removed.

/// `<user_query>` wins when present: cursor and grok wrap the real prompt in it
/// and surround it with several kilobytes of environment.
pub fn strip(text: &str) -> String {
    if let Some(inner) = between(text, "<user_query>", "</user_query>") {
        return inner.trim().to_string();
    }
    // A slash command's turn is nothing but its own tags. What the person
    // typed is the name and the arguments; the rest is the harness talking.
    if text.trim_start().starts_with("<command-name>") {
        return command(text);
    }
    let mut out = text.to_string();
    for (open, close) in [
        ("<system-reminder>", "</system-reminder>"),
        ("<task-notification>", "</task-notification>"),
        ("<user_info>", "</user_info>"),
        ("<timestamp>", "</timestamp>"),
    ] {
        out = drop_blocks(&out, open, close);
    }
    out.trim().to_string()
}

fn command(text: &str) -> String {
    let name = between(text, "<command-name>", "</command-name>").unwrap_or_default();
    let args = between(text, "<command-args>", "</command-args>").unwrap_or_default();
    format!("{} {}", name.trim(), args.trim())
        .trim()
        .to_string()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Every occurrence, not just the first: a turn can carry several reminders.
fn drop_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        match after.find(close) {
            Some(end) => rest = &after[end + close.len()..],
            // An unterminated block swallows the rest of the turn; dropping it
            // is better than pasting a half-open tag into the card.
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_query_block_is_the_whole_message() {
        let text = "<user_info>OS: linux</user_info>\n<user_query>\nfix the wrap\n</user_query>";
        assert_eq!(strip(text), "fix the wrap");
    }

    #[test]
    fn injected_blocks_are_removed_when_there_is_no_user_query() {
        let text = "read this\n<system-reminder>ignore me</system-reminder>\nand that";
        assert_eq!(strip(text), "read this\n\nand that");
    }

    #[test]
    fn every_occurrence_goes_not_just_the_first() {
        let text = "<system-reminder>a</system-reminder>keep<system-reminder>b</system-reminder>";
        assert_eq!(strip(text), "keep");
    }

    #[test]
    fn an_unterminated_block_does_not_leak_its_tag() {
        let text = "keep this\n<system-reminder>truncated…";
        assert_eq!(strip(text), "keep this");
    }

    #[test]
    fn a_slash_command_shows_as_what_was_typed() {
        let text = "<command-name>/model</command-name>\n  <command-message>model</command-message>\n  <command-args>claude-opus-5</command-args>";
        assert_eq!(strip(text), "/model claude-opus-5");
    }

    #[test]
    fn a_slash_command_without_arguments_is_just_its_name() {
        let text = "<command-name>/clear</command-name>\n  <command-message>clear</command-message>\n  <command-args></command-args>";
        assert_eq!(strip(text), "/clear");
    }

    /// Quoting the tags is not running the command: the prose has to survive.
    #[test]
    fn a_message_that_quotes_the_tags_keeps_its_prose() {
        let text = "`/clear` prints\n<command-name>/clear</command-name>\nwhy?";
        assert!(strip(text).starts_with("`/clear` prints"));
    }

    #[test]
    fn plain_text_survives_untouched() {
        assert_eq!(strip("  just a prompt  "), "just a prompt");
    }
}
