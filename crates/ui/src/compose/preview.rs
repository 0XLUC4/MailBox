//! Read-only rendering of the body markup, as the recipient will see it.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::compose::markup::{self, Block, BlockKind};

use crate::theme::{SPACE_1, SPACE_3, SPACE_4, Theme, hex};

/// Renders every line of `source` with its formatting (font sizes are kept for the HTML only).
pub fn render(source: &str, t: &Theme) -> Div {
    let blocks = markup::parse(source);
    div()
        .flex()
        .flex_col()
        .gap(SPACE_1)
        .children(blocks.iter().map(|block| render_block(block, t)))
}

fn render_block(block: &Block, t: &Theme) -> AnyElement {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for span in &block.spans {
        let start = text.len();
        text.push_str(&span.text);
        let style = &span.style;
        let is_link = style.link.is_some();
        let line = || UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: false,
        };
        let highlight = HighlightStyle {
            color: if is_link {
                Some(t.accent)
            } else {
                style.color.map(hex)
            },
            font_weight: style.bold.then_some(FontWeight::BOLD),
            font_style: style.italic.then_some(FontStyle::Italic),
            background_color: style.highlight.map(hex),
            underline: (style.underline || is_link).then(line),
            strikethrough: style.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            fade_out: None,
        };
        if highlight != HighlightStyle::default() {
            highlights.push((start..text.len(), highlight));
        }
    }
    let marker = match block.kind {
        BlockKind::Paragraph => None,
        BlockKind::Bullet => Some("•".to_owned()),
        BlockKind::Numbered(number) => Some(format!("{number}.")),
    };
    // An empty line keeps its height.
    let content = if text.is_empty() {
        div().h(px(18.)).into_any_element()
    } else {
        StyledText::new(text)
            .with_highlights(highlights)
            .into_any_element()
    };
    let indent = SPACE_4 * (block.depth as f32 + if marker.is_some() { 1. } else { 0. });
    let mut line = div()
        .flex()
        .flex_row()
        .items_start()
        .pl(indent)
        .when_some(marker, |el, marker| {
            el.child(
                div()
                    .flex_shrink_0()
                    .w(px(20.))
                    .ml(-px(20.))
                    .text_color(t.text_muted)
                    .child(marker),
            )
        })
        .child(div().flex_1().min_w(px(0.)).child(content))
        .into_any_element();
    for _ in 0..block.quote {
        line = div()
            .pl(SPACE_3)
            .border_l_2()
            .border_color(t.border)
            .text_color(t.text_muted)
            .child(line)
            .into_any_element();
    }
    line
}
