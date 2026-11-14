//! Small form controls shared by settings and dialogs: segmented choice, stepper, setting row,
//! colour swatches.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::components::{Icon, IconButton, IconName};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM,
    hex, theme,
};

type SelectHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

/// Mutually exclusive options shown as a pill group.
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: impl Into<SharedString>,
    options: &[(&'static str, T)],
    current: T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let t = theme(cx).clone();
    let id: SharedString = id.into();
    let on_select: SelectHandler<T> = Rc::new(on_select);
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .p(SPACE_0_5)
        .gap(SPACE_0_5)
        .rounded(RADIUS_MD)
        .bg(t.active)
        .children(options.iter().enumerate().map(|(index, &(label, value))| {
            let selected = value == current;
            let on_select = on_select.clone();
            let hover = t.hover;
            div()
                .id(ElementId::NamedInteger(id.clone(), index as u64))
                .px(SPACE_2)
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(RADIUS_MD)
                .text_size(TEXT_SM)
                .cursor_pointer()
                .when(selected, |el| {
                    el.bg(t.elevated)
                        .text_color(t.text)
                        .font_weight(FontWeight::MEDIUM)
                        .shadow_xs()
                })
                .when(!selected, |el| {
                    el.text_color(t.text_muted)
                        .hover(move |style| style.bg(hover))
                })
                .child(label)
                .on_click(move |_, window, cx| on_select(value, window, cx))
        }))
}

/// `- value +` control for small integer ranges.
pub fn stepper(
    id: impl Into<SharedString>,
    value: String,
    can_decrease: bool,
    can_increase: bool,
    on_step: impl Fn(i32, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let id: SharedString = id.into();
    let on_step: Rc<dyn Fn(i32, &mut Window, &mut App)> = Rc::new(on_step);
    let down = on_step.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(SPACE_1)
        .child(
            IconButton::new(ElementId::NamedInteger(id.clone(), 0), IconName::Minus)
                .size(px(24.))
                .icon_size(ICON_SIZE_SM)
                .disabled(!can_decrease)
                .on_click(move |_, window, cx| down(-1, window, cx)),
        )
        .child(
            div()
                .min_w(px(52.))
                .text_center()
                .text_size(TEXT_SM)
                .text_color(theme(cx).text)
                .child(value),
        )
        .child(
            IconButton::new(ElementId::NamedInteger(id, 1), IconName::Plus)
                .size(px(24.))
                .icon_size(ICON_SIZE_SM)
                .disabled(!can_increase)
                .on_click(move |_, window, cx| on_step(1, window, cx)),
        )
}

/// Settings line: title + optional description on the left, control on the right.
pub fn setting_row(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(SPACE_3)
        .py(SPACE_3)
        .border_b_1()
        .border_color(t.border_subtle)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(SPACE_0_5)
                .child(div().text_size(TEXT_MD).child(title.into()))
                .when_some(description, |el, description| {
                    el.child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child(description),
                    )
                }),
        )
        .child(control)
}

/// Round colour choices; the selected one is ringed.
pub fn swatches(
    id: impl Into<SharedString>,
    colors: &[(&'static str, u32)],
    current: u32,
    on_select: impl Fn(u32, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let t = theme(cx).clone();
    let id: SharedString = id.into();
    let on_select: SelectHandler<u32> = Rc::new(on_select);
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(SPACE_2)
        .children(colors.iter().enumerate().map(|(index, &(name, color))| {
            let selected = color == current;
            let on_select = on_select.clone();
            div()
                .id(ElementId::NamedInteger(id.clone(), index as u64))
                .flex()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(RADIUS_FULL)
                .border_2()
                .border_color(if selected {
                    t.text
                } else {
                    t.background.alpha(0.)
                })
                .cursor_pointer()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(18.))
                        .rounded(RADIUS_FULL)
                        .bg(hex(color))
                        .text_color(t.on_accent)
                        .when(selected, |el| {
                            el.child(Icon::new(IconName::Check).size(px(12.)))
                        }),
                )
                .tooltip(move |window, cx| {
                    gpui::component::tooltip::Tooltip::new(name).build(window, cx)
                })
                .on_click(move |_, window, cx| on_select(color, window, cx))
        }))
}
