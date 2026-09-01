//! The one line a transcript cannot hold: what the person has typed into the
//! agent's box but not yet sent.

/// What an empty box renders instead of nothing. Matching these is the only
/// way to tell "the person typed nothing" from "the person typed this".
const PLACEHOLDERS: [&str; 3] = ["Ask Codex to do", "Plan, search,", "Add a follow-up"];

/// The draft's first line, or `None` when the box is empty, shows a
/// placeholder, or no longer looks the way this agent's box looked. A wrong
/// string labelled as the user's draft is worse than a blank.
pub fn composer(agent: &str, screen: &str) -> Option<String> {
    let rows: Vec<&str> = screen.lines().collect();
    let line = match agent {
        // The same glyph prefixes messages already sent, echoed further up the
        // screen; the composer is always the last one. Claude draws rules around
        // its box and grok draws none, and neither fact matters here.
        "claude" | "grok" => last_starting_with(&rows, '❯')?,
        "codex" => last_starting_with(&rows, '›')?,
        // `→` shows up in ordinary output, so cursor's box is found by its
        // block borders rather than by the glyph.
        "cursor" => between_borders(&rows)?,
        _ => return None,
    };

    let draft = line.trim_start().trim_start_matches(['❯', '›', '→']).trim();
    let placeholder = PLACEHOLDERS
        .iter()
        .any(|candidate| draft.starts_with(candidate));
    (!draft.is_empty() && !placeholder).then(|| draft.to_string())
}

fn last_starting_with<'a>(rows: &[&'a str], glyph: char) -> Option<&'a str> {
    rows.iter()
        .rev()
        .find(|row| row.trim_start().starts_with(glyph))
        .copied()
}

/// A border row is three or more of one drawing character and nothing else.
fn border(row: &str, glyph: char) -> bool {
    let trimmed = row.trim();
    trimmed.chars().count() >= 3 && trimmed.chars().all(|c| c == glyph)
}

/// The first non-empty row inside the last box, whose top is `▄` and whose
/// bottom is `▀`. Requiring the orientation keeps a lone border, or two of the
/// same kind, from being read as a box.
fn between_borders<'a>(rows: &[&'a str]) -> Option<&'a str> {
    let bottom = rows.iter().rposition(|row| border(row, '▀'))?;
    let top = rows[..bottom].iter().rposition(|row| border(row, '▄'))?;
    let row = rows[top + 1..bottom]
        .iter()
        .find(|row| !row.trim().is_empty())
        .copied()?;
    row.trim_start().starts_with('→').then_some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = "\
❯ このメッセージは送信済みのエコー
  ⎿  tool output

────────────
❯ 打ちかけの下書き
────────────
  Opus 5 (1M context) xhigh";

    #[test]
    fn claude_takes_the_last_prompt_row_not_the_echoed_message() {
        assert_eq!(composer("claude", CLAUDE).unwrap(), "打ちかけの下書き");
    }

    #[test]
    fn an_empty_claude_box_is_blank() {
        let screen = "────────────\n❯\n────────────\n  Opus 5";
        assert!(composer("claude", screen).is_none());
    }

    /// Grok draws no rules at all, which is why nothing looks for them.
    #[test]
    fn grok_has_no_box_around_its_prompt_row() {
        let screen = "◆ session_start\nminimal · /help\n❯ 下書き\nGrok 4.6 (xhigh) · a";
        assert_eq!(composer("grok", screen).unwrap(), "下書き");
        assert!(composer("grok", "minimal · /help\n❯\nGrok 4.6").is_none());
    }

    #[test]
    fn only_the_first_row_of_a_wrapped_draft_is_taken() {
        let screen = "❯ DRAFT_PROBE_XY 下\n  書き\nGrok 4.6";
        assert_eq!(composer("grok", screen).unwrap(), "DRAFT_PROBE_XY 下");
    }

    #[test]
    fn codex_reads_its_prompt_row_and_ignores_the_placeholder() {
        assert_eq!(
            composer("codex", "› review the diff").unwrap(),
            "review the diff"
        );
        assert!(composer("codex", "› Ask Codex to do an").is_none());
    }

    #[test]
    fn cursor_reads_between_its_block_borders_and_knows_both_placeholders() {
        let screen = " ▄▄▄▄▄▄▄▄\n  → 続きを頼む\n ▀▀▀▀▀▀▀▀\n  Cursor Grok 4.6";
        assert_eq!(composer("cursor", screen).unwrap(), "続きを頼む");
        for empty in [
            " ▄▄▄▄▄▄▄▄\n  → Add a follow-up\n ▀▀▀▀▀▀▀▀",
            " ▄▄▄▄▄▄▄▄\n  → Plan, search,\n    build anything\n ▀▀▀▀▀▀▀▀",
        ] {
            assert!(composer("cursor", empty).is_none(), "{empty}");
        }
    }

    #[test]
    fn a_bare_prompt_glyph_is_an_empty_box_for_every_agent() {
        assert!(composer("codex", "  gpt-5.6\n›\n  status").is_none());
        assert!(composer("cursor", " ▄▄▄▄\n  →\n ▀▀▀▀").is_none());
        assert!(composer("grok", "❯").is_none());
    }

    #[test]
    fn a_lone_border_is_not_a_box() {
        assert!(composer("cursor", " ▀▀▀▀\n  → draft").is_none());
    }

    #[test]
    fn a_bordered_output_panel_is_not_a_cursor_composer() {
        let screen = " ▄▄▄▄▄▄▄▄\n  ordinary output\n ▀▀▀▀▀▀▀▀";
        assert!(composer("cursor", screen).is_none());
    }

    #[test]
    fn an_unknown_agent_and_a_broken_screen_both_blank() {
        assert!(composer("devin", "❯ something").is_none());
        assert!(composer("claude", "no prompt row here at all").is_none());
    }
}
