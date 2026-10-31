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
}
impl MessageCache {
    /// All loaded messages in ascending ULID order, without duplicate echoes.
    pub fn ordered(&self) -> Vec<&v1::Message> {
        let mut merged: BTreeMap<_, _> = self.history.iter().collect();
        merged.extend(self.recent.iter());
        merged.into_values().collect()
    }
    /// Number retained from live traffic (at most 200).
    pub fn recent_len(&self) -> usize {
        self.recent.len()
    }
    /// Number of explicitly loaded historical messages.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
    pub(super) fn newest(&self) -> Option<&v1::Message> {
        self.ordered().last().copied()
    }
    pub(super) fn remove(&mut self, key: Id) {
        self.recent.remove(&key);
        self.history.remove(&key);
    }
    pub(super) fn upsert(&mut self, message: v1::Message, created: bool) -> Result<(), Error> {
        let key = id(&message.id)?;
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
            if !self.recent.contains_key(&key) {
                self.history.insert(key, message);
            }
        }
        Ok(())
    }
}
