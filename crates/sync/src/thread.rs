//! Conversation threading from `Message-ID`, `In-Reply-To` and `References`.
//!
//! A thread is named after its root message id (first `References` entry, else
//! `In-Reply-To`, else the message's own id). The index remembers every id seen so a reply
//! without `References` still joins the thread of the message it answers, whatever the
//! arrival order.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadIndex {
    by_message_id: HashMap<String, String>,
}

/// `<Abc@Host>` → `Abc@Host` (ids are case-sensitive on the left part; keep the case).
pub fn normalize_id(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_owned()
}

impl ThreadIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the thread id for a message and records its ids.
    /// `fallback` names the thread when the message has no ids at all.
    pub fn assign(
        &mut self,
        message_id: &str,
        in_reply_to: &[String],
        references: &[String],
        fallback: &str,
    ) -> String {
        let own = normalize_id(message_id);
        let related: Vec<String> = references
            .iter()
            .chain(in_reply_to)
            .map(|id| normalize_id(id))
            .filter(|id| !id.is_empty() && *id != own)
            .collect();
        let all_ids = || {
            std::iter::once(&own)
                .chain(&related)
                .filter(|id| !id.is_empty())
        };

        let thread = all_ids()
            .find_map(|id| self.by_message_id.get(id).cloned())
            .or_else(|| related.first().cloned())
            .or_else(|| (!own.is_empty()).then(|| own.clone()))
            .unwrap_or_else(|| fallback.to_owned());

        let ids: Vec<String> = all_ids().cloned().collect();
        for id in ids {
            self.by_message_id
                .entry(id)
                .or_insert_with(|| thread.clone());
        }
        thread
    }

    pub fn len(&self) -> usize {
        self.by_message_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_message_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn replies_join_the_root_thread() {
        let mut index = ThreadIndex::new();
        let root = index.assign("<a@x>", &[], &[], "f");
        let reply = index.assign("<b@x>", &ids(&["<a@x>"]), &ids(&["<a@x>"]), "f");
        let nested = index.assign("<c@x>", &ids(&["<b@x>"]), &ids(&["<a@x>", "<b@x>"]), "f");
        assert_eq!(root, "a@x");
        assert_eq!(reply, root);
        assert_eq!(nested, root);
    }

    #[test]
    fn reply_without_references_uses_in_reply_to() {
        let mut index = ThreadIndex::new();
        let root = index.assign("<a@x>", &[], &[], "f");
        let reply = index.assign("<b@x>", &ids(&["<a@x>"]), &[], "f");
        let reply_to_reply = index.assign("<c@x>", &ids(&["<b@x>"]), &[], "f");
        assert_eq!(reply, root);
        assert_eq!(reply_to_reply, root);
    }

    #[test]
    fn child_arriving_first_still_matches_parent() {
        let mut index = ThreadIndex::new();
        let child = index.assign("<b@x>", &ids(&["<a@x>"]), &[], "f");
        let parent = index.assign("<a@x>", &[], &[], "f");
        assert_eq!(child, parent);
    }

    #[test]
    fn unrelated_messages_get_distinct_threads() {
        let mut index = ThreadIndex::new();
        assert_ne!(
            index.assign("<a@x>", &[], &[], "f"),
            index.assign("<z@x>", &[], &[], "g")
        );
        assert_eq!(index.assign("", &[], &[], "fallback"), "fallback");
    }
}
