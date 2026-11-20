// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Small sets used by the scheduler: writer sets and parent masks (spec §5, §7.4).

use crate::ids::NodeId;

/// The writers on the chains that reached an entry or run; sorted and unique.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriterSet(Vec<NodeId>);

impl WriterSet {
    pub(crate) fn new() -> WriterSet {
        WriterSet(Vec::new())
    }

    pub(crate) fn contains(&self, id: NodeId) -> bool {
        self.0.binary_search(&id).is_ok()
    }

    pub(crate) fn insert(&mut self, id: NodeId) {
        if let Err(at) = self.0.binary_search(&id) {
            self.0.insert(at, id);
        }
    }

    pub(crate) fn remove(&mut self, id: NodeId) {
        if let Ok(at) = self.0.binary_search(&id) {
            self.0.remove(at);
        }
    }

    pub(crate) fn union(&mut self, other: &WriterSet) {
        for id in &other.0 {
            self.insert(*id);
        }
    }

    /// This set plus `id`.
    pub(crate) fn with(&self, id: Option<NodeId>) -> WriterSet {
        let mut out: WriterSet = self.clone();
        if let Some(id) = id {
            out.insert(id);
        }
        out
    }

    /// This set minus `id`.
    pub(crate) fn without(&self, id: Option<NodeId>) -> WriterSet {
        let mut out: WriterSet = self.clone();
        if let Some(id) = id {
            out.remove(id);
        }
        out
    }
}

/// Releases parents still owe, as a bitmask over the node's parents (the first 64 inline, the rest in
/// words allocated only for nodes with more parents), plus counts past one, which few entries need.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParentMask {
    low: u64,
    high: Vec<u64>,
    /// (parent index, releases owed beyond the first): a parent framed again before it released.
    more: Vec<(usize, u32)>,
}

impl ParentMask {
    pub(crate) fn new() -> ParentMask {
        ParentMask { low: 0, high: Vec::new(), more: Vec::new() }
    }

    /// Parent `index` owes one more release.
    pub(crate) fn add(&mut self, index: usize) {
        if !self.get(index) {
            self.set(index);
        } else if let Some((_, count)) = self.more.iter_mut().find(|(at, _): &&mut (usize, u32)| *at == index) {
            *count += 1;
        } else {
            self.more.push((index, 1));
        }
    }

    /// Parent `index` owes at least one release.
    pub(crate) fn ensure(&mut self, index: usize) {
        if !self.get(index) {
            self.set(index);
        }
    }

    /// Takes one release parent `index` owes; false if it owes none.
    pub(crate) fn take(&mut self, index: usize) -> bool {
        if !self.get(index) {
            return false;
        }
        match self.more.iter().position(|(at, _): &(usize, u32)| *at == index) {
            Some(at) if self.more[at].1 > 1 => self.more[at].1 -= 1,
            Some(at) => {
                self.more.swap_remove(at);
            }
            None => self.clear(index),
        }
        true
    }

    fn set(&mut self, index: usize) {
        if index < 64 {
            self.low |= 1u64 << index;
            return;
        }
        let word: usize = index / 64 - 1;
        if self.high.len() <= word {
            self.high.resize(word + 1, 0);
        }
        self.high[word] |= 1u64 << (index % 64);
    }

    fn clear(&mut self, index: usize) {
        if index < 64 {
            self.low &= !(1u64 << index);
        } else if let Some(bits) = self.high.get_mut(index / 64 - 1) {
            *bits &= !(1u64 << (index % 64));
        }
    }

    pub(crate) fn get(&self, index: usize) -> bool {
        if index < 64 {
            return self.low & (1u64 << index) != 0;
        }
        self.high.get(index / 64 - 1).is_some_and(|bits: &u64| bits & (1u64 << (index % 64)) != 0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.low == 0 && self.high.iter().all(|bits: &u64| *bits == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_set_ops() {
        let mut set: WriterSet = WriterSet::new();
        set.insert(3);
        set.insert(1);
        set.insert(3);
        assert!(set.contains(1) && set.contains(3) && !set.contains(2));
        let with: WriterSet = set.with(Some(2));
        assert!(with.contains(2));
        assert!(!with.without(Some(2)).contains(2));
    }

    #[test]
    fn mask_grows_past_64() {
        let mut mask: ParentMask = ParentMask::new();
        assert!(mask.is_empty());
        mask.set(70);
        mask.set(1);
        assert!(mask.get(70) && mask.get(1) && !mask.get(2));
        mask.clear(70);
        mask.clear(1);
        assert!(mask.is_empty());
    }

    #[test]
    fn mask_word_edges() {
        let mut mask: ParentMask = ParentMask::new();
        for index in [0usize, 63, 64, 127, 128, 200] {
            assert!(!mask.get(index));
            mask.set(index);
            assert!(mask.get(index) && !mask.is_empty());
        }
        assert!(!mask.get(62) && !mask.get(65) && !mask.get(129) && !mask.get(500));
        assert!(mask.high.len() == 3);
        for index in [0usize, 63, 64, 127, 128] {
            mask.clear(index);
            assert!(!mask.get(index) && !mask.is_empty());
        }
        mask.clear(500);
        mask.clear(200);
        assert!(mask.is_empty());
        let mut fresh: ParentMask = ParentMask::new();
        fresh.set(5);
        assert!(fresh.high.is_empty());
    }

    #[test]
    fn mask_counts_repeated_releases() {
        let mut mask: ParentMask = ParentMask::new();
        assert!(!mask.take(3));
        mask.add(3);
        mask.add(3);
        mask.add(70);
        mask.ensure(70);
        mask.add(3);
        assert!(mask.take(3) && mask.take(3) && mask.get(3) && mask.more.is_empty());
        assert!(mask.take(3) && !mask.get(3) && !mask.take(3));
        assert!(mask.take(70) && mask.is_empty() && !mask.take(70));
    }
}
