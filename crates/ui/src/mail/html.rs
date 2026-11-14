//! Safe mail body rendering: a small, tolerant HTML parser producing [`Block`]s, a plain-text
//! fallback, and a renderer to GPUI elements.
//!
//! Nothing executable survives parsing: `script`, `style`, `iframe`, forms' internals, event
//! attributes and `javascript:` links are dropped. Remote images are kept as references and
//! only loaded when the caller allows it ("Show pictures").

use std::rc::Rc;

use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::components::{Icon, IconName};
use crate::theme::{
    RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XL, Theme,
};

// ---------------------------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub code: bool,
}

/// A run of text with one style; `link` is a sanitized http(s)/mailto URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub depth: usize,
    /// `Some(n)` for ordered lists.
    pub number: Option<usize>,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading(u8, Vec<Span>),
    List(Vec<ListItem>),
    Quote(Vec<Block>),
    /// Rows of cells; each cell holds blocks.
    Table(Vec<Vec<Vec<Block>>>),
    Preformatted(String),
    Image {
        src: String,
        alt: String,
    },
    Rule,
}

/// True for an image that would be fetched from the network.
pub fn is_remote(src: &str) -> bool {
    let lower = src.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")
}

/// Whether any block references a remote image (shows the "Show pictures" bar).
pub fn has_remote_images(blocks: &[Block]) -> bool {
    blocks.iter().any(|block| match block {
        Block::Image { src, .. } => is_remote(src),
        Block::Quote(inner) => has_remote_images(inner),
        Block::Table(rows) => rows.iter().flatten().any(|cell| has_remote_images(cell)),
        _ => false,
    })
}

// ---------------------------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open {
        name: String,
        attrs: Vec<(String, String)>,
    },
    Close(String),
}

/// Elements whose content is never shown.
const SKIPPED: &[&str] = &[
    "script", "style", "head", "title", "noscript", "template", "iframe", "object", "svg", "math",
    "select", "textarea", "button",
];

fn tokenize(html: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            tokens.push(Token::Text(decode_entities(rest)));
            break;
        };
        if lt > 0 {
            tokens.push(Token::Text(decode_entities(&rest[..lt])));
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = if closing { 2 } else { 1 };
        let name_len = rest[name_start..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len() - name_start);
        if name_len == 0 {
            // A lone "<" in text.
            tokens.push(Token::Text("<".into()));
            rest = &rest[1..];
            continue;
        }
        let name = rest[name_start..name_start + name_len].to_ascii_lowercase();
        let tag_rest = &rest[name_start + name_len..];
        let Some(end) = tag_end(tag_rest) else {
            break; // Unterminated tag: drop the remainder.
        };
        let inner = &tag_rest[..end];
        rest = &tag_rest[end + 1..];
        if closing {
            tokens.push(Token::Close(name));
            continue;
        }
        if SKIPPED.contains(&name.as_str()) && !inner.trim_end().ends_with('/') {
            let close = format!("</{name}");
            rest = match rest.to_ascii_lowercase().find(&close) {
                Some(index) => {
                    let after = &rest[index..];
                    after.find('>').map_or("", |end| &after[end + 1..])
                }
                None => "",
            };
            continue;
        }
        tokens.push(Token::Open {
            name,
            attrs: parse_attrs(inner),
        });
    }
    tokens
}

/// Index of the `>` closing a tag, ignoring `>` inside quoted attribute values.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_attrs(inner: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut rest = inner.trim_start();
    while !rest.is_empty() {
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/' || c == '>')
            .unwrap_or(rest.len());
        if name_len == 0 {
            rest = rest[1..].trim_start();
            continue;
        }
        let name = rest[..name_len].to_ascii_lowercase();
        rest = rest[name_len..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (raw, remaining) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    (&body[..end], body.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            value = decode_entities(raw);
            rest = remaining.trim_start();
        }
        attrs.push((name, value));
    }
    attrs
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Decodes the common named entities and every numeric one.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| entity(&rest[1..=semi]).map(|c| (c, semi + 2)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).filter(|c| *c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "deg" => '°',
        "times" => '×',
        "rarr" => '→',
        "larr" => '←',
        "zwnj" | "zwj" | "shy" => '\u{200b}',
        _ => return None,
    })
}

/// Keeps only links that are safe to open.
fn safe_link(href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:"))
        .then(|| href.to_owned())
}

// ---------------------------------------------------------------------------------------------
// Tree building
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Root,
    Quote,
    Cell,
}

struct Frame {
    kind: FrameKind,
    blocks: Vec<Block>,
    /// Lists opened outside this frame don't receive its text.
    list_base: usize,
}

struct ListState {
    ordered: bool,
    counter: usize,
    item_open: bool,
    items: Vec<ListItem>,
}

struct TableState {
    rows: Vec<Vec<Vec<Block>>>,
    /// Frame depth of the open cell, if any.
    cell_depth: Option<usize>,
}

#[derive(Default)]
struct Builder {
    frames: Vec<Frame>,
    inline: Vec<Span>,
    heading: Option<u8>,
    bold: usize,
    italic: usize,
    underline: usize,
    code: usize,
    pre: usize,
    links: Vec<Option<String>>,
    lists: Vec<ListState>,
    tables: Vec<TableState>,
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "center",
    "address",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "body",
    "html",
    "caption",
    "thead",
    "tbody",
    "tfoot",
];

impl Builder {
    fn new() -> Self {
        Self {
            frames: vec![Frame {
                kind: FrameKind::Root,
                blocks: Vec::new(),
                list_base: 0,
            }],
            ..Self::default()
        }
    }

    fn style(&self) -> SpanStyle {
        SpanStyle {
            bold: self.bold > 0 || self.heading.is_some(),
            italic: self.italic > 0,
            underline: self.underline > 0,
            code: self.code > 0 || self.pre > 0,
        }
    }

    fn push_raw(&mut self, text: &str) {
        let style = self.style();
        let link = self.links.iter().rev().flatten().next().cloned();
        match self.inline.last_mut() {
            Some(last) if last.style == style && last.link == link => last.text.push_str(text),
            _ => self.inline.push(Span {
                text: text.to_owned(),
                style,
                link,
            }),
        }
    }

    fn text(&mut self, text: &str) {
        if self.pre > 0 {
            self.push_raw(text);
            return;
        }
        let mut collapsed = String::with_capacity(text.len());
        let mut space = self
            .inline
            .last()
            .and_then(|span| span.text.chars().last())
            .is_none_or(char::is_whitespace);
        for c in text.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !space {
                    collapsed.push(' ');
                    space = true;
                }
            } else if c != '\u{200b}' {
                collapsed.push(c);
                space = false;
            }
        }
        if !collapsed.is_empty() {
            self.push_raw(&collapsed);
        }
    }

    fn line_break(&mut self) {
        self.push_raw("\n");
    }

    /// Takes the pending inline content, trimmed; `None` when blank.
    fn take_inline(&mut self) -> Option<Vec<Span>> {
        let mut spans = std::mem::take(&mut self.inline);
        while let Some(last) = spans.last_mut() {
            let trimmed = last.text.trim_end().len();
            last.text.truncate(trimmed);
            if last.text.is_empty() {
                spans.pop();
            } else {
                break;
            }
        }
        while let Some(first) = spans.first_mut() {
            let trimmed = first.text.trim_start().to_owned();
            first.text = trimmed;
            if first.text.is_empty() {
                spans.remove(0);
            } else {
                break;
            }
        }
        (!spans.is_empty()).then_some(spans)
    }

    fn frame(&mut self) -> &mut Frame {
        // The root frame is never popped.
        let last = self.frames.len() - 1;
        &mut self.frames[last]
    }

    /// The open list item that receives text, if it belongs to the current frame.
    fn open_item(&mut self) -> Option<&mut ListItem> {
        let base = self.frames.last().map_or(0, |frame| frame.list_base);
        if self.lists.len() <= base {
            return None;
        }
        let list = self.lists.last_mut()?;
        if list.item_open {
            list.items.last_mut()
        } else {
            None
        }
    }

    fn flush(&mut self) {
        if self.pre > 0 {
            let text: String = self.inline.drain(..).map(|span| span.text).collect();
            let text = text.trim_matches('\n').to_owned();
            if !text.trim().is_empty() {
                self.push_block(Block::Preformatted(text));
            }
            return;
        }
        let Some(spans) = self.take_inline() else {
            return;
        };
        if let Some(item) = self.open_item() {
            if !item.spans.is_empty() {
                item.spans.push(Span {
                    text: "\n".into(),
                    style: SpanStyle::default(),
                    link: None,
                });
            }
            item.spans.extend(spans);
            return;
        }
        let block = match self.heading {
            Some(level) => Block::Heading(level, spans),
            None => Block::Paragraph(spans),
        };
        self.frame().blocks.push(block);
    }

    fn push_block(&mut self, block: Block) {
        if let Some(item) = self.open_item() {
            if let Block::Image { alt, .. } = &block
                && !alt.trim().is_empty()
            {
                item.spans.push(Span {
                    text: format!(" [{}]", alt.trim()),
                    style: SpanStyle::default(),
                    link: None,
                });
            }
            if matches!(block, Block::Image { .. }) {
                return;
            }
        }
        self.frame().blocks.push(block);
    }

    fn push_frame(&mut self, kind: FrameKind) {
        let list_base = self.lists.len();
        self.frames.push(Frame {
            kind,
            blocks: Vec::new(),
            list_base,
        });
    }

    /// Pops the top frame (never the root) and returns its blocks.
    fn pop_frame(&mut self) -> Vec<Block> {
        if self.frames.len() <= 1 {
            return Vec::new();
        }
        self.frames
            .pop()
            .map(|frame| frame.blocks)
            .unwrap_or_default()
    }

    fn close_quote(&mut self) {
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.kind == FrameKind::Quote)
        {
            self.flush();
            let blocks = self.pop_frame();
            if !blocks.is_empty() {
                self.push_block(Block::Quote(blocks));
            }
        }
    }

    fn close_list(&mut self) {
        self.flush();
        let Some(list) = self.lists.pop() else {
            return;
        };
        let base = self.frames.last().map_or(0, |frame| frame.list_base);
        if self.lists.len() > base {
            if let Some(parent) = self.lists.last_mut() {
                parent.items.extend(list.items);
            }
        } else if !list.items.is_empty() {
            self.push_block(Block::List(list.items));
        }
    }

    fn open_item_tag(&mut self) {
        self.flush();
        let base = self.frames.last().map_or(0, |frame| frame.list_base);
        if self.lists.len() <= base {
            self.lists.push(ListState {
                ordered: false,
                counter: 0,
                item_open: false,
                items: Vec::new(),
            });
        }
        let depth = self.lists.len() - 1 - base;
        if let Some(list) = self.lists.last_mut() {
            list.counter += 1;
            list.item_open = true;
            list.items.push(ListItem {
                depth,
                number: list.ordered.then_some(list.counter),
                spans: Vec::new(),
            });
        }
    }

    fn close_cell(&mut self) {
        let Some(depth) = self.tables.last().and_then(|table| table.cell_depth) else {
            return;
        };
        self.flush();
        while self.frames.len() > depth + 1 {
            let blocks = self.pop_frame();
            if !blocks.is_empty() {
                self.push_block(Block::Quote(blocks));
            }
        }
        let blocks = self.pop_frame();
        if let Some(table) = self.tables.last_mut() {
            table.cell_depth = None;
            match table.rows.last_mut() {
                Some(row) => row.push(blocks),
                None => table.rows.push(vec![blocks]),
            }
        }
    }

    fn open_cell(&mut self) {
        if self.tables.is_empty() {
            return;
        }
        self.close_cell();
        self.flush();
        self.push_frame(FrameKind::Cell);
        let depth = self.frames.len() - 1;
        if let Some(table) = self.tables.last_mut() {
            table.cell_depth = Some(depth);
        }
    }

    fn close_table(&mut self) {
        self.close_cell();
        self.flush();
        let Some(table) = self.tables.pop() else {
            return;
        };
        let rows: Vec<Vec<Vec<Block>>> = table
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .filter(|cell| !cell.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        // Layout tables (one cell per row) are common in HTML mail: flatten them.
        if rows.iter().all(|row| row.len() <= 1) {
            for block in rows.into_iter().flatten().flatten() {
                self.push_block(block);
            }
        } else {
            self.push_block(Block::Table(rows));
        }
    }

    fn open(&mut self, name: &str, attrs: &[(String, String)]) {
        match name {
            "br" => self.line_break(),
            "hr" => {
                self.flush();
                self.push_block(Block::Rule);
            }
            "img" => {
                let tiny = ["width", "height"]
                    .iter()
                    .any(|key| matches!(attr(attrs, key).map(str::trim), Some("0" | "1" | "1px")));
                let src = attr(attrs, "src").unwrap_or_default().trim().to_owned();
                if !tiny && !src.is_empty() {
                    self.flush();
                    let alt = attr(attrs, "alt").unwrap_or_default().trim().to_owned();
                    self.push_block(Block::Image { src, alt });
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.heading = name[1..].parse().ok();
            }
            "pre" => {
                self.flush();
                self.pre += 1;
            }
            "blockquote" => {
                self.flush();
                self.push_frame(FrameKind::Quote);
            }
            "ul" | "ol" | "menu" => {
                self.flush();
                let start = attr(attrs, "start")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(1);
                self.lists.push(ListState {
                    ordered: name == "ol",
                    counter: start.saturating_sub(1),
                    item_open: false,
                    items: Vec::new(),
                });
            }
            "li" => self.open_item_tag(),
            "table" => {
                self.flush();
                self.tables.push(TableState {
                    rows: Vec::new(),
                    cell_depth: None,
                });
            }
            "tr" => {
                self.close_cell();
                if let Some(table) = self.tables.last_mut() {
                    table.rows.push(Vec::new());
                }
            }
            "td" | "th" => {
                self.open_cell();
                if name == "th" {
                    self.bold += 1;
                }
            }
            "b" | "strong" => self.bold += 1,
            "i" | "em" | "cite" => self.italic += 1,
            "u" | "ins" => self.underline += 1,
            "code" | "tt" | "kbd" | "samp" => self.code += 1,
            "a" => self.links.push(attr(attrs, "href").and_then(safe_link)),
            _ if BLOCK_TAGS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.heading = None;
            }
            "pre" => {
                self.flush();
                self.pre = self.pre.saturating_sub(1);
            }
            "blockquote" => self.close_quote(),
            "ul" | "ol" | "menu" => self.close_list(),
            "li" => {
                self.flush();
                if let Some(list) = self.lists.last_mut() {
                    list.item_open = false;
                }
            }
            "table" => self.close_table(),
            "tr" => self.close_cell(),
            "td" => self.close_cell(),
            "th" => {
                self.close_cell();
                self.bold = self.bold.saturating_sub(1);
            }
            "b" | "strong" => self.bold = self.bold.saturating_sub(1),
            "i" | "em" | "cite" => self.italic = self.italic.saturating_sub(1),
            "u" | "ins" => self.underline = self.underline.saturating_sub(1),
            "code" | "tt" | "kbd" | "samp" => self.code = self.code.saturating_sub(1),
            "a" => {
                self.links.pop();
            }
            _ if BLOCK_TAGS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        while !self.tables.is_empty() {
            self.close_table();
        }
        while !self.lists.is_empty() {
            self.close_list();
        }
        while self.frames.len() > 1 {
            self.flush();
            let blocks = self.pop_frame();
            if !blocks.is_empty() {
                self.push_block(Block::Quote(blocks));
            }
        }
        self.flush();
        self.frames
            .pop()
            .map(|frame| frame.blocks)
            .unwrap_or_default()
    }
}

/// Parses an HTML mail body into display blocks.
pub fn parse(html: &str) -> Vec<Block> {
    let mut builder = Builder::new();
    for token in tokenize(html) {
        match token {
            Token::Text(text) => builder.text(&text),
            Token::Open { name, attrs } => builder.open(&name, &attrs),
            Token::Close(name) => builder.close(&name),
        }
    }
    builder.finish()
}

/// Splits plain text on URLs, making them links.
fn linkify(line: &str, spans: &mut Vec<Span>) {
    let mut rest = line;
    while !rest.is_empty() {
        let next = ["https://", "http://"]
            .iter()
            .filter_map(|scheme| rest.find(scheme))
            .min();
        let Some(start) = next else {
            push_plain(spans, rest, None);
            break;
        };
        push_plain(spans, &rest[..start], None);
        let url_part = &rest[start..];
        let end = url_part
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .unwrap_or(url_part.len());
        let url = url_part[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
        push_plain(spans, url, Some(url.to_owned()));
        rest = &rest[start + url.len()..];
    }
}

fn push_plain(spans: &mut Vec<Span>, text: &str, link: Option<String>) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.link.is_none() && link.is_none() => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_owned(),
            style: SpanStyle::default(),
            link,
        }),
    }
}

/// Plain-text body: paragraphs on blank lines, `>` lines become quotes, URLs become links.
pub fn parse_plain(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if line.trim_start().starts_with('>') {
            let mut quoted = Vec::new();
            while index < lines.len() && lines[index].trim_start().starts_with('>') {
                let inner = lines[index].trim_start()[1..].strip_prefix(' ');
                quoted.push(inner.unwrap_or(&lines[index].trim_start()[1..]));
                index += 1;
            }
            blocks.push(Block::Quote(parse_plain(&quoted.join("\n"))));
            continue;
        }
        let mut spans = Vec::new();
        while index < lines.len()
            && !lines[index].trim().is_empty()
            && !lines[index].trim_start().starts_with('>')
        {
            if !spans.is_empty() {
                push_plain(&mut spans, "\n", None);
            }
            linkify(lines[index].trim_end(), &mut spans);
            index += 1;
        }
        blocks.push(Block::Paragraph(spans));
    }
    blocks
}

/// Visible text of blocks (copy to clipboard, quoting in replies).
pub fn to_text(blocks: &[Block]) -> String {
    fn spans_text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph(spans) | Block::Heading(_, spans) => parts.push(spans_text(spans)),
            Block::List(items) => parts.push(
                items
                    .iter()
                    .map(|item| {
                        let marker = item.number.map_or("•".to_owned(), |n| format!("{n}."));
                        format!(
                            "{}{marker} {}",
                            "  ".repeat(item.depth),
                            spans_text(&item.spans)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Quote(inner) => parts.push(
                to_text(inner)
                    .lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Table(rows) => parts.push(
                rows.iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| to_text(cell))
                            .collect::<Vec<_>>()
                            .join("\t")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Preformatted(text) => parts.push(text.clone()),
            Block::Image { alt, .. } if !alt.is_empty() => parts.push(format!("[{alt}]")),
            Block::Image { .. } => {}
            Block::Rule => parts.push("---".into()),
        }
    }
    parts.join("\n\n")
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// Callback for a clicked link (already sanitized).
pub type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the renderer needs besides the blocks.
#[derive(Clone)]
pub struct RenderContext {
    pub theme: Theme,
    /// Remote images are loaded only when true.
    pub show_images: bool,
    pub on_link: LinkHandler,
}

/// Host shown after a link when its text doesn't already say where it goes.
fn link_target(href: &str) -> String {
    if let Some(address) = href.strip_prefix("mailto:") {
        return address.split('?').next().unwrap_or(address).to_owned();
    }
    let without_scheme = href.split_once("://").map_or(href, |(_, rest)| rest);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .to_owned()
}

fn render_spans(id: ElementId, spans: &[Span], size: Pixels, ctx: &RenderContext) -> AnyElement {
    let t = &ctx.theme;
    let mut text = String::new();
    let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let range = start..text.len();
        let mut style = HighlightStyle::default();
        if span.style.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
        }
        if span.style.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if span.style.code {
            style.background_color = Some(t.hover);
        }
        if span.style.underline || span.link.is_some() {
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(if span.link.is_some() {
                    t.accent.alpha(0.5)
                } else {
                    t.text
                }),
                wavy: false,
            });
        }
        if let Some(href) = &span.link {
            style.color = Some(t.accent);
            links.push((range.clone(), href.clone()));
        }
        if style != HighlightStyle::default() && !range.is_empty() {
            highlights.push((range, style));
        }
        // Show where a link goes unless its text already says so.
        if let Some(href) = &span.link {
            let target = link_target(href);
            if !target.is_empty() && !span.text.contains(&target) {
                let start = text.len();
                text.push_str(&format!(" ({target})"));
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(t.text_subtle),
                        ..HighlightStyle::default()
                    },
                ));
            }
        }
    }
    let styled = StyledText::new(text).with_highlights(highlights);
    let ranges: Vec<std::ops::Range<usize>> =
        links.iter().map(|(range, _)| range.clone()).collect();
    let hrefs: Rc<Vec<String>> = Rc::new(links.into_iter().map(|(_, href)| href).collect());
    let on_link = ctx.on_link.clone();
    let click_hrefs = hrefs.clone();
    let tooltip_ranges = ranges.clone();
    div()
        .text_size(size)
        .line_height(size * 1.6)
        .child(
            InteractiveText::new(id, styled)
                .on_click(ranges, move |index, window, cx| {
                    if let Some(href) = click_hrefs.get(index) {
                        on_link(href, window, cx);
                    }
                })
                .tooltip(move |index, window, cx| {
                    let position = tooltip_ranges
                        .iter()
                        .position(|range| range.contains(&index))?;
                    let href = hrefs.get(position)?.clone();
                    Some(Tooltip::new(SharedString::from(href)).build(window, cx))
                }),
        )
        .into_any_element()
}

fn child_id(prefix: &str, index: usize) -> String {
    format!("{prefix}.{index}")
}

/// Renders blocks; `prefix` keeps element ids unique per message.
pub fn render_blocks(prefix: &str, blocks: &[Block], ctx: &RenderContext) -> Vec<AnyElement> {
    let t = &ctx.theme;
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let path = child_id(prefix, index);
            let id = ElementId::Name(path.clone().into());
            match block {
                Block::Paragraph(spans) => render_spans(id, spans, TEXT_MD, ctx),
                Block::Heading(level, spans) => {
                    let size = match level {
                        1 => TEXT_XL,
                        2 => TEXT_LG,
                        _ => TEXT_MD,
                    };
                    div()
                        .pt(SPACE_1)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(render_spans(id, spans, size, ctx))
                        .into_any_element()
                }
                Block::List(items) => div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_1)
                    .children(items.iter().enumerate().map(|(item_index, item)| {
                        let marker = item.number.map_or("•".to_owned(), |n| format!("{n}."));
                        let item_id = ElementId::Name(child_id(&path, item_index).into());
                        div()
                            .flex()
                            .flex_row()
                            .gap(SPACE_2)
                            .pl(px(12. + item.depth as f32 * 18.))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .min_w(px(14.))
                                    .text_size(TEXT_MD)
                                    .line_height(TEXT_MD * 1.6)
                                    .text_color(t.text_muted)
                                    .child(marker),
                            )
                            .child(div().flex_1().min_w(px(0.)).child(render_spans(
                                item_id,
                                &item.spans,
                                TEXT_MD,
                                ctx,
                            )))
                    }))
                    .into_any_element(),
                Block::Quote(inner) => div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_2)
                    .pl(SPACE_3)
                    .border_l_2()
                    .border_color(t.border)
                    .text_color(t.text_muted)
                    .children(render_blocks(&path, inner, ctx))
                    .into_any_element(),
                Block::Table(rows) => div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(t.border_subtle)
                    .overflow_hidden()
                    .children(rows.iter().enumerate().map(|(row_index, row)| {
                        let row_path = child_id(&path, row_index);
                        div()
                            .flex()
                            .flex_row()
                            .when(row_index > 0, |el| {
                                el.border_t_1().border_color(t.border_subtle)
                            })
                            .children(row.iter().enumerate().map(|(cell_index, cell)| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .gap(SPACE_1)
                                    .px(SPACE_2)
                                    .py(SPACE_1)
                                    .when(cell_index > 0, |el| {
                                        el.border_l_1().border_color(t.border_subtle)
                                    })
                                    .children(render_blocks(
                                        &child_id(&row_path, cell_index),
                                        cell,
                                        ctx,
                                    ))
                            }))
                    }))
                    .into_any_element(),
                Block::Preformatted(text) => div()
                    .p(SPACE_3)
                    .rounded(RADIUS_MD)
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.border_subtle)
                    .text_size(TEXT_SM)
                    .font_family("monospace")
                    .child(text.clone())
                    .into_any_element(),
                // Inline (`cid:`) pictures resolved to stored attachments: local, always shown.
                Block::Image { src, .. } if crate::attachments::is_inline_file(src) => div()
                    .max_w(px(640.))
                    .child(
                        img(std::path::PathBuf::from(src))
                            .max_w_full()
                            .rounded(RADIUS_MD),
                    )
                    .into_any_element(),
                Block::Image { src, alt } => {
                    if ctx.show_images && is_remote(src) {
                        let source = if src.starts_with("//") {
                            format!("https:{src}")
                        } else {
                            src.clone()
                        };
                        div()
                            .max_w(px(640.))
                            .child(img(source).max_w_full().rounded(RADIUS_MD))
                            .into_any_element()
                    } else {
                        let label = if alt.is_empty() {
                            "Image"
                        } else {
                            alt.as_str()
                        };
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .self_start()
                            .px(SPACE_2)
                            .py(SPACE_1)
                            .rounded(RADIUS_MD)
                            .border_1()
                            .border_color(t.border_subtle)
                            .bg(t.surface)
                            .text_size(TEXT_SM)
                            .text_color(t.text_subtle)
                            .child(Icon::new(IconName::ImageOff).size(px(14.)))
                            .child(label.to_owned())
                            .into_any_element()
                    }
                }
                Block::Rule => div()
                    .h(px(1.))
                    .my(SPACE_1)
                    .bg(t.border_subtle)
                    .into_any_element(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it would pull in gpui's `test` macro through `use gpui::*`.
    use super::{Block, has_remote_images, link_target, parse, parse_plain, to_text};

    fn text(blocks: &[Block]) -> String {
        to_text(blocks)
    }

    #[test]
    fn drops_scripts_styles_and_unsafe_links() {
        let blocks = parse(
            "<html><head><style>p{color:red}</style><title>x</title></head><body>\
             <script>alert(1)</script><p onclick=\"evil()\">Hello <b>world</b></p>\
             <a href=\"javascript:alert(1)\">bad</a> <a href='https://ok.example/x'>good</a>\
             </body></html>",
        );
        assert_eq!(text(&blocks), "Hello world\n\nbad good");
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("expected a paragraph");
        };
        assert!(
            spans
                .iter()
                .all(|span| span.link.as_deref() != Some("javascript:alert(1)"))
        );
        assert!(
            spans
                .iter()
                .any(|span| span.link.as_deref() == Some("https://ok.example/x"))
        );
        let Block::Paragraph(first) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        assert!(
            first
                .iter()
                .any(|span| span.style.bold && span.text == "world")
        );
    }

    #[test]
    fn structure_headings_lists_quotes_tables() {
        let blocks = parse(
            "<h1>Title</h1><ul><li>One</li><li>Two<ol><li>Sub</li></ol></li></ul>\
             <blockquote><p>Quoted &amp; kept</p></blockquote>\
             <table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>\
             <table><tr><td><p>Layout</p></td></tr></table>",
        );
        assert!(matches!(blocks[0], Block::Heading(1, _)));
        let Block::List(items) = &blocks[1] else {
            panic!("expected a list");
        };
        assert_eq!(items.len(), 3);
        assert_eq!((items[2].depth, items[2].number), (1, Some(1)));
        assert!(matches!(&blocks[2], Block::Quote(inner) if text(inner) == "Quoted & kept"));
        assert!(matches!(&blocks[3], Block::Table(rows) if rows.len() == 2 && rows[0].len() == 2));
        assert!(matches!(&blocks[4], Block::Paragraph(_)));
        assert_eq!(text(&blocks[4..]), "Layout");
    }

    #[test]
    fn images_tracking_pixels_and_entities() {
        let blocks = parse(
            "<p>Hi&nbsp;there &#8212; &#x41;</p><img src=\"https://cdn.example/a.png\" alt=\"Logo\">\
             <img src=\"https://t.example/p.gif\" width=\"1\" height=\"1\"><img src=\"cid:part1\">",
        );
        assert_eq!(text(&blocks[..1]), "Hi\u{a0}there — A");
        assert_eq!(
            blocks[1],
            Block::Image {
                src: "https://cdn.example/a.png".into(),
                alt: "Logo".into()
            }
        );
        assert_eq!(blocks.len(), 3);
        assert!(has_remote_images(&blocks));
        assert!(!has_remote_images(&blocks[2..]));
    }

    #[test]
    fn tolerates_broken_markup() {
        let blocks = parse("<p>Open <b>bold <i>both</p><div>after</div><p>tail <");
        assert_eq!(text(&blocks), "Open bold both\n\nafter\n\ntail <");
        assert!(parse("<").len() == 1);
        assert!(parse("<div").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn plain_text_quotes_and_links() {
        let blocks =
            parse_plain("Hi Ada,\nsee https://example.com/doc.\n\n> earlier\n> message\n\nBye");
        assert_eq!(blocks.len(), 3);
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        assert!(
            spans
                .iter()
                .any(|span| span.link.as_deref() == Some("https://example.com/doc"))
        );
        assert!(matches!(&blocks[1], Block::Quote(inner) if text(inner) == "earlier\nmessage"));
    }

    #[test]
    fn link_targets() {
        assert_eq!(
            link_target("https://www.example.com/a?b"),
            "www.example.com"
        );
        assert_eq!(
            link_target("mailto:ada@example.com?subject=x"),
            "ada@example.com"
        );
    }
}
