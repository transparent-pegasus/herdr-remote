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
    // A `!` command's turn is nothing but its own tag, and `!` is what the
    // person pressed to open it — the same line the harness itself shows back.
    if let Some(typed) = tagged(text, "bash-input") {
        return format!("! {}", typed.trim());
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

/// A local command's output is a turn of its own whose whole body is the tag,
/// so only a text that is nothing else is one. Terminal colour comes with it,
/// and nothing on the phone renders it.
pub fn command_output(text: &str) -> Option<String> {
    if let Some(body) = tagged(text, "local-command-stdout") {
        return Some(without_ansi(body).trim().to_string());
    }
    bash_output(text.trim())
}

/// A `!` command answers in two streams and Claude Code files both in one turn,
/// escaping `&`, `<` and `>` on the way in. Both belong to the command above,
/// and the rule between them is what stops the second reading as more of the
/// first.
fn bash_output(text: &str) -> Option<String> {
    // The same guard the local-command tag carries: a turn that only begins
    // with the tag is prose about it, and keeps every character.
    if !text.starts_with("<bash-stdout>")
        || !(text.ends_with("</bash-stderr>") || text.ends_with("</bash-stdout>"))
    {
        return None;
    }
    let stream = |open: &str, close: &str| {
        let raw = between(text, open, close).unwrap_or_default();
        without_ansi(&unescape(raw)).trim().to_string()
    };
    let streams = [
        stream("<bash-stdout>", "</bash-stdout>"),
        stream("<bash-stderr>", "</bash-stderr>"),
    ];
    Some(
        streams
            .into_iter()
            .filter(|stream| !stream.is_empty())
            .collect::<Vec<_>>()
            .join("\n---\n"),
    )
}

/// The three the writer escapes, and only those. `&amp;` goes last: decoding it
/// first would turn an escaped `&amp;lt;` into a `<` its writer never typed.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The body of a turn that is one tag and nothing else. Prose that merely
/// quotes the tag is a person writing about it, and keeps every character.
fn tagged<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    text.trim()
        .strip_prefix(&format!("<{tag}>"))?
        .strip_suffix(&format!("</{tag}>"))
}

/// CSI sequences only, which is all a command's own output carries.
fn without_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        if characters.peek() == Some(&'[') {
            characters.next();
            // A CSI ends at its final byte, the first one in `@`..=`~`.
            for character in characters.by_ref() {
                if ('@'..='~').contains(&character) {
                    break;
                }
            }
        }
    }
    out
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
    fn a_local_command_writes_its_output_as_its_own_turn() {
        let text =
            "<local-command-stdout>Set model to \u{1b}[1mOpus 5\u{1b}[22m</local-command-stdout>";
        assert_eq!(command_output(text).unwrap(), "Set model to Opus 5");
    }

    #[test]
    fn prose_that_merely_mentions_the_tag_is_not_output() {
        assert!(command_output("see <local-command-stdout> in the file").is_none());
        assert!(command_output("plain words").is_none());
    }

    #[test]
    fn a_bash_command_reads_the_way_it_was_typed() {
        assert_eq!(strip("<bash-input>gpso main</bash-input>"), "! gpso main");
    }

    /// Same rule the slash command's tags follow: quoting one is not running it.
    #[test]
    fn prose_that_quotes_the_bash_tag_keeps_its_words() {
        let text = "`<bash-input>` is where a `!` command lands";
        assert_eq!(strip(text), text);
    }

    #[test]
    fn a_bash_command_hands_its_streams_to_the_command_above() {
        let text = "<bash-stdout>b5d0e18..314abca  main -&gt; main</bash-stdout>\
<bash-stderr></bash-stderr>";
        assert_eq!(
            command_output(text).unwrap(),
            "b5d0e18..314abca  main -> main"
        );
    }

    #[test]
    fn two_streams_are_told_apart_by_a_rule_between_them() {
        let text = "<bash-stdout>done</bash-stdout><bash-stderr>warning: slow</bash-stderr>";
        assert_eq!(command_output(text).unwrap(), "done\n---\nwarning: slow");
    }

    /// Only stderr spoke, so there is nothing for a rule to separate.
    #[test]
    fn a_stream_that_stayed_quiet_leaves_no_rule_behind() {
        let text =
            "<bash-stdout></bash-stdout><bash-stderr>zsh: command not found: pso\n</bash-stderr>";
        assert_eq!(command_output(text).unwrap(), "zsh: command not found: pso");
    }

    /// The writer escapes `&` before `<`, so an `&lt;` its author typed arrives
    /// as `&amp;lt;` and has to come back as `&lt;`, not as `<`.
    #[test]
    fn an_escaped_ampersand_decodes_once_not_twice() {
        let text = "<bash-stdout>echo &amp;lt;a&amp;gt; &amp;amp; true</bash-stdout><bash-stderr></bash-stderr>";
        assert_eq!(command_output(text).unwrap(), "echo &lt;a&gt; &amp; true");
    }

    #[test]
    fn prose_that_opens_with_the_stream_tag_is_still_prose() {
        assert!(command_output("<bash-stdout>x</bash-stdout> is where it lands").is_none());
    }

    #[test]
    fn a_command_that_said_nothing_at_all_is_nothing_at_all() {
        let text = "<bash-stdout></bash-stdout><bash-stderr></bash-stderr>";
        assert_eq!(command_output(text).unwrap(), "");
    }

    #[test]
    fn plain_text_survives_untouched() {
        assert_eq!(strip("  just a prompt  "), "just a prompt");
    }
}
