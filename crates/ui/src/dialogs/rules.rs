//! Rules dialog: ordered list (enable, reorder, delete) and an editor for one rule.
//! Every change is an undoable command; the engine lives in `mailbox_core::rules`.

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputState};
use gpui::component::switch::Switch;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{
    Account, Category, Command, Folder, Id, Importance, Rule, RuleAction, RuleCondition, rules,
};

use super::{Confirm, confirm, dialog_max_height, field, section_title};
use crate::actions::{current_account, run_rules};
use crate::app_state::AppState;
use crate::components::{Banner, Button, EmptyState, Icon, IconButton, IconName};
use crate::settings::controls::segmented;
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_LG,
    TEXT_SM, TEXT_XS, hex, theme,
};

const IMPORTANCE_FILTER: [(&str, Option<Importance>); 4] = [
    ("Any", None),
    ("Low", Some(Importance::Low)),
    ("Normal", Some(Importance::Normal)),
    ("High", Some(Importance::High)),
];

const IMPORTANCE_SET: [(&str, Option<Importance>); 4] = [
    ("Unchanged", None),
    ("Low", Some(Importance::Low)),
    ("Normal", Some(Importance::Normal)),
    ("High", Some(Importance::High)),
];

/// Editor fields; text conditions live in the inputs.
#[derive(Debug, Clone, Default, PartialEq)]
struct Draft {
    has_attachment: bool,
    importance: Option<Importance>,
    move_to: Option<Id>,
    category: Option<String>,
    mark_read: bool,
    flag: bool,
    set_importance: Option<Importance>,
    delete: bool,
    stop: bool,
    enabled: bool,
}

pub struct RulesDialog {
    state: Entity<AppState>,
    account_id: Option<Id>,
    /// Rule in the editor: `Some(0)` for a new rule, `None` when the editor is closed.
    editing: Option<Id>,
    name: Entity<InputState>,
    from: Entity<InputState>,
    to: Entity<InputState>,
    subject: Entity<InputState>,
    body: Entity<InputState>,
    draft: Draft,
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl RulesDialog {
    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, placeholder)))
        };
        let account_id = current_account(&state, cx);
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            account_id,
            editing: None,
            name: input("Rule name", window, cx),
            from: input("Sender name or address", window, cx),
            to: input("Recipient address", window, cx),
            subject: input("Words in the subject", window, cx),
            body: input("Words in the message", window, cx),
            draft: Draft::default(),
            error: None,
            state,
            _subscription: subscription,
        }
    }

    fn text_inputs(&self) -> [&Entity<InputState>; 5] {
        [&self.name, &self.from, &self.to, &self.subject, &self.body]
    }

    /// Loads `rule` (or a blank rule) into the editor.
    fn edit(&mut self, rule: Option<Rule>, window: &mut Window, cx: &mut Context<Self>) {
        let rule = rule.unwrap_or(Rule {
            id: 0,
            account_id: self.account_id.unwrap_or_default(),
            name: String::new(),
            enabled: true,
            conditions: Vec::new(),
            actions: Vec::new(),
        });
        let mut texts: [String; 5] = Default::default();
        texts[0] = rule.name.clone();
        let mut draft = Draft {
            enabled: rule.enabled,
            ..Draft::default()
        };
        for condition in &rule.conditions {
            match condition {
                RuleCondition::FromContains(text) => texts[1] = text.clone(),
                RuleCondition::ToContains(text) => texts[2] = text.clone(),
                RuleCondition::SubjectContains(text) => texts[3] = text.clone(),
                RuleCondition::BodyContains(text) => texts[4] = text.clone(),
                RuleCondition::HasAttachment => draft.has_attachment = true,
                RuleCondition::ImportanceIs(importance) => draft.importance = Some(*importance),
            }
        }
        for action in &rule.actions {
            match action {
                RuleAction::MoveTo(folder) => draft.move_to = Some(*folder),
                RuleAction::AddCategory(name) => draft.category = Some(name.clone()),
                RuleAction::MarkRead => draft.mark_read = true,
                RuleAction::Flag => draft.flag = true,
                RuleAction::SetImportance(importance) => draft.set_importance = Some(*importance),
                RuleAction::Delete => draft.delete = true,
                RuleAction::StopProcessing => draft.stop = true,
            }
        }
        let inputs = self.text_inputs().map(Entity::clone);
        for (input, text) in inputs.iter().zip(texts) {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        self.draft = draft;
        self.editing = Some(rule.id);
        self.error = None;
        let name = self.name.clone();
        name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn build_rule(&self, cx: &App) -> Result<Rule, &'static str> {
        let account_id = self.account_id.ok_or("Add an account first")?;
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let draft = &self.draft;
        let mut conditions = Vec::new();
        for (value, make) in [
            (
                text(&self.from),
                RuleCondition::FromContains as fn(String) -> RuleCondition,
            ),
            (text(&self.to), RuleCondition::ToContains),
            (text(&self.subject), RuleCondition::SubjectContains),
            (text(&self.body), RuleCondition::BodyContains),
        ] {
            if !value.is_empty() {
                conditions.push(make(value));
            }
        }
        if draft.has_attachment {
            conditions.push(RuleCondition::HasAttachment);
        }
        if let Some(importance) = draft.importance {
            conditions.push(RuleCondition::ImportanceIs(importance));
        }
        let mut actions = Vec::new();
        if let Some(folder) = draft.move_to {
            actions.push(RuleAction::MoveTo(folder));
        }
        if let Some(category) = &draft.category {
            actions.push(RuleAction::AddCategory(category.clone()));
        }
        if draft.mark_read {
            actions.push(RuleAction::MarkRead);
        }
        if draft.flag {
            actions.push(RuleAction::Flag);
        }
        if let Some(importance) = draft.set_importance {
            actions.push(RuleAction::SetImportance(importance));
        }
        if draft.delete {
            actions.push(RuleAction::Delete);
        }
        if conditions.is_empty() {
            return Err("Add at least one condition");
        }
        if actions.is_empty() {
            return Err("Add at least one action");
        }
        if draft.stop {
            actions.push(RuleAction::StopProcessing);
        }
        let name = match text(&self.name) {
            name if name.is_empty() => "Untitled rule".to_owned(),
            name => name,
        };
        Ok(Rule {
            id: self.editing.unwrap_or(0),
            account_id,
            name,
            enabled: draft.enabled,
            conditions,
            actions,
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let rule = match self.build_rule(cx) {
            Ok(rule) => rule,
            Err(message) => {
                self.error = Some(tr(cx, message).into());
                return cx.notify();
            }
        };
        let is_new = rule.id == 0;
        let account_id = rule.account_id;
        let saved = self.state.update(cx, |state, cx| {
            let saved = state.run(Command::SaveRule(Box::new(rule)), cx);
            if saved {
                state.notify_info(tr(cx, "Rule saved"), cx);
            }
            saved
        });
        if saved && is_new {
            // Show the created rule (highest id of the account) in the editor.
            self.editing = self
                .state
                .read(cx)
                .store()
                .list_rules(account_id)
                .ok()
                .and_then(|rules| rules.iter().map(|rule| rule.id).max());
        }
        self.error = None;
        cx.notify();
    }

    fn set_enabled(&mut self, rule: &Rule, enabled: bool, cx: &mut Context<Self>) {
        let rule = Rule {
            enabled,
            ..rule.clone()
        };
        if self.editing == Some(rule.id) {
            self.draft.enabled = enabled;
        }
        self.state.update(cx, |state, cx| {
            state.run(Command::SaveRule(Box::new(rule)), cx);
        });
    }

    fn reorder(&mut self, ids: &[Id], from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(account_id) = self.account_id else {
            return;
        };
        if to >= ids.len() {
            return;
        }
        let mut ids = ids.to_vec();
        ids.swap(from, to);
        self.state.update(cx, |state, cx| {
            match rules::reorder_command(account_id, &ids) {
                Ok(command) => {
                    state.run(command, cx);
                }
                Err(err) => state.report_error(err, cx),
            }
        });
    }

    fn delete(&mut self, rule: &Rule, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let view = cx.entity().downgrade();
        let id = rule.id;
        confirm(
            Confirm {
                title: tr(cx, "Delete rule?").into(),
                description: tr(cx, "“{}” will be removed.")
                    .replace("{}", &rule.name)
                    .into(),
                ok: tr(cx, "Delete").into(),
                danger: true,
            },
            move |_, cx| {
                state.update(cx, |state, cx| {
                    state.run(Command::DeleteRule(id), cx);
                });
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        if this.editing == Some(id) {
                            this.editing = None;
                            cx.notify();
                        }
                    });
                }
            },
            window,
            cx,
        );
    }

    fn switch(
        &self,
        id: &'static str,
        checked: bool,
        set: fn(&mut Draft, bool),
        cx: &mut Context<Self>,
    ) -> Switch {
        Switch::new(id).checked(checked).on_click(cx.listener(
            move |this, checked: &bool, _, cx| {
                set(&mut this.draft, *checked);
                cx.notify();
            },
        ))
    }

    fn render_list(&self, rules: &[Rule], cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx).clone();
        let ids: Vec<Id> = rules.iter().map(|rule| rule.id).collect();
        let rows = rules.iter().enumerate().map(|(index, rule)| {
            let selected = self.editing == Some(rule.id);
            let id = rule.id as u64;
            let (up_ids, down_ids) = (ids.clone(), ids.clone());
            let (enable_rule, delete_rule, edit_rule) = (rule.clone(), rule.clone(), rule.clone());
            div()
                .id(("rule", id))
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_2)
                .p(SPACE_2)
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .when(selected, |el| el.bg(t.accent_subtle))
                .when(!selected, |el| el.hover(|style| style.bg(t.hover)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.edit(Some(edit_rule.clone()), window, cx)
                }))
                .child(
                    Switch::new(("rule-enabled", id))
                        .checked(rule.enabled)
                        .on_click(cx.listener(move |this, enabled: &bool, _, cx| {
                            this.set_enabled(&enable_rule, *enabled, cx)
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .when(!rule.enabled, |el| el.text_color(t.text_subtle))
                                .child(rule.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_XS)
                                .text_color(t.text_muted)
                                .child(rules::describe(rule)),
                        ),
                )
                .child(
                    IconButton::new(("rule-up", id), IconName::ArrowUp)
                        .size(px(22.))
                        .icon_size(ICON_SIZE_SM)
                        .disabled(index == 0)
                        .tooltip(tr(cx, "Run earlier"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reorder(&up_ids, index, index.saturating_sub(1), cx)
                        })),
                )
                .child(
                    IconButton::new(("rule-down", id), IconName::ArrowDown)
                        .size(px(22.))
                        .icon_size(ICON_SIZE_SM)
                        .disabled(index + 1 == rules.len())
                        .tooltip(tr(cx, "Run later"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reorder(&down_ids, index, index + 1, cx)
                        })),
                )
                .child(
                    IconButton::new(("rule-delete", id), IconName::Trash)
                        .size(px(22.))
                        .icon_size(ICON_SIZE_SM)
                        .tooltip(tr(cx, "Delete"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete(&delete_rule, window, cx)
                        })),
                )
        });
        div()
            .id("rules-list")
            .flex()
            .flex_col()
            .gap(SPACE_1)
            .overflow_y_scroll()
            .children(rows)
            .when(rules.is_empty(), |el| {
                el.child(
                    div()
                        .p(SPACE_3)
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(tr(
                            cx,
                            "No rules yet. Rules file, flag or categorize mail automatically.",
                        )),
                )
            })
    }

    fn render_editor(
        &self,
        folders: &[Folder],
        categories: &[Category],
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let draft = &self.draft;
        let view = cx.entity().downgrade();
        let set_draft = move |change: Box<dyn FnOnce(&mut Draft)>, cx: &mut App| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| {
                    change(&mut this.draft);
                    cx.notify();
                });
            }
        };
        let set_importance_filter = {
            let set_draft = set_draft.clone();
            move |value: Option<Importance>, _: &mut Window, cx: &mut App| {
                set_draft(Box::new(move |draft| draft.importance = value), cx)
            }
        };
        let set_importance_action = {
            let set_draft = set_draft.clone();
            move |value: Option<Importance>, _: &mut Window, cx: &mut App| {
                set_draft(Box::new(move |draft| draft.set_importance = value), cx)
            }
        };

        let chip = |id: ElementId, label: SharedString, color: Option<u32>, selected: bool| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_1)
                .h(px(24.))
                .px(SPACE_2)
                .rounded(RADIUS_FULL)
                .border_1()
                .text_size(TEXT_SM)
                .cursor_pointer()
                .when(selected, |el| {
                    el.border_color(t.accent)
                        .bg(t.accent_subtle)
                        .text_color(t.accent)
                })
                .when(!selected, |el| {
                    el.border_color(t.border)
                        .text_color(t.text_muted)
                        .hover(|style| style.bg(t.hover))
                })
                .when_some(color, |el, color| {
                    el.child(div().size(px(8.)).rounded(RADIUS_FULL).bg(hex(color)))
                })
                .child(label)
        };

        let folder_chips = std::iter::once((None, tr(cx, "Don't move").to_owned()))
            .chain(
                folders
                    .iter()
                    .map(|folder| (Some(folder.id), folder.name.clone())),
            )
            .map(|(folder, name)| {
                let set_draft = set_draft.clone();
                chip(
                    ElementId::NamedInteger("move-to".into(), folder.unwrap_or(0) as u64),
                    name.into(),
                    None,
                    draft.move_to == folder,
                )
                .on_click(move |_, _, cx| {
                    set_draft(Box::new(move |draft| draft.move_to = folder), cx)
                })
            })
            .collect::<Vec<_>>();
        let category_chips = std::iter::once((None, tr(cx, "None").to_owned(), None))
            .chain(categories.iter().map(|category| {
                (
                    Some(category.name.clone()),
                    category.name.clone(),
                    Some(category.color),
                )
            }))
            .enumerate()
            .map(|(index, (value, label, color))| {
                let set_draft = set_draft.clone();
                let selected = draft.category == value;
                chip(
                    ElementId::NamedInteger("category".into(), index as u64),
                    label.into(),
                    color,
                    selected,
                )
                .on_click(move |_, _, cx| {
                    let value = value.clone();
                    set_draft(Box::new(move |draft| draft.category = value), cx)
                })
            })
            .collect::<Vec<_>>();

        let toggle = |label: &'static str, switch: Switch| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .py(SPACE_1)
                .text_size(TEXT_SM)
                .child(label)
                .child(switch)
        };

        div()
            .flex()
            .flex_col()
            .gap(SPACE_4)
            .child(field(tr(cx, "Name"), Input::new(&self.name), cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_2)
                    .child(section_title(
                        tr(cx, "WHEN A MESSAGE ARRIVES THAT MATCHES ALL OF"),
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(SPACE_2)
                            .child(div().flex_1().child(field(
                                tr(cx, "From contains"),
                                Input::new(&self.from),
                                cx,
                            )))
                            .child(div().flex_1().child(field(
                                tr(cx, "To contains"),
                                Input::new(&self.to),
                                cx,
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(SPACE_2)
                            .child(div().flex_1().child(field(
                                tr(cx, "Subject contains"),
                                Input::new(&self.subject),
                                cx,
                            )))
                            .child(div().flex_1().child(field(
                                tr(cx, "Body contains"),
                                Input::new(&self.body),
                                cx,
                            ))),
                    )
                    .child(toggle(
                        tr(cx, "Has an attachment"),
                        self.switch(
                            "has-attachment",
                            draft.has_attachment,
                            |d, v| d.has_attachment = v,
                            cx,
                        ),
                    ))
                    .child(field(
                        tr(cx, "Importance is"),
                        segmented(
                            "importance-filter",
                            &IMPORTANCE_FILTER.map(|(label, value)| (tr(cx, label), value)),
                            draft.importance,
                            set_importance_filter,
                            cx,
                        ),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_2)
                    .child(section_title(tr(cx, "DO THE FOLLOWING"), cx))
                    .child(field(
                        tr(cx, "Move to folder"),
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(SPACE_1)
                            .children(folder_chips),
                        cx,
                    ))
                    .child(field(
                        tr(cx, "Categorize"),
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(SPACE_1)
                            .children(category_chips),
                        cx,
                    ))
                    .child(toggle(
                        tr(cx, "Mark as read"),
                        self.switch("mark-read", draft.mark_read, |d, v| d.mark_read = v, cx),
                    ))
                    .child(toggle(
                        tr(cx, "Flag"),
                        self.switch("flag", draft.flag, |d, v| d.flag = v, cx),
                    ))
                    .child(field(
                        tr(cx, "Set importance"),
                        segmented(
                            "importance-set",
                            &IMPORTANCE_SET.map(|(label, value)| (tr(cx, label), value)),
                            draft.set_importance,
                            set_importance_action,
                            cx,
                        ),
                        cx,
                    ))
                    .child(toggle(
                        tr(cx, "Delete (move to Trash)"),
                        self.switch("delete", draft.delete, |d, v| d.delete = v, cx),
                    ))
                    .child(toggle(
                        tr(cx, "Stop processing more rules"),
                        self.switch("stop", draft.stop, |d, v| d.stop = v, cx),
                    )),
            )
    }
}

impl Render for RulesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (accounts, rules, folders, categories): (
            Vec<Account>,
            Vec<Rule>,
            Vec<Folder>,
            Vec<Category>,
        ) = {
            let store = self.state.read(cx).store();
            let accounts = store.list_accounts().unwrap_or_default();
            match self.account_id {
                Some(account) => (
                    accounts,
                    rules::ordered_rules(store, account).unwrap_or_default(),
                    store.list_folders(account).unwrap_or_default(),
                    store.list_categories().unwrap_or_default(),
                ),
                None => (accounts, Vec::new(), Vec::new(), Vec::new()),
            }
        };

        let account_tabs = (accounts.len() > 1).then(|| {
            div()
                .flex()
                .flex_row()
                .gap(SPACE_1)
                .children(accounts.iter().map(|account| {
                    let account_id = account.id;
                    let selected = self.account_id == Some(account_id);
                    div()
                        .id(("rules-account", account_id as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .h(px(26.))
                        .px(SPACE_2)
                        .rounded(RADIUS_MD)
                        .text_size(TEXT_SM)
                        .cursor_pointer()
                        .when(selected, |el| el.bg(t.accent_subtle).text_color(t.accent))
                        .when(!selected, |el| {
                            el.text_color(t.text_muted).hover(|style| style.bg(t.hover))
                        })
                        .child(account.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.account_id = Some(account_id);
                            this.editing = None;
                            cx.notify();
                        }))
                }))
        });

        let editor = match self.editing {
            Some(_) => self
                .render_editor(&folders, &categories, cx)
                .into_any_element(),
            None => div()
                .size_full()
                .child(
                    EmptyState::new(IconName::ListFilter, tr(cx, "Select a rule"))
                        .description(tr(cx, "Pick a rule to edit it, or create a new one.")),
                )
                .into_any_element(),
        };

        let editing = self.editing.is_some();
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(
                        Icon::new(IconName::ListFilter)
                            .size(px(18.))
                            .text_color(t.accent),
                    )
                    .child(
                        div()
                            .text_size(TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr(cx, "Rules")),
                    )
                    .children(account_tabs)
                    .child(div().flex_1())
                    // Clear the dialog's close button in the top-right corner.
                    .pr(px(28.))
                    .child(
                        Button::new("rules-run")
                            .ghost()
                            .icon(IconName::WandSparkles)
                            .label(tr(cx, "Run on current folder"))
                            .disabled(rules.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| run_rules(&this.state, cx))),
                    )
                    .child(
                        Button::new("rules-new")
                            .primary()
                            .icon(IconName::Plus)
                            .label(tr(cx, "New rule"))
                            .disabled(self.account_id.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.edit(None, window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_3)
                    // Header, messages and footer stay visible in a short window.
                    .h(px(500.).min((dialog_max_height(window) - px(140.)).max(px(160.))))
                    .child(
                        div()
                            .w(px(300.))
                            .flex_shrink_0()
                            .h_full()
                            .p(SPACE_1)
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(t.border_subtle)
                            .bg(t.surface)
                            .child(self.render_list(&rules, cx)),
                    )
                    .child(
                        div()
                            .id("rule-editor")
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .overflow_y_scroll()
                            .pr(SPACE_1)
                            .child(editor),
                    ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(Banner::error("rules-error", error))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .when(editing, |el| {
                        el.child(
                            Switch::new("rule-draft-enabled")
                                .checked(self.draft.enabled)
                                .on_click(cx.listener(|this, enabled: &bool, _, cx| {
                                    this.draft.enabled = *enabled;
                                    cx.notify();
                                })),
                        )
                        .child(div().text_size(TEXT_SM).child(tr(cx, "Enabled")))
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("rules-close")
                            .label(tr(cx, "Close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .when(editing, |el| {
                        el.child(
                            Button::new("rules-save")
                                .primary()
                                .icon(IconName::Check)
                                .label(tr(cx, "Save"))
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                        )
                    }),
            )
    }
}

/// Opens the rules dialog for the current account.
pub fn open(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| RulesDialog::new(state, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.w(px(860.)).child(view.clone())
    });
}
