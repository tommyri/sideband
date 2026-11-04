use super::id;
use crate::Error;
use ss_core::Id;
use ss_proto::v1;
use std::collections::BTreeMap;

/// Last 200 live messages plus historical pages explicitly requested by the
/// caller. Both sets are ordered by ULID and share identity across echoes.
#[derive(Debug, Clone, Default)]
pub struct MessageCache {
    recent: BTreeMap<Id, v1::Message>,
    history: BTreeMap<Id, v1::Message>,
    deleted: BTreeMap<Id, v1::Message>,
}
impl MessageCache {
    /// All loaded messages in ascending ULID order, without duplicate echoes.
    pub fn ordered(&self) -> Vec<&v1::Message> {
        let mut merged: BTreeMap<_, _> = self.history.iter().collect();
        merged.extend(self.recent.iter());
        merged.into_values().collect()
    }
    /// Retain messages explicitly requested by a presentation host alongside
    /// loaded history, so the rolling live window cannot create visible gaps.
    pub(super) fn retain_loaded(&mut self) {
        self.history.extend(self.recent.clone());
    }
    /// Number retained from live traffic (at most 200).
    pub fn recent_len(&self) -> usize {
        self.recent.len()
    }
    /// Number of explicitly loaded historical messages.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
    pub(super) fn get(&self, key: Id) -> Option<&v1::Message> {
        self.recent.get(&key).or_else(|| self.history.get(&key))
    }
    pub(super) fn newest(&self) -> Option<&v1::Message> {
        self.ordered().last().copied()
    }
    /// Own-message deletion markers observed in this session, with bodies/files erased.
    pub fn deleted(&self) -> Vec<&v1::Message> {
        self.deleted.values().collect()
    }
    pub(super) fn tombstone(&mut self, key: Id, mut message: v1::Message) {
        self.remove(key);
        message.content.clear();
        message.attachments.clear();
        message.link_previews.clear();
        message.mentions.clear();
        message.reply_to = None;
        self.deleted.insert(key, message);
    }
    pub(super) fn remove(&mut self, key: Id) {
        self.recent.remove(&key);
        self.history.remove(&key);
    }
    pub(super) fn upsert(&mut self, message: v1::Message, created: bool) -> Result<(), Error> {
        let key = id(&message.id)?;
        if self.deleted.contains_key(&key) {
            return Ok(());
        }
        if let Some(old) = self.history.get_mut(&key) {
            *old = message.clone();
        }
        if created || self.recent.contains_key(&key) {
            self.recent.insert(key, message);
        }
        while self.recent.len() > 200 {
            self.recent.pop_first();
        }
        Ok(())
    }
    pub(super) fn load(&mut self, page: Vec<v1::Message>) -> Result<(), Error> {
        // Validate the complete page before changing anything.
        let page: Result<Vec<_>, Error> = page
            .into_iter()
            .map(|message| Ok((id(&message.id)?, message)))
            .collect();
        for (key, message) in page? {
            if !self.recent.contains_key(&key) && !self.deleted.contains_key(&key) {
                self.history.insert(key, message);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_messages_survive_live_eviction_but_follow_edits_and_deletes() {
        let mut cache = MessageCache::default();
        let message = |value: u128| v1::Message {
            id: value.to_be_bytes().to_vec().into(),
            content: value.to_string(),
            ..Default::default()
        };
        cache.upsert(message(1), true).unwrap();
        cache.retain_loaded();
        for value in 2..=202 {
            cache.upsert(message(value), true).unwrap();
        }
        assert_eq!(cache.recent_len(), 200);
        assert_eq!(cache.ordered()[0].content, "1");
        let mut edited = message(1);
        edited.content = "edited".into();
        cache.upsert(edited, false).unwrap();
        assert_eq!(cache.ordered()[0].content, "edited");
        cache.remove(Id::from_bytes(1_u128.to_be_bytes()));
        assert!(cache.ordered().iter().all(|m| m.content != "edited"));
    }
}
