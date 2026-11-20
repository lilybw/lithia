// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Synchronization primitives of the engine: std and parking_lot normally, loom's under
//! `--cfg lithia_loom`, so the model checker sees every lock, atomic and thread-local (spec §11.4 step 8).

#[cfg(not(lithia_loom))]
pub(crate) use parking_lot::{Mutex, MutexGuard};
#[cfg(not(lithia_loom))]
pub(crate) use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};

#[cfg(lithia_loom)]
pub(crate) use loom::sync::MutexGuard;
#[cfg(lithia_loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};

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

/// Atomically replaceable `Option<Arc<T>>` with lock-free reads: arc-swap normally, and a
/// loom mutex under loom so the model sees every read and write of a value.
#[cfg(not(lithia_loom))]
pub(crate) struct ArcCell<T>(arc_swap::ArcSwapOption<T>);

#[cfg(lithia_loom)]
pub(crate) struct ArcCell<T>(Mutex<Option<std::sync::Arc<T>>>);

impl<T> ArcCell<T> {
    pub(crate) fn new(value: Option<std::sync::Arc<T>>) -> ArcCell<T> {
        #[cfg(not(lithia_loom))]
        return ArcCell(arc_swap::ArcSwapOption::new(value));
        #[cfg(lithia_loom)]
        return ArcCell(Mutex::new(value));
    }

    /// The current value, as a new reference.
    pub(crate) fn load_full(&self) -> Option<std::sync::Arc<T>> {
        #[cfg(not(lithia_loom))]
        return self.0.load_full();
        #[cfg(lithia_loom)]
        return self.0.lock().clone();
    }

    /// Replaces the value, returning the previous one.
    pub(crate) fn swap(&self, value: Option<std::sync::Arc<T>>) -> Option<std::sync::Arc<T>> {
        #[cfg(not(lithia_loom))]
        return self.0.swap(value);
        #[cfg(lithia_loom)]
        return std::mem::replace(&mut *self.0.lock(), value);
    }

    /// Replaces the value; the previous one drops here.
    pub(crate) fn store(&self, value: Option<std::sync::Arc<T>>) {
        drop(self.swap(value));
    }
}

/// Declares thread-locals through std, or through loom, whose model threads share one OS thread.
macro_rules! thread_local_shim {
    ($($body:tt)*) => {
        #[cfg(not(lithia_loom))]
        std::thread_local!($($body)*);
        #[cfg(lithia_loom)]
        loom::thread_local!($($body)*);
    };
}

pub(crate) use thread_local_shim as thread_local;
