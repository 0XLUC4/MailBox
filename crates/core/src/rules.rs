//! Mail rules engine: ordered rules, each "when every condition matches, run every action".
//!
//! - New mail (sync): [`process_incoming`] mutates a message before it is saved.
//! - On demand (UI): [`run_command`] builds one undoable [`Command`] for existing messages.
//!
//! Order lives in the `rules.order.<account>` setting (rules missing from it run last, by id).

use std::collections::HashMap;

use crate::command::Command;
use crate::error::Result;
use crate::model::{Folder, FolderRole, Id, Message, Rule, RuleAction};
use crate::settings;
use crate::store::Store;

fn order_key(account_id: Id) -> String {
    format!("rules.order.{account_id}")
}

/// Rules of an account in execution order.
pub fn ordered_rules(store: &Store, account_id: Id) -> Result<Vec<Rule>> {
    let order: Vec<Id> = settings::get_json(store, &order_key(account_id))?.unwrap_or_default();
    let position: HashMap<Id, usize> = order
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect();
    let mut rules = store.list_rules(account_id)?;
    rules.sort_by_key(|rule| {
        (
            position.get(&rule.id).copied().unwrap_or(usize::MAX),
            rule.id,
        )
    });
    Ok(rules)
}

/// Command that stores a new execution order for an account's rules.
pub fn reorder_command(account_id: Id, rule_ids: &[Id]) -> Result<Command> {
    settings::set_json_command(order_key(account_id), &rule_ids)
}

/// What the rules did to a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Number of rules that matched.
    pub matched: usize,
    /// A `Delete` action ran: the caller moves the message to Trash (or drops it).
    pub delete: bool,
}

/// Runs `rules` in order on `message`, applying their actions in place.
/// `MoveTo` only changes `folder_id`: callers fix `account_id` for cross-account moves.
pub fn apply(rules: &[Rule], message: &mut Message) -> Outcome {
    let mut outcome = Outcome::default();
    for rule in rules {
        if !rule.matches(message) {
            continue;
        }
        outcome.matched += 1;
        let mut stop = false;
        for action in &rule.actions {
            match action {
                RuleAction::MoveTo(folder_id) => message.folder_id = *folder_id,
                RuleAction::AddCategory(name) => {
                    if !message
                        .categories
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(name))
                    {
                        message.categories.push(name.clone());
                    }
                }
                RuleAction::MarkRead => message.flags.read = true,
                RuleAction::Flag => message.flags.flagged = true,
                RuleAction::SetImportance(importance) => message.importance = *importance,
                RuleAction::Delete => outcome.delete = true,
                RuleAction::StopProcessing => stop = true,
            }
        }
        if stop {
            break;
        }
    }
    outcome
}

/// Trash folder of the account, if any.
fn trash(store: &Store, account_id: Id) -> Result<Option<Folder>> {
    store.folder_by_role(account_id, FolderRole::Trash)
}

/// Applies the account's rules to an incoming (not yet saved) message.
/// Returns `false` when the message should be dropped (deleted with no Trash folder).
pub fn process_incoming(store: &Store, message: &mut Message) -> Result<bool> {
    let rules = ordered_rules(store, message.account_id)?;
    let outcome = apply(&rules, message);
    if outcome.delete {
        match trash(store, message.account_id)? {
            Some(trash) => message.folder_id = trash.id,
            None => return Ok(false),
        }
    }
    if let Some(folder) = store.get::<Folder>(message.folder_id)? {
        message.account_id = folder.account_id;
    }
    Ok(true)
}

/// Runs the rules of `account_id` on existing messages. Returns one undoable command
/// (`None` when no rule changed anything) and the number of messages changed.
pub fn run_command(
    store: &Store,
    account_id: Id,
    message_ids: &[Id],
) -> Result<(Option<Command>, usize)> {
    let rules = ordered_rules(store, account_id)?;
    let trash = trash(store, account_id)?;
    let mut commands = Vec::new();
    for &id in message_ids {
        let original = store.require::<Message>(id)?;
        let mut message = original.clone();
        let outcome = apply(&rules, &mut message);
        if outcome.delete {
            // Soft delete; a message already in Trash stays there (never purged by a rule run).
            match &trash {
                Some(trash) => message.folder_id = trash.id,
                None => {
                    commands.push(Command::DeleteMessages { ids: vec![id] });
                    continue;
                }
            }
        }
        if message != original {
            if message.folder_id != original.folder_id {
                message.account_id = store.require::<Folder>(message.folder_id)?.account_id;
            }
            commands.push(Command::SaveMessage(Box::new(message)));
        }
    }
    let changed = commands.len();
    Ok((
        (!commands.is_empty()).then_some(Command::Batch(commands)),
        changed,
    ))
}

/// Short human summary of a rule, e.g. "From contains ada → Move, Flag".
pub fn describe(rule: &Rule) -> String {
    use crate::model::RuleCondition as C;
    let conditions: Vec<String> = rule
        .conditions
        .iter()
        .map(|condition| match condition {
            C::FromContains(text) => format!("from contains \"{text}\""),
            C::ToContains(text) => format!("to contains \"{text}\""),
            C::SubjectContains(text) => format!("subject contains \"{text}\""),
            C::BodyContains(text) => format!("body contains \"{text}\""),
            C::HasAttachment => "has attachment".to_owned(),
            C::ImportanceIs(importance) => format!("importance is {}", importance.as_str()),
        })
        .collect();
    let actions: Vec<String> = rule
        .actions
        .iter()
        .map(|action| match action {
            RuleAction::MoveTo(_) => "move".to_owned(),
            RuleAction::AddCategory(name) => format!("categorize \"{name}\""),
            RuleAction::MarkRead => "mark read".to_owned(),
            RuleAction::Flag => "flag".to_owned(),
            RuleAction::SetImportance(importance) => {
                format!("set importance {}", importance.as_str())
            }
            RuleAction::Delete => "delete".to_owned(),
            RuleAction::StopProcessing => "stop".to_owned(),
        })
        .collect();
    let when = if conditions.is_empty() {
        "never".to_owned()
    } else {
        conditions.join(" and ")
    };
    format!("When {when} → {}", actions.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::model::{Importance, RuleCondition};
    use crate::store::tests::{account, folder, message, store};

    fn rule(account_id: Id, conditions: Vec<RuleCondition>, actions: Vec<RuleAction>) -> Rule {
        Rule {
            id: 0,
            account_id,
            name: "test".into(),
            enabled: true,
            conditions,
            actions,
        }
    }

    #[test]
    fn conditions_and_actions() {
        let mut msg = message(1, 10);
        msg.body_text = "Please find the invoice attached".into();
        let rules = vec![
            rule(
                1,
                vec![
                    RuleCondition::FromContains("ADA".into()),
                    RuleCondition::BodyContains("invoice".into()),
                    RuleCondition::HasAttachment,
                    RuleCondition::ImportanceIs(Importance::High),
                ],
                vec![
                    RuleAction::MoveTo(20),
                    RuleAction::AddCategory("Finance".into()),
                    RuleAction::MarkRead,
                    RuleAction::Flag,
                    RuleAction::SetImportance(Importance::Low),
                ],
            ),
            // Does not match: subject differs.
            rule(
                1,
                vec![RuleCondition::SubjectContains("nope".into())],
                vec![RuleAction::Delete],
            ),
        ];
        let outcome = apply(&rules, &mut msg);
        assert_eq!(
            outcome,
            Outcome {
                matched: 1,
                delete: false
            }
        );
        assert_eq!(msg.folder_id, 20);
        assert_eq!(
            msg.categories,
            vec!["Work".to_owned(), "Finance".to_owned()]
        );
        assert!(msg.flags.read && msg.flags.flagged);
        assert_eq!(msg.importance, Importance::Low);
    }

    #[test]
    fn stop_processing_and_disabled_rules() {
        let mut msg = message(1, 10);
        let mut disabled = rule(
            1,
            vec![RuleCondition::HasAttachment],
            vec![RuleAction::Delete],
        );
        disabled.enabled = false;
        let rules = vec![
            disabled,
            rule(
                1,
                vec![RuleCondition::ToContains("me@".into())],
                vec![RuleAction::Flag, RuleAction::StopProcessing],
            ),
            rule(
                1,
                vec![RuleCondition::HasAttachment],
                vec![RuleAction::Delete],
            ),
        ];
        let outcome = apply(&rules, &mut msg);
        assert_eq!(
            outcome,
            Outcome {
                matched: 1,
                delete: false
            }
        );
        assert!(msg.flags.flagged);
    }

    #[test]
    fn order_setting_and_on_demand_run_is_undoable() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let trash = folder(&store, account.id, FolderRole::Trash);
        let mut msg = message(account.id, inbox.id);
        store.save(&mut msg).unwrap();

        let mut flag = rule(
            account.id,
            vec![RuleCondition::FromContains("ada".into())],
            vec![RuleAction::Flag, RuleAction::StopProcessing],
        );
        let mut delete = rule(
            account.id,
            vec![RuleCondition::SubjectContains("hello".into())],
            vec![RuleAction::Delete],
        );
        store.save(&mut flag).unwrap();
        store.save(&mut delete).unwrap();

        let mut history = History::new();
        // Default order (by id): flag runs first and stops.
        let (command, changed) = run_command(&store, account.id, &[msg.id]).unwrap();
        assert_eq!(changed, 1);
        history.run(&store, command.unwrap()).unwrap();
        let after = store.require::<Message>(msg.id).unwrap();
        assert!(after.flags.flagged);
        assert_eq!(after.folder_id, inbox.id);
        history.undo(&store).unwrap();
        assert_eq!(store.require::<Message>(msg.id).unwrap(), msg);

        // Reordered: delete first -> moved to trash.
        history
            .run(
                &store,
                reorder_command(account.id, &[delete.id, flag.id]).unwrap(),
            )
            .unwrap();
        let order: Vec<Id> = ordered_rules(&store, account.id)
            .unwrap()
            .iter()
            .map(|rule| rule.id)
            .collect();
        assert_eq!(order, vec![delete.id, flag.id]);
        let (command, _) = run_command(&store, account.id, &[msg.id]).unwrap();
        history.run(&store, command.unwrap()).unwrap();
        assert_eq!(
            store.require::<Message>(msg.id).unwrap().folder_id,
            trash.id
        );

        // Nothing to do: no command.
        let (command, changed) = run_command(&store, account.id, &[]).unwrap();
        assert!(command.is_none());
        assert_eq!(changed, 0);
    }

    #[test]
    fn incoming_mail_is_processed_before_save() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let mut spam = rule(
            account.id,
            vec![RuleCondition::SubjectContains("hello".into())],
            vec![RuleAction::Delete],
        );
        store.save(&mut spam).unwrap();

        // No trash folder: dropped.
        let mut msg = message(account.id, inbox.id);
        assert!(!process_incoming(&store, &mut msg).unwrap());

        let trash = folder(&store, account.id, FolderRole::Trash);
        assert!(process_incoming(&store, &mut msg).unwrap());
        assert_eq!(msg.folder_id, trash.id);
        assert!(describe(&spam).contains("subject contains \"hello\""));
    }
}
