//! Lightweight rich-text markup typed in the body editor, exported to HTML and plain text.
//!
//! Inline: `**bold**`, `*italic*`, `__underline__`, `~~strike~~`, `[text](url)`,
//! `{color=#rrggbb}…{/}`, `{bg=#rrggbb}…{/}`, `{size=18}…{/}`; a `\` escapes the next marker.
//! Lines: `- item` / `1. item` lists (2 spaces per nesting level) and `> quote` (repeatable).
//!
//! The editing helpers ([`toggle_wrap`], [`toggle_lines`]...) return an [`Edit`] in UTF-8 byte
//! offsets, matching the text input's selection API.

use std::ops::Range;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Text colour `0xRRGGBB`.
    pub color: Option<u32>,
    /// Background colour `0xRRGGBB`.
    pub highlight: Option<u32>,
    /// Font size in px.
    pub size: Option<u8>,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Bullet,
    /// Numbered list item with its number.
    Numbered(u32),
}

/// One line of the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// List nesting level (0 = top level); always 0 for paragraphs.
    pub depth: usize,
    /// Quote nesting level (number of leading `>`).
    pub quote: usize,
    pub spans: Vec<Span>,
}

/// Parses markup into one block per line.
pub fn parse(markup: &str) -> Vec<Block> {
    markup
        .split('\n')
        .map(|line| parse_line(line.strip_suffix('\r').unwrap_or(line)))
        .collect()
}

fn parse_line(line: &str) -> Block {
    let mut rest = line;
    let mut quote = 0;
    while let Some(after) = rest.strip_prefix('>') {
        quote += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    let body = rest.trim_start_matches([' ', '\t']);
    let indent = &rest[..rest.len() - body.len()];
    let depth = indent
        .chars()
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum::<usize>()
        / 2;
    let (kind, depth, content) = match list_marker(body) {
        Some((kind, marker_len)) => (kind, depth, &body[marker_len..]),
        None => (BlockKind::Paragraph, 0, rest),
    };
    Block {
        kind,
        depth,
        quote,
        spans: parse_inline(content, &SpanStyle::default()),
    }
}

/// List marker at the start of `body` and its byte length (`- `, `* `, `• `, `12. `).
fn list_marker(body: &str) -> Option<(BlockKind, usize)> {
    for bullet in ["- ", "* ", "• "] {
        if body.starts_with(bullet) {
            return Some((BlockKind::Bullet, bullet.len()));
        }
    }
    let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 9 || !body[digits..].starts_with(". ") {
        return None;
    }
    let number = body[..digits].parse().ok()?;
    Some((BlockKind::Numbered(number), digits + 2))
}

#[derive(Debug, Clone, Copy)]
enum Toggle {
    Bold,
    Italic,
    Underline,
    Strike,
}

fn toggle_marker(rest: &str) -> Option<(&'static str, Toggle)> {
    [
        ("**", Toggle::Bold),
        ("__", Toggle::Underline),
        ("~~", Toggle::Strike),
        ("*", Toggle::Italic),
    ]
    .into_iter()
    .find(|(marker, _)| rest.starts_with(marker))
}

fn flag(style: &mut SpanStyle, toggle: Toggle) -> &mut bool {
    match toggle {
        Toggle::Bold => &mut style.bold,
        Toggle::Italic => &mut style.italic,
        Toggle::Underline => &mut style.underline,
        Toggle::Strike => &mut style.strike,
    }
}

fn is_markup_char(c: char) -> bool {
    matches!(
        c,
        '*' | '_' | '~' | '[' | ']' | '{' | '}' | '\\' | '(' | ')' | '>' | '-'
    )
}

/// Parses inline markers of one line, starting from `base`.
pub fn parse_inline(text: &str, base: &SpanStyle) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut style = base.clone();
    let mut saved: Vec<SpanStyle> = Vec::new();
    let mut buffer = String::new();
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if let Some(after) = rest.strip_prefix('\\') {
            match after.chars().next() {
                Some(c) if is_markup_char(c) => {
                    buffer.push(c);
                    index += 1 + c.len_utf8();
                }
                _ => {
                    buffer.push('\\');
                    index += 1;
                }
            }
            continue;
        }
        if let Some((marker, toggle)) = toggle_marker(rest) {
            let after = &rest[marker.len()..];
            let active = *flag(&mut style, toggle);
            let opens = !active
                && after.chars().next().is_some_and(|c| !c.is_whitespace())
                && has_unescaped(after, marker);
            if active || opens {
                push_span(&mut spans, &mut buffer, &style);
                *flag(&mut style, toggle) = !active;
            } else {
                // A lone marker is literal text (never half of a longer marker).
                buffer.push_str(marker);
            }
            index += marker.len();
            continue;
        }
        if rest.starts_with('[')
            && let Some((label, url, consumed)) = parse_link(rest)
        {
            push_span(&mut spans, &mut buffer, &style);
            let link_style = SpanStyle {
                link: Some(url),
                ..style.clone()
            };
            for span in parse_inline(label, &link_style) {
                merge_span(&mut spans, span);
            }
            index += consumed;
            continue;
        }
        if rest.starts_with("{/}")
            && let Some(previous) = saved.pop()
        {
            push_span(&mut spans, &mut buffer, &style);
            style = previous;
            index += 3;
            continue;
        }
        if rest.starts_with('{')
            && let Some((attribute, consumed)) = parse_attribute(rest)
            && rest[consumed..].contains("{/}")
        {
            push_span(&mut spans, &mut buffer, &style);
            saved.push(style.clone());
            match attribute {
                Attribute::Color(color) => style.color = Some(color),
                Attribute::Highlight(color) => style.highlight = Some(color),
                Attribute::Size(size) => style.size = Some(size),
            }
            index += consumed;
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        buffer.push(c);
        index += c.len_utf8();
    }
    push_span(&mut spans, &mut buffer, &style);
    spans
}

fn has_unescaped(haystack: &str, marker: &str) -> bool {
    haystack
        .match_indices(marker)
        .any(|(index, _)| !haystack[..index].ends_with('\\'))
}

fn push_span(spans: &mut Vec<Span>, buffer: &mut String, style: &SpanStyle) {
    if !buffer.is_empty() {
        let text = std::mem::take(buffer);
        merge_span(
            spans,
            Span {
                text,
                style: style.clone(),
            },
        );
    }
}

fn merge_span(spans: &mut Vec<Span>, span: Span) {
    match spans.last_mut() {
        Some(last) if last.style == span.style => last.text.push_str(&span.text),
        _ => spans.push(span),
    }
}

/// `[label](url)` at the start of `rest`: (label, normalized url, consumed bytes).
fn parse_link(rest: &str) -> Option<(&str, String, usize)> {
    let label_end = rest.find("](")?;
    let label = &rest[1..label_end];
    let url_start = label_end + 2;
    let url_len = rest[url_start..].find(')')?;
    let url = rest[url_start..url_start + url_len].trim();
    if label.is_empty() || url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    Some((label, safe_url(url)?, url_start + url_len + 1))
}

/// Keeps web, mail and phone links; adds `https://` to bare hosts; rejects other schemes.
pub fn safe_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    const ALLOWED: [&str; 5] = ["http://", "https://", "mailto:", "tel:", "ftp://"];
    if ALLOWED.iter().any(|scheme| lower.starts_with(scheme)) {
        Some(url.to_owned())
    } else if url.contains('@') && !url.contains('/') {
        Some(format!("mailto:{url}"))
    } else if lower.contains(':') && !lower.starts_with("www.") {
        None
    } else {
        Some(format!("https://{url}"))
    }
}

enum Attribute {
    Color(u32),
    Highlight(u32),
    Size(u8),
}

fn parse_attribute(rest: &str) -> Option<(Attribute, usize)> {
    let end = rest.find('}')?;
    let (key, value) = rest[1..end].split_once('=')?;
    let color = || {
        let hex = value.strip_prefix('#')?;
        (hex.len() == 6)
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
    };
    let attribute = match key {
        "color" => Attribute::Color(color()?),
        "bg" => Attribute::Highlight(color()?),
        "size" => Attribute::Size(value.parse().ok().filter(|size| (6..=72).contains(size))?),
        _ => return None,
    };
    Some((attribute, end + 1))
}

// ---------- export ----------

const BLOCKQUOTE_OPEN: &str =
    r#"<blockquote style="margin:0 0 0 0.8ex;border-left:2px solid #c7c7cc;padding-left:1ex">"#;

/// Renders blocks as an HTML fragment (lists nested properly, quotes as `<blockquote>`).
pub fn to_html(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut quote = 0;
    // Open lists (ordered?) from the outermost; each has an open `<li>`.
    let mut lists: Vec<bool> = Vec::new();
    for block in blocks {
        let is_item = block.kind != BlockKind::Paragraph;
        if block.quote != quote || !is_item {
            close_lists(&mut out, &mut lists, 0);
        }
        while quote < block.quote {
            out.push_str(BLOCKQUOTE_OPEN);
            quote += 1;
        }
        while quote > block.quote {
            out.push_str("</blockquote>");
            quote -= 1;
        }
        let content = spans_html(&block.spans);
        if !is_item {
            let content = if content.is_empty() { "<br>" } else { &content };
            out.push_str(&format!("<div>{content}</div>"));
            continue;
        }
        let ordered = matches!(block.kind, BlockKind::Numbered(_));
        let level = block.depth.min(lists.len());
        close_lists(&mut out, &mut lists, level + 1);
        if lists.len() == level + 1 && lists[level] != ordered {
            close_lists(&mut out, &mut lists, level);
        }
        if lists.len() == level + 1 {
            out.push_str("</li>");
        } else {
            match block.kind {
                BlockKind::Numbered(start) if start != 1 => {
                    out.push_str(&format!(r#"<ol start="{start}">"#))
                }
                BlockKind::Numbered(_) => out.push_str("<ol>"),
                _ => out.push_str("<ul>"),
            }
            lists.push(ordered);
        }
        out.push_str("<li>");
        out.push_str(&content);
    }
    close_lists(&mut out, &mut lists, 0);
    for _ in 0..quote {
        out.push_str("</blockquote>");
    }
    out
}

fn close_lists(out: &mut String, lists: &mut Vec<bool>, keep: usize) {
    while lists.len() > keep {
        let ordered = lists.pop().unwrap_or_default();
        out.push_str(if ordered { "</li></ol>" } else { "</li></ul>" });
    }
}

fn spans_html(spans: &[Span]) -> String {
    spans.iter().map(span_html).collect()
}

fn span_html(span: &Span) -> String {
    let style = &span.style;
    let mut html = escape_html(&span.text);
    for (on, tag) in [
        (style.strike, "s"),
        (style.underline, "u"),
        (style.italic, "i"),
        (style.bold, "b"),
    ] {
        if on {
            html = format!("<{tag}>{html}</{tag}>");
        }
    }
    let mut css = Vec::new();
    if let Some(color) = style.color {
        css.push(format!("color:#{color:06x}"));
    }
    if let Some(color) = style.highlight {
        css.push(format!("background-color:#{color:06x}"));
    }
    if let Some(size) = style.size {
        css.push(format!("font-size:{size}px"));
    }
    if !css.is_empty() {
        html = format!(r#"<span style="{}">{html}</span>"#, css.join(";"));
    }
    if let Some(url) = &style.link {
        html = format!(r#"<a href="{}">{html}</a>"#, escape_html(url));
    }
    html
}

pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wraps an HTML fragment in a complete document with a sane default font.
pub fn html_document(fragment: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>\
         <div style=\"font-family:'Segoe UI',-apple-system,Helvetica,Arial,sans-serif;\
         font-size:14px;line-height:1.45\">{fragment}</div></body></html>"
    )
}

/// Plain-text rendering: `• ` bullets, `N. ` numbers, `> ` quotes, `text <url>` links.
pub fn to_plain(blocks: &[Block]) -> String {
    let lines: Vec<String> = blocks
        .iter()
        .map(|block| {
            let mut line = "> ".repeat(block.quote);
            match block.kind {
                BlockKind::Paragraph => {}
                BlockKind::Bullet => {
                    line.push_str(&"  ".repeat(block.depth));
                    line.push_str("• ");
                }
                BlockKind::Numbered(number) => {
                    line.push_str(&"  ".repeat(block.depth));
                    line.push_str(&format!("{number}. "));
                }
            }
            for span in &block.spans {
                line.push_str(&span.text);
                if let Some(url) = &span.style.link {
                    let bare = url.strip_prefix("mailto:").unwrap_or(url);
                    if bare != span.text {
                        line.push_str(&format!(" <{bare}>"));
                    }
                }
            }
            line.trim_end().to_owned()
        })
        .collect();
    lines.join("\n")
}

/// Markup to plain text in one call.
pub fn plain_text(markup: &str) -> String {
    to_plain(&parse(markup))
}

/// Escapes text so it shows literally when inserted into markup (e.g. a quoted message).
pub fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        let needs_escape = match c {
            '\\' | '*' | '[' | '{' => true,
            '_' | '~' => next == Some(c),
            _ => false,
        };
        if needs_escape {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------- editing helpers ----------

/// Replace `range` (bytes, current text) with `text`, then select `select` (bytes, new text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub select: Range<usize>,
}

fn clamp(text: &str, range: Range<usize>) -> Range<usize> {
    let fix = |mut offset: usize| {
        offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let (start, end) = (fix(range.start), fix(range.end));
    start.min(end)..start.max(end)
}

/// Wraps the selection in `open`/`close`, or unwraps it when already wrapped (toggle).
/// With an empty selection, inserts both markers and puts the caret between them.
pub fn toggle_wrap(text: &str, selection: Range<usize>, open: &str, close: &str) -> Edit {
    let Range { start, end } = clamp(text, selection);
    let inner = &text[start..end];
    if inner.len() >= open.len() + close.len() && inner.starts_with(open) && inner.ends_with(close)
    {
        let unwrapped = &inner[open.len()..inner.len() - close.len()];
        return Edit {
            range: start..end,
            text: unwrapped.to_owned(),
            select: start..start + unwrapped.len(),
        };
    }
    if !inner.is_empty() && text[..start].ends_with(open) && text[end..].starts_with(close) {
        let outer = start - open.len();
        return Edit {
            range: outer..end + close.len(),
            text: inner.to_owned(),
            select: outer..outer + inner.len(),
        };
    }
    let inner_start = start + open.len();
    Edit {
        range: start..end,
        text: format!("{open}{inner}{close}"),
        select: inner_start..inner_start + inner.len(),
    }
}

/// Line-level formats toggled by [`toggle_lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineFormat {
    Bullet,
    Numbered,
    Quote,
}

/// Byte range of the whole lines touched by `selection`.
fn line_range(text: &str, selection: &Range<usize>) -> Range<usize> {
    let start = text[..selection.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[selection.end..]
        .find('\n')
        .map_or(text.len(), |i| selection.end + i);
    start..end
}

/// Toggles a list or quote prefix on every line touched by the selection.
pub fn toggle_lines(text: &str, selection: Range<usize>, format: LineFormat) -> Edit {
    let selection = clamp(text, selection);
    let range = line_range(text, &selection);
    let lines: Vec<&str> = text[range.clone()].split('\n').collect();
    let has_format = |line: &str| match format {
        LineFormat::Quote => line.starts_with('>'),
        LineFormat::Bullet => matches!(
            list_marker(line.trim_start_matches([' ', '\t'])),
            Some((BlockKind::Bullet, _))
        ),
        LineFormat::Numbered => matches!(
            list_marker(line.trim_start_matches([' ', '\t'])),
            Some((BlockKind::Numbered(_), _))
        ),
    };
    let all_formatted = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .all(|line| has_format(line))
        && lines.iter().any(|line| !line.trim().is_empty());

    let mut number = 0;
    let new_lines: Vec<String> = lines
        .iter()
        .map(|line| {
            if format == LineFormat::Quote {
                return if all_formatted {
                    let rest = &line[1..];
                    rest.strip_prefix(' ').unwrap_or(rest).to_owned()
                } else {
                    format!("> {line}")
                };
            }
            let body = line.trim_start_matches([' ', '\t']);
            let indent = &line[..line.len() - body.len()];
            let content = list_marker(body).map_or(body, |(_, len)| &body[len..]);
            if all_formatted {
                format!("{indent}{content}")
            } else if line.trim().is_empty() && lines.len() > 1 {
                (*line).to_owned()
            } else {
                number += 1;
                match format {
                    LineFormat::Numbered => format!("{indent}{number}. {content}"),
                    _ => format!("{indent}- {content}"),
                }
            }
        })
        .collect();
    let new_text = new_lines.join("\n");
    let caret = range.start + new_text.len();
    Edit {
        select: if selection.is_empty() {
            caret..caret
        } else {
            range.start..caret
        },
        range,
        text: new_text,
    }
}

/// Indents (or outdents) every touched line by one list level (two spaces).
pub fn indent_lines(text: &str, selection: Range<usize>, outdent: bool) -> Edit {
    let selection = clamp(text, selection);
    let range = line_range(text, &selection);
    let new_text = text[range.clone()]
        .split('\n')
        .map(|line| {
            if outdent {
                let spaces = line.len() - line.trim_start_matches(' ').len();
                line[spaces.min(2)..].to_owned()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let caret = range.start + new_text.len();
    Edit {
        select: range.start..caret,
        range,
        text: new_text,
    }
}

/// Turns the selection (or the url itself) into `[label](url)`; caret after the link.
pub fn insert_link(text: &str, selection: Range<usize>, url: &str) -> Edit {
    let Range { start, end } = clamp(text, selection);
    let label = if start == end { url } else { &text[start..end] };
    let link = format!("[{label}]({})", url.trim());
    let caret = start + link.len();
    Edit {
        range: start..end,
        text: link,
        select: caret..caret,
    }
}

/// Applies an edit to a string (what the text input does), for tests and previews.
pub fn apply_edit(text: &str, edit: &Edit) -> String {
    format!(
        "{}{}{}",
        &text[..edit.range.start],
        edit.text,
        &text[edit.range.end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(text: &str, change: impl FnOnce(&mut SpanStyle)) -> Span {
        let mut style = SpanStyle::default();
        change(&mut style);
        Span {
            text: text.into(),
            style,
        }
    }

    #[test]
    fn parses_inline_styles() {
        let spans = parse_inline(
            "a **b** *c* __d__ ~~e~~ {color=#ff0000}f{/} [g](example.com) 2 * 3",
            &SpanStyle::default(),
        );
        assert_eq!(
            spans,
            vec![
                styled("a ", |_| {}),
                styled("b", |s| s.bold = true),
                styled(" ", |_| {}),
                styled("c", |s| s.italic = true),
                styled(" ", |_| {}),
                styled("d", |s| s.underline = true),
                styled(" ", |_| {}),
                styled("e", |s| s.strike = true),
                styled(" ", |_| {}),
                styled("f", |s| s.color = Some(0xff0000)),
                styled(" ", |_| {}),
                styled("g", |s| s.link = Some("https://example.com".into())),
                styled(" 2 * 3", |_| {}),
            ]
        );
    }

    #[test]
    fn unmatched_markers_and_escapes_stay_literal() {
        let spans = parse_inline(
            r"**open \*not\* {size=200}x{/} snake_case",
            &SpanStyle::default(),
        );
        assert_eq!(
            spans,
            vec![styled("**open *not* {size=200}x{/} snake_case", |_| {})]
        );
        assert_eq!(
            plain_text(&escape_markup("a*b __c__ [d](e) ~~f~~")),
            "a*b __c__ [d](e) ~~f~~"
        );
        assert_eq!(safe_url("javascript:alert(1)"), None);
    }

    #[test]
    fn exports_nested_lists_and_quotes() {
        let markup = "Hi **all**\n- one\n  1. sub\n  2. sub2\n- two\n> quoted\n> > deeper\nbye";
        let html = to_html(&parse(markup));
        assert_eq!(
            html,
            format!(
                "<div>Hi <b>all</b></div><ul><li>one<ol><li>sub</li><li>sub2</li></ol></li>\
                 <li>two</li></ul>{q}<div>quoted</div>{q}<div>deeper</div></blockquote>\
                 </blockquote><div>bye</div>",
                q = BLOCKQUOTE_OPEN
            )
        );
        assert_eq!(
            plain_text(markup),
            "Hi all\n• one\n  1. sub\n  2. sub2\n• two\n> quoted\n> > deeper\nbye"
        );
        assert_eq!(
            to_html(&parse("[x](a@b.co) <tag> & {bg=#00ff00}hi{/}")),
            "<div><a href=\"mailto:a@b.co\">x</a> &lt;tag&gt; &amp; \
             <span style=\"background-color:#00ff00\">hi</span></div>"
        );
    }

    #[test]
    fn wrap_toggles_both_ways() {
        let text = "hello world";
        let edit = toggle_wrap(text, 6..11, "**", "**");
        let wrapped = apply_edit(text, &edit);
        assert_eq!(wrapped, "hello **world**");
        assert_eq!(&wrapped[edit.select.clone()], "world");
        let edit = toggle_wrap(&wrapped, edit.select, "**", "**");
        assert_eq!(apply_edit(&wrapped, &edit), text);
        let edit = toggle_wrap(text, 5..5, "*", "*");
        assert_eq!(apply_edit(text, &edit), "hello** world");
        assert_eq!(edit.select, 6..6);
    }

    #[test]
    fn line_formats_toggle_and_indent() {
        let text = "a\nb\nc";
        let edit = toggle_lines(text, 0..3, LineFormat::Numbered);
        let numbered = apply_edit(text, &edit);
        assert_eq!(numbered, "1. a\n2. b\nc");
        let edit = toggle_lines(&numbered, 0..numbered.len(), LineFormat::Bullet);
        assert_eq!(apply_edit(&numbered, &edit), "- a\n- b\n- c");
        let edit = toggle_lines("- a\n- b", 0..1, LineFormat::Bullet);
        assert_eq!(apply_edit("- a\n- b", &edit), "a\n- b");
        let edit = toggle_lines("x", 0..0, LineFormat::Quote);
        assert_eq!(apply_edit("x", &edit), "> x");
        let edit = indent_lines("- a\n- b", 4..5, false);
        assert_eq!(apply_edit("- a\n- b", &edit), "- a\n  - b");
        let edit = insert_link("see docs", 4..8, "docs.rs");
        assert_eq!(apply_edit("see docs", &edit), "see [docs](docs.rs)");
    }
}
