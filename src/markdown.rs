//! Markdown to HTML on the server, so no parser ships to the phone.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// `ENABLE_GFM` is deliberately absent: in 0.13 it only adds alert blockquotes,
/// which nothing here renders. Tables and task lists are their own flags.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// A destination the page may follow. Anything with a scheme that is not
/// http, https, or mailto is emptied — `javascript:` and `data:` included.
fn safe_url(dest: CowStr<'_>) -> CowStr<'_> {
    let scheme = dest.split(':').next().unwrap_or_default();
    let relative = !dest.contains(':') || scheme.contains('/');
    if relative || ["http", "https", "mailto"].contains(&scheme) {
        dest
    } else {
        "".into()
    }
}

/// Raw HTML is escaped rather than executed, and links and images are limited
/// to relative destinations and the http/https/mailto schemes, so an agent's
/// output cannot script the page that displays it.
pub fn to_html(md: &str) -> String {
    let events = Parser::new_ext(md, options()).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        // An image's src runs through the same gate: `<img src=x onerror=…>`
        // arrives as raw HTML and is escaped, but `![a](javascript:…)` is a
        // parsed image and would otherwise pass straight through.
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        other => other,
    });

    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// A user's own words, as HTML that means exactly what they typed. Both
/// speakers reach the phone through the same `html` field, so the user's half
/// is escaped here rather than left as a second, differently-handled shape.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
    out
}

/// The card's three-line preview is plain text in one node, which is what lets
/// CSS clamp it cleanly; markup would give the clamp block children to trip on.
/// Block ends and soft breaks become single spaces, so a heading does not run
/// into the paragraph beneath it.
pub fn preview(md: &str, cap: usize) -> String {
    let mut out = String::new();
    for event in Parser::new_ext(md, options()) {
        match event {
            // Raw HTML is text here as it is in `to_html`: prose that mentions
            // `<Foo>` must not lose it from the card it is previewed on.
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push_str(&text)
            }
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                out.push(' ');
            }
            _ => {}
        }
        if out.chars().count() >= cap {
            break;
        }
    }
    // Inline ends (emphasis, code spans) push nothing, so runs of whitespace
    // only come from the source itself; collapse them so the clamp counts real
    // lines rather than blank ones.
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(cap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_survives() {
        let html =
            to_html("| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n```rust\nlet x = 1;\n```");
        assert!(html.contains("<table>"));
        assert!(html.contains(r#"type="checkbox""#));
        assert!(html.contains(r#"class="language-rust""#));
    }

    #[test]
    fn raw_html_is_escaped_not_executed() {
        let html = to_html("型は <Foo> と書く。\n<script>alert(1)</script>");
        assert!(html.contains("&lt;Foo&gt;"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn a_javascript_link_loses_its_destination() {
        let html = to_html("[click](javascript:alert(1)) [ok](https://herdr.dev) [rel](./a.md)");
        assert!(html.contains(r#"<a href="">click</a>"#));
        assert!(html.contains(r#"href="https://herdr.dev""#));
        assert!(html.contains(r#"href="./a.md""#));
    }

    #[test]
    fn an_image_source_runs_through_the_same_gate() {
        let html = to_html("![a](javascript:alert(1)) ![b](https://x/y.png)");
        assert!(html.contains(r#"<img src="" alt="a" />"#));
        assert!(html.contains(r#"<img src="https://x/y.png" alt="b" />"#));
        assert!(to_html("![c](data:image/svg+xml;base64,AAA)").contains(r#"src="""#));
    }

    #[test]
    fn an_event_handler_attribute_is_escaped_with_its_tag() {
        let html = to_html("<img src=x onerror=alert(1)>");
        assert_eq!(html, "&lt;img src=x onerror=alert(1)&gt;");
    }

    #[test]
    fn preview_is_plain_text_within_its_cap() {
        let text = preview("## 原因1\n\n`read()` は **recent** を渡す。", 300);
        assert_eq!(text, "原因1 read() は recent を渡す。");
    }

    #[test]
    fn a_users_own_words_become_html_that_means_what_they_typed() {
        assert_eq!(
            escape("<script>alert('x' & \"y\")</script>"),
            "&lt;script&gt;alert(&#39;x&#39; &amp; &quot;y&quot;)&lt;/script&gt;"
        );
    }

    /// The card previews what the modal shows. Prose about `<Foo>` keeps it in
    /// both places.
    #[test]
    fn preview_keeps_literal_html_shaped_prose() {
        assert_eq!(preview("型は <Foo> と書く。", 300), "型は <Foo> と書く。");
    }

    #[test]
    fn preview_stops_at_the_cap() {
        assert_eq!(preview(&"あ".repeat(500), 10).chars().count(), 10);
    }
}
