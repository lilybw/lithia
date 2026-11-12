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

/// Parents not yet released, as a bitmask over the node's parents that grows past 64 bits.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParentMask(Vec<u64>);

impl ParentMask {
    pub(crate) fn new() -> ParentMask {
        ParentMask(Vec::new())
    }

    pub(crate) fn set(&mut self, index: usize) {
        let word: usize = index / 64;
        if self.0.len() <= word {
            self.0.resize(word + 1, 0);
        }
        self.0[word] |= 1u64 << (index % 64);
    }

    pub(crate) fn clear(&mut self, index: usize) {
        let word: usize = index / 64;
        if let Some(bits) = self.0.get_mut(word) {
            *bits &= !(1u64 << (index % 64));
        }
    }

    pub(crate) fn get(&self, index: usize) -> bool {
        self.0.get(index / 64).is_some_and(|bits: &u64| bits & (1u64 << (index % 64)) != 0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.iter().all(|bits: &u64| *bits == 0)
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
}
