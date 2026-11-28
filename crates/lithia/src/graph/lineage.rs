// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Lineages: the exact writer identity a node created in a run shares with the same creation in its
//! creator's next run (spec §7.4).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::expand::keyed::{Key, KeyHash, SharedKey};
use crate::graph::ids::{self, WriterId};
use crate::site::SiteKey;
use crate::sync::{Mutex, MutexGuard};

/// The identity nodes created in runs write under, and the lineages of what they create in turn.
pub(crate) struct Lineage {
    pub(crate) id: WriterId,
    /// A keyed creation's key, compared when a later creation looks for its lineage.
    key: Option<SharedKey>,
    /// Lineages created by runs of nodes that write under this one.
    created: Mutex<Created>,
}

/// A creator's lineages: those of the last run that ended without a successor, and those made since.
#[derive(Default)]
struct Created {
    last: Places,
    since: Places,
}

/// Lineages by where their creation sits: the n-th unkeyed creation at a site, or a key at a site.
#[derive(Default)]
pub(crate) struct Places {
    /// Unkeyed lineages by site, in place order; a run creates at few sites, so a list.
    nth: Vec<(SiteKey, Vec<Arc<Lineage>>)>,
    keyed: HashMap<(SiteKey, KeyHash), Made, BuildHasherDefault<Mix>>,
}

/// The keyed lineages made at one site with one key hash: one, unless keys hash alike.
type Made = SmallVec<[Arc<Lineage>; 1]>;

/// A hasher for keys that are addresses and hashes already; nothing outside lithia chooses them.
#[derive(Default)]
struct Mix(u64);

impl Hasher for Mix {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_u64(u64::from(*byte));
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }
}

/// The lineages made at `site` in `list`, if any.
fn at_site(list: &[(SiteKey, Vec<Arc<Lineage>>)], site: SiteKey) -> Option<&[Arc<Lineage>]> {
    list.iter().find(|(at, _): &&(SiteKey, Vec<Arc<Lineage>>)| *at == site).map(|(_, made)| made.as_slice())
}

impl Lineage {
    /// The lineage a top-level node's creations descend from, under the node's own identity.
    pub(crate) fn root(id: WriterId) -> Arc<Lineage> {
        Arc::new(Lineage { id, key: None, created: Mutex::new(Created::default()) })
    }

    fn fresh(key: Option<SharedKey>) -> Arc<Lineage> {
        Arc::new(Lineage { id: ids::next_lineage(), key, created: Mutex::new(Created::default()) })
    }

    /// The lineage of the `n`-th unkeyed creation at `site` in a run of a node writing under `self`.
    pub(crate) fn nth(&self, site: SiteKey, n: u64) -> Arc<Lineage> {
        let at: usize = usize::try_from(n).unwrap_or(usize::MAX);
        let mut guard: MutexGuard<'_, Created> = self.created.lock();
        let Created { last, since }: &mut Created = &mut guard;
        let known: Option<Arc<Lineage>> = at_site(&since.nth, site)
            .and_then(|made: &[Arc<Lineage>]| made.get(at))
            .or_else(|| at_site(&last.nth, site).and_then(|made: &[Arc<Lineage>]| made.get(at)))
            .cloned();
        let lineage: Arc<Lineage> = known.unwrap_or_else(|| Lineage::fresh(None));
        let index: usize = match since.nth.iter().position(|(known, _): &(SiteKey, Vec<Arc<Lineage>>)| *known == site) {
            Some(index) => index,
            None => {
                // A run usually makes as many here as the last one did.
                let expected: usize = at_site(&last.nth, site).map_or(0, <[Arc<Lineage>]>::len);
                since.nth.push((site, Vec::with_capacity(expected)));
                since.nth.len() - 1
            }
        };
        let made: &mut Vec<Arc<Lineage>> = &mut since.nth[index].1;
        // Places count up from 0 in each run, so a place is one already recorded or the next one.
        debug_assert!(at <= made.len(), "lithia: a creation's place skipped one");
        match made.get_mut(at) {
            Some(slot) => *slot = lineage.clone(),
            None => made.push(lineage.clone()),
        }
        lineage
    }

    /// The lineage of a creation keyed `key` at `site`; keys are compared with no lock held, as
    /// they run user code.
    pub(crate) fn keyed(&self, site: SiteKey, key: &Key) -> Arc<Lineage> {
        let candidates: SmallVec<[Arc<Lineage>; 2]> = {
            let guard: MutexGuard<'_, Created> = self.created.lock();
            let at: (SiteKey, KeyHash) = (site, key.hash);
            let since: &[Arc<Lineage>] = guard.since.keyed.get(&at).map_or(&[], |made: &Made| made.as_slice());
            let last: &[Arc<Lineage>] = guard.last.keyed.get(&at).map_or(&[], |made: &Made| made.as_slice());
            since.iter().chain(last).cloned().collect()
        };
        let known: Option<Arc<Lineage>> = candidates
            .into_iter()
            .find(|candidate: &Arc<Lineage>| candidate.key.as_ref().is_some_and(|own: &SharedKey| own.same(key)));
        let lineage: Arc<Lineage> = known.unwrap_or_else(|| Lineage::fresh(Some(key.share())));
        self.keep_keyed(site, key.hash, &lineage);
        lineage
    }

    /// Records a keyed lineage as created since the last run ended, as a reclaimed node keeps its own.
    pub(crate) fn keep_keyed(&self, site: SiteKey, hash: KeyHash, lineage: &Arc<Lineage>) {
        let mut guard: MutexGuard<'_, Created> = self.created.lock();
        let made: &mut Made = guard.since.keyed.entry((site, hash)).or_default();
        if !made.iter().any(|known: &Arc<Lineage>| Arc::ptr_eq(known, lineage)) {
            made.push(lineage.clone());
        }
    }

    /// A run ended without a successor: what it created is what the next run finds. Returns the
    /// lineages let go of, for the caller to drop once it holds no lock.
    pub(crate) fn rotate(&self) -> Places {
        let mut guard: MutexGuard<'_, Created> = self.created.lock();
        let since: Places = std::mem::take(&mut guard.since);
        std::mem::replace(&mut guard.last, since)
    }
}
