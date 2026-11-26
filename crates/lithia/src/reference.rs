// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! `Ref`: a node's value as a read returned it (spec §3.3).

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// A node's value as one read saw it: it derefs to the value, and later writes leave it as it is.
/// `let shared: Arc<u32> = count.get().into_arc();`
pub struct Ref<K> {
    value: Arc<K>,
}

impl<K> Ref<K> {
    pub(crate) fn new(value: Arc<K>) -> Ref<K> {
        Ref { value }
    }

    /// The `Arc` holding the value. A `Ref<K>` also converts with `into()` wherever an `Arc<K>` is wanted.
    /// `let shared: Arc<String> = name.get().into_arc();`
    pub fn into_arc(self) -> Arc<K> {
        self.value
    }

    /// The `Arc` holding the value, borrowed; called as `Ref::as_arc(&r)`, so it hides no method of the value.
    pub fn as_arc(this: &Ref<K>) -> &Arc<K> {
        &this.value
    }

    /// True if both hold the same value, as `Arc::ptr_eq` says; called as `Ref::ptr_eq(&a, &b)`.
    pub fn ptr_eq(this: &Ref<K>, other: &Ref<K>) -> bool {
        Arc::ptr_eq(&this.value, &other.value)
    }
}

impl<K> Clone for Ref<K> {
    fn clone(&self) -> Ref<K> {
        Ref { value: Arc::clone(&self.value) }
    }
}

impl<K> Deref for Ref<K> {
    type Target = K;

    fn deref(&self) -> &K {
        &self.value
    }
}

impl<K> AsRef<K> for Ref<K> {
    fn as_ref(&self) -> &K {
        &self.value
    }
}

impl<K> Borrow<K> for Ref<K> {
    fn borrow(&self) -> &K {
        &self.value
    }
}

impl<K> From<Ref<K>> for Arc<K> {
    fn from(read: Ref<K>) -> Arc<K> {
        read.value
    }
}

impl<K: fmt::Debug> fmt::Debug for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.value, f)
    }
}

impl<K: fmt::Display> fmt::Display for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.value, f)
    }
}

impl<K> fmt::Pointer for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&self.value, f)
    }
}

impl<K: PartialEq> PartialEq for Ref<K> {
    fn eq(&self, other: &Ref<K>) -> bool {
        *self.value == *other.value
    }
}

impl<K: Eq> Eq for Ref<K> {}

impl<K: PartialOrd> PartialOrd for Ref<K> {
    fn partial_cmp(&self, other: &Ref<K>) -> Option<Ordering> {
        (*self.value).partial_cmp(&*other.value)
    }
}

impl<K: Ord> Ord for Ref<K> {
    fn cmp(&self, other: &Ref<K>) -> Ordering {
        (*self.value).cmp(&*other.value)
    }
}

impl<K: Hash> Hash for Ref<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (*self.value).hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::Ref;
    use std::sync::Arc;

    #[test]
    fn reads_as_its_value_and_hands_its_arc_on() {
        let value: Arc<String> = Arc::new("lithia".to_string());
        let read: Ref<String> = Ref::new(Arc::clone(&value));
        let copy: Ref<String> = read.clone();
        assert_eq!(read.len(), 6);
        assert_eq!(format!("{read} {copy:?}"), "lithia \"lithia\"");
        assert!(Ref::ptr_eq(&read, &copy) && Arc::ptr_eq(Ref::as_arc(&read), &value));
        assert_eq!(read, copy);
        let shared: Arc<String> = read.into();
        assert!(Arc::ptr_eq(&shared, &copy.into_arc()));
    }
}
