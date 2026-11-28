// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Synchronization primitives of the engine: std and parking_lot normally, loom's under
//! `--cfg lithia_loom`, so the model checker sees every lock, atomic and thread-local (spec §11.4 step 8).
//! `clippy.toml` keeps the others out of the engine, but where a use says why loom need not see it.

#![allow(clippy::disallowed_types)]

#[cfg(not(lithia_loom))]
use std::sync::atomic::Ordering;

#[cfg(not(lithia_loom))]
pub(crate) use parking_lot::MutexGuard;

/// A non-poisoning mutex: parking_lot's, or loom's under `--cfg lithia_loom`.
#[cfg(not(lithia_loom))]
#[derive(Default)]
pub(crate) struct Mutex<T>(parking_lot::Mutex<T>);

#[cfg(not(lithia_loom))]
impl<T> Mutex<T> {
    pub(crate) const fn new(value: T) -> Mutex<T> {
        Mutex(parking_lot::Mutex::new(value))
    }

    #[inline]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock()
    }

    #[inline]
    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
}

/// Declares an atomic of the engine over std's, with the operations the engine uses.
#[cfg(not(lithia_loom))]
macro_rules! atomic {
    ($(#[$doc:meta])* $name:ident, $std:ty, $value:ty $(, $int:ident)?) => {
        $(#[$doc])*
        #[derive(Debug, Default)]
        #[repr(transparent)]
        pub(crate) struct $name($std);

        // One set of operations for every width; not every atomic uses each.
        #[allow(dead_code)]
        impl $name {
            pub(crate) const fn new(value: $value) -> $name {
                $name(<$std>::new(value))
            }

            #[inline]
            pub(crate) fn load(&self, order: Ordering) -> $value {
                self.0.load(order)
            }

            #[inline]
            pub(crate) fn store(&self, value: $value, order: Ordering) {
                self.0.store(value, order)
            }

            #[inline]
            pub(crate) fn swap(&self, value: $value, order: Ordering) -> $value {
                self.0.swap(value, order)
            }

            #[inline]
            pub(crate) fn compare_exchange_weak(
                &self,
                current: $value,
                new: $value,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$value, $value> {
                self.0.compare_exchange_weak(current, new, success, failure)
            }

            #[inline]
            pub(crate) fn compare_exchange(
                &self,
                current: $value,
                new: $value,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$value, $value> {
                self.0.compare_exchange(current, new, success, failure)
            }

            #[inline]
            pub(crate) fn fetch_update(
                &self,
                set: Ordering,
                fetch: Ordering,
                f: impl FnMut($value) -> Option<$value>,
            ) -> Result<$value, $value> {
                self.0.fetch_update(set, fetch, f)
            }
        }

        $(atomic!(@$int $name, $value);)?
    };
    (@int $name:ident, $value:ty) => {
        #[allow(dead_code)]
        impl $name {
            #[inline]
            pub(crate) fn fetch_add(&self, value: $value, order: Ordering) -> $value {
                self.0.fetch_add(value, order)
            }

            #[inline]
            pub(crate) fn fetch_sub(&self, value: $value, order: Ordering) -> $value {
                self.0.fetch_sub(value, order)
            }

            #[inline]
            pub(crate) fn fetch_and(&self, value: $value, order: Ordering) -> $value {
                self.0.fetch_and(value, order)
            }

            #[inline]
            pub(crate) fn fetch_max(&self, value: $value, order: Ordering) -> $value {
                self.0.fetch_max(value, order)
            }
        }
    };
}

#[cfg(not(lithia_loom))]
atomic!(
    /// An atomic flag of the engine.
    AtomicBool, std::sync::atomic::AtomicBool, bool
);
#[cfg(not(lithia_loom))]
atomic!(
    /// An atomic `u32` of the engine.
    AtomicU32, std::sync::atomic::AtomicU32, u32, int
);
#[cfg(not(lithia_loom))]
atomic!(
    /// An atomic `u64` of the engine.
    AtomicU64, std::sync::atomic::AtomicU64, u64, int
);
#[cfg(not(lithia_loom))]
atomic!(
    /// An atomic `usize` of the engine.
    AtomicUsize, std::sync::atomic::AtomicUsize, usize, int
);

#[cfg(lithia_loom)]
pub(crate) use loom::sync::MutexGuard;
#[cfg(lithia_loom)]
pub(crate) use loom::sync::atomic::fence;
#[cfg(lithia_loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
/// A memory fence; loom checks `SeqCst` fences, though it treats `SeqCst` accesses as `AcqRel`.
#[cfg(not(lithia_loom))]
pub(crate) use std::sync::atomic::fence;

/// A static on cache lines of its own, so writes to its neighbours do not stall its readers on
/// other cores; 128 bytes, as adjacent-line prefetchers fetch lines in pairs.
#[repr(align(128))]
pub(crate) struct Padded<T>(pub(crate) T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Non-poisoning mutex over loom's, with parking_lot's interface.
#[cfg(lithia_loom)]
pub(crate) struct Mutex<T>(loom::sync::Mutex<T>);

#[cfg(lithia_loom)]
impl<T> Mutex<T> {
    pub(crate) fn new(value: T) -> Mutex<T> {
        Mutex(loom::sync::Mutex::new(value))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Replaceable `Option<Arc<T>>`: a reader-writer lock, whose write costs the same however many
/// threads read, unlike a lock-free cell's; a loom mutex under loom, so models see every access.
/// Its reads are recursive, so a read inside `with` cannot wait behind a queued write.
#[cfg(not(lithia_loom))]
pub(crate) struct ArcCell<T>(parking_lot::RecursiveRwLock<Slot<T>>);

/// What an `ArcCell` holds: a shared value, or none.
type Slot<T> = Option<std::sync::Arc<T>>;

#[cfg(lithia_loom)]
pub(crate) struct ArcCell<T>(Mutex<Slot<T>>);

impl<T> ArcCell<T> {
    pub(crate) fn new(value: Slot<T>) -> ArcCell<T> {
        #[cfg(not(lithia_loom))]
        return ArcCell(parking_lot::RecursiveRwLock::new(value));
        #[cfg(lithia_loom)]
        return ArcCell(Mutex::new(value));
    }

    /// The current value, as a new reference.
    pub(crate) fn load_full(&self) -> Slot<T> {
        #[cfg(not(lithia_loom))]
        return self.0.read().clone();
        #[cfg(lithia_loom)]
        return self.0.lock().clone();
    }

    /// Replaces the value, returning the previous one.
    pub(crate) fn swap(&self, value: Slot<T>) -> Slot<T> {
        #[cfg(not(lithia_loom))]
        return std::mem::replace(&mut *self.0.write(), value);
        #[cfg(lithia_loom)]
        return std::mem::replace(&mut *self.0.lock(), value);
    }

    /// Calls `f` with the current value, borrowed, so no reference count changes.
    pub(crate) fn with<R>(&self, f: impl FnOnce(Option<&T>) -> R) -> R {
        #[cfg(not(lithia_loom))]
        return f(self.0.read().as_deref());
        #[cfg(lithia_loom)]
        return f(self.0.lock().as_deref());
    }

    /// Replaces the value; the previous one drops here.
    pub(crate) fn store(&self, value: Slot<T>) {
        drop(self.swap(value));
    }
}

/// Declares thread-locals with `const` initializers through std, or through loom, whose model
/// threads share one OS thread and whose macro takes the initializer as a plain expression.
macro_rules! thread_local_shim {
    ($($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const $init:block;)*) => {
        #[cfg(not(lithia_loom))]
        std::thread_local!($($(#[$attr])* $vis static $name: $t = const $init;)*);
        #[cfg(lithia_loom)]
        loom::thread_local!($($(#[$attr])* $vis static $name: $t = $init;)*);
    };
}

pub(crate) use thread_local_shim as thread_local;
