use super::id;
use crate::Error;
use ss_core::Id;
use ss_proto::v1;
use std::collections::{BTreeMap, BTreeSet};

/// A history page holds at most this many messages; a shorter one reached
/// the end of the channel in the direction it was read.
const PAGE: usize = 50;
/// Above every message id: a range ending here is complete up to now.
const LATEST: Id = Id::from_bytes([0xff; 16]);

fn next(key: Id) -> Option<Id> {
    u128::from_be_bytes(key.as_bytes())
        .checked_add(1)
        .map(|value| Id::from_bytes(value.to_be_bytes()))
}
fn previous(key: Id) -> Option<Id> {
    u128::from_be_bytes(key.as_bytes())
        .checked_sub(1)
        .map(|value| Id::from_bytes(value.to_be_bytes()))
}

/// The ids a reconnect resync page is authoritative for: `from` inclusive,
/// `before` exclusive. `None` is open-ended.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    pub(crate) from: Option<Id>,
    pub(crate) before: Option<Id>,
}

impl Span {
    fn contains(self, key: Id) -> bool {
        self.from.is_none_or(|from| key >= from) && self.before.is_none_or(|before| key < before)
    }
}

/// How a history page was asked for, which says what it covers.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fetch {
    /// The newest messages older than the id, or the newest of all.
    Before(Option<Id>),
    /// The oldest messages newer than the id.
    After(Id),
    /// The messages around the id.
    Around(Id),
}

/// A run of cached messages with nothing missing between them: what a host
/// shows at once. Messages outside it are separated by messages not loaded.
#[derive(Debug, Default)]
pub struct Segment<'a> {
    /// Messages in ascending id order.
    pub messages: Vec<&'a v1::Message>,
    /// Own deletion markers in the same range.
    pub deleted: Vec<&'a v1::Message>,
    /// Nothing older than the first message exists.
    pub start: bool,
    /// Nothing newer than the last message exists: the run reaches the live
    /// edge, so new messages appear at its end.
    pub latest: bool,
}

/// Last 200 live messages plus historical pages explicitly requested by the
/// caller. Both sets are ordered by ULID and share identity across echoes.
#[derive(Debug, Clone, Default)]
pub struct MessageCache {
    recent: BTreeMap<Id, v1::Message>,
    history: BTreeMap<Id, v1::Message>,
    deleted: BTreeMap<Id, v1::Message>,
    /// First seen in a reconnect resync page, not yet as a live event.
    unannounced: BTreeSet<Id>,
    /// Inclusive id ranges, `from` to `to`, in which every message of the
    /// channel is cached. Disjoint and never touching. A jump to an old
    /// message adds a range apart from the newest one; the messages between
    /// them are not loaded.
    known: BTreeMap<Id, Id>,
    /// The store revision of the last live event that changed this cache.
    pub(super) changed: u64,
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
    pub(super) fn oldest(&self) -> Option<&v1::Message> {
        self.ordered().first().copied()
    }
    /// Whether `key` came from a reconnect resync page and its live
    /// `MessageCreated` has not been seen. The page is read from the
    /// database and can overtake the event, which must still count as new.
    pub(crate) fn unannounced(&self, key: Id) -> bool {
        self.unannounced.contains(&key)
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
        self.unannounced.remove(&key);
    }
    pub(super) fn upsert(&mut self, message: v1::Message, created: bool) -> Result<(), Error> {
        let key = id(&message.id)?;
        if self.deleted.contains_key(&key) {
            return Ok(());
        }
        if created {
            self.unannounced.remove(&key);
        }
        if let Some(old) = self.history.get_mut(&key) {
            *old = message.clone();
        }
        if created || self.recent.contains_key(&key) {
            self.recent.insert(key, message);
        }
        if created {
            // Live messages arrive in order: the newest range stays complete.
            self.cover(key, LATEST);
        }
        while self.recent.len() > 200 {
            if let Some((evicted, _)) = self.recent.pop_first()
                && !self.history.contains_key(&evicted)
            {
                self.uncover(evicted);
            }
        }
        Ok(())
    }
    pub(super) fn load(&mut self, page: Vec<v1::Message>, fetch: Fetch) -> Result<(), Error> {
        // Validate the complete page before changing anything.
        let page: Vec<_> = page
            .into_iter()
            .map(|message| Ok((id(&message.id)?, message)))
            .collect::<Result<_, Error>>()?;
        let full = page.len() >= PAGE;
        let first = page.first().map(|(key, _)| *key);
        let last = page.last().map(|(key, _)| *key);
        // A short page reached the end of the channel in its direction.
        let (from, to) = match fetch {
            Fetch::Before(before) => (
                if full { first } else { Some(Id::nil()) },
                before.map_or(Some(LATEST), previous),
            ),
            Fetch::After(after) => (next(after), if full { last } else { Some(LATEST) }),
            Fetch::Around(_) if full => (first, last),
            Fetch::Around(_) => (Some(Id::nil()), Some(LATEST)),
        };
        for (key, message) in page {
            if !self.recent.contains_key(&key) && !self.deleted.contains_key(&key) {
                self.history.insert(key, message);
            }
        }
        if let (Some(from), Some(to)) = (from, to) {
            self.cover(from, to);
        }
        Ok(())
    }

    /// Record that every message in `from..=to` is cached, merging the
    /// ranges it overlaps or touches.
    fn cover(&mut self, mut from: Id, mut to: Id) {
        if from > to {
            return;
        }
        let reach = next(to).unwrap_or(to);
        let floor = previous(from);
        let merged: Vec<(Id, Id)> = self
            .known
            .range(..=reach)
            .rev()
            .take_while(|(_, end)| floor.is_none_or(|floor| **end >= floor))
            .map(|(start, end)| (*start, *end))
            .collect();
        for (start, end) in merged {
            self.known.remove(&start);
            from = from.min(start);
            to = to.max(end);
        }
        self.known.insert(from, to);
    }

    /// `key` left the cache although it still exists: the range holding it
    /// splits around it. Parts with no cached message are dropped.
    fn uncover(&mut self, key: Id) {
        let Some((&from, &to)) = self.known.range(..=key).next_back() else {
            return;
        };
        if to < key {
            return;
        }
        self.known.remove(&from);
        for (start, end) in [(Some(from), previous(key)), (next(key), Some(to))] {
            if let (Some(start), Some(end)) = (start, end)
                && start <= end
                && self.holds(start, end)
            {
                self.known.insert(start, end);
            }
        }
    }

    fn holds(&self, from: Id, to: Id) -> bool {
        self.recent.range(from..=to).next().is_some()
            || self.history.range(from..=to).next().is_some()
            || self.deleted.range(from..=to).next().is_some()
    }

    /// The gapless run around `anchor`, or the newest run when `anchor` is
    /// `None` or not cached.
    pub fn segment(&self, anchor: Option<Id>) -> Segment<'_> {
        let cached = |key: &Id| self.get(*key).is_some() || self.deleted.contains_key(key);
        let range = |key: Id| {
            self.known
                .range(..=key)
                .next_back()
                .filter(|(_, to)| **to >= key)
                .map_or((key, key), |(from, to)| (*from, *to))
        };
        let (from, to) = match anchor.filter(cached) {
            Some(anchor) => range(anchor),
            None => match self.known.last_key_value() {
                Some((from, to)) if *to == LATEST => (*from, *to),
                _ => match self.newest().and_then(|m| id(&m.id).ok()) {
                    Some(newest) => range(newest),
                    None => return Segment::default(),
                },
            },
        };
        let within =
            |message: &&v1::Message| id(&message.id).is_ok_and(|key| (from..=to).contains(&key));
        Segment {
            messages: self.ordered().into_iter().filter(within).collect(),
            deleted: self.deleted.values().filter(within).collect(),
            start: from == Id::nil(),
            latest: to == LATEST,
        }
    }

    /// Make the cache match `page` within `span`, a page re-read after a
    /// reconnect. A cached message in the span that the page lacks was
    /// deleted while offline and goes, unless `live` says a live event
    /// brought it since the fetch began. Page entries replace cached copies
    /// (edits made while offline); own deletion markers stay.
    pub(super) fn replace(
        &mut self,
        span: Span,
        page: Vec<v1::Message>,
        live: impl Fn(&Id) -> bool,
    ) -> Result<(), Error> {
        // Validate the complete page before changing anything.
        let page: Vec<_> = page
            .into_iter()
            .map(|message| Ok((id(&message.id)?, message)))
            .collect::<Result<_, Error>>()?;
        let fetched: BTreeSet<Id> = page.iter().map(|(key, _)| *key).collect();
        let first = page.first().map(|(key, _)| *key);
        let gone: Vec<Id> = self
            .recent
            .keys()
            .chain(self.history.keys())
            .filter(|key| span.contains(**key) && !fetched.contains(key) && !live(key))
            .copied()
            .collect();
        for key in gone {
            self.remove(key);
        }
        for (key, message) in page {
            if self.deleted.contains_key(&key) {
                continue;
            }
            let recent = self.recent.get_mut(&key);
            let known = recent.is_some() || self.history.contains_key(&key);
            if let Some(old) = recent {
                old.clone_from(&message);
            }
            if !known {
                self.unannounced.insert(key);
            }
            if !known || self.history.contains_key(&key) {
                self.history.insert(key, message);
            }
        }
        // A page can come back filtered by live deletes, so its length does
        // not show the channel's first message: only `from` or an empty page
        // does. Dropped older messages are then simply not cached.
        let from = span.from.or(first).unwrap_or(Id::nil());
        if span.from.is_none() {
            // Out of pages: what was older is gone, and so is its coverage.
            self.known.retain(|_, end| *end >= from);
        }
        if let Some(to) = span.before.map_or(Some(LATEST), previous) {
            self.cover(from, to);
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

    fn page(ids: std::ops::Range<u128>) -> Vec<v1::Message> {
        ids.map(|value| v1::Message {
            id: value.to_be_bytes().to_vec().into(),
            ..Default::default()
        })
        .collect()
    }
    fn key(value: u128) -> Id {
        Id::from_bytes(value.to_be_bytes())
    }
    fn run(cache: &MessageCache, anchor: u128) -> Vec<u128> {
        cache
            .segment(Some(key(anchor)))
            .messages
            .iter()
            .map(|m| u128::from_be_bytes(m.id[..].try_into().unwrap()))
            .collect()
    }
    #[test]
    fn a_resync_that_drops_older_messages_forgets_their_coverage() {
        let mut cache = MessageCache::default();
        cache.load(page(1000..1050), Fetch::Before(None)).unwrap();
        cache.load(page(100..150), Fetch::Around(key(120))).unwrap();
        // Out of resync pages: everything older than the page goes.
        let span = Span {
            from: None,
            before: Some(key(1000)),
        };
        cache.replace(span, page(950..1000), |_| false).unwrap();
        assert_eq!(run(&cache, 1000), (950..1050).collect::<Vec<_>>());
        assert!(!cache.segment(None).start);
        // Pages around the dropped run must not be joined through it.
        cache.load(page(140..190), Fetch::Around(key(160))).unwrap();
        cache
            .load(page(50..100), Fetch::Before(Some(key(100))))
            .unwrap();
        assert_eq!(run(&cache, 160), (140..190).collect::<Vec<_>>());
        assert_eq!(run(&cache, 60), (50..100).collect::<Vec<_>>());
    }
    #[test]
    fn short_pages_mark_the_ends_of_the_channel() {
        let mut cache = MessageCache::default();
        cache.load(page(10..20), Fetch::Around(key(15))).unwrap();
        let segment = cache.segment(None);
        assert!(segment.start && segment.latest);
        let mut cache = MessageCache::default();
        cache.load(page(10..60), Fetch::Around(key(30))).unwrap();
        cache.load(page(60..70), Fetch::After(key(59))).unwrap();
        let segment = cache.segment(Some(key(30)));
        assert!(!segment.start && segment.latest);
        assert_eq!(segment.messages.len(), 60);
        // An empty channel is one complete, empty run.
        let mut cache = MessageCache::default();
        cache.load(Vec::new(), Fetch::Before(None)).unwrap();
        let segment = cache.segment(None);
        assert!(segment.start && segment.latest && segment.messages.is_empty());
    }
}
