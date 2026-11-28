// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The sweep lock (spec §10.1). A build with `--cfg lithia_sweep_lock="…"` swaps it for another lock,
//! so `.github/scripts/lock_bench.py` can compare them on the scaling benchmark; loom models keep loom's.

// The locks compared are the point here; loom models take the sweep lock from `crate::sync`.
#![allow(clippy::disallowed_types)]

#[cfg(lithia_loom)]
pub(crate) type SweepGuard<'a> = crate::sync::MutexGuard<'a, ()>;

/// The sweep lock's acquisitions and the time spent waiting for it and holding it, in a build with
/// `--cfg lithia_lock_stats`, for the scaling benchmark.
#[cfg(all(lithia_lock_stats, not(lithia_loom)))]
pub mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static ACQUIRED: AtomicU64 = AtomicU64::new(0);
    static WAITED_NS: AtomicU64 = AtomicU64::new(0);
    static HELD_NS: AtomicU64 = AtomicU64::new(0);
    static LONGEST_NS: AtomicU64 = AtomicU64::new(0);

    /// What the lock went through since the last `take`.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct LockStats {
        /// Times the lock was taken.
        pub acquired: u64,
        /// Time spent waiting to take it, over all acquisitions.
        pub waited: Duration,
        /// Time it was held, over all acquisitions.
        pub held: Duration,
        /// The longest single hold.
        pub longest: Duration,
    }

    /// Returns the figures gathered since the last call and starts again from zero.
    pub fn take() -> LockStats {
        LockStats {
            acquired: ACQUIRED.swap(0, Ordering::Relaxed),
            waited: Duration::from_nanos(WAITED_NS.swap(0, Ordering::Relaxed)),
            held: Duration::from_nanos(HELD_NS.swap(0, Ordering::Relaxed)),
            longest: Duration::from_nanos(LONGEST_NS.swap(0, Ordering::Relaxed)),
        }
    }

    pub(crate) fn acquired(waited: Duration) {
        ACQUIRED.fetch_add(1, Ordering::Relaxed);
        WAITED_NS.fetch_add(waited.as_nanos() as u64, Ordering::Relaxed);
    }

    pub(crate) fn released(held: Duration) {
        let nanos: u64 = held.as_nanos() as u64;
        HELD_NS.fetch_add(nanos, Ordering::Relaxed);
        LONGEST_NS.fetch_max(nanos, Ordering::Relaxed);
    }
}

/// The sweep lock as held, timed in a build with `--cfg lithia_lock_stats`.
pub(crate) struct Held<'a> {
    _guard: SweepGuard<'a>,
    #[cfg(all(lithia_lock_stats, not(lithia_loom)))]
    since: std::time::Instant,
}

impl<'a> Held<'a> {
    /// Takes `lock` with `take`, timing the wait when stats are on.
    #[inline]
    pub(crate) fn take(take: impl FnOnce() -> SweepGuard<'a>) -> Held<'a> {
        #[cfg(all(lithia_lock_stats, not(lithia_loom)))]
        {
            let start: std::time::Instant = std::time::Instant::now();
            let guard: SweepGuard<'a> = take();
            let since: std::time::Instant = std::time::Instant::now();
            stats::acquired(since - start);
            Held { _guard: guard, since }
        }
        #[cfg(not(all(lithia_lock_stats, not(lithia_loom))))]
        Held { _guard: take() }
    }
}

#[cfg(all(lithia_lock_stats, not(lithia_loom)))]
impl Drop for Held<'_> {
    fn drop(&mut self) {
        stats::released(self.since.elapsed());
    }
}

#[cfg(not(lithia_loom))]
pub(crate) use imp::{SweepGuard, SweepLock};

/// `parking_lot`'s mutex, which unlocks unfairly: the default.
#[cfg(all(
    not(lithia_loom),
    not(any(
        lithia_sweep_lock = "fair",
        lithia_sweep_lock = "std",
        lithia_sweep_lock = "spin",
        lithia_sweep_lock = "ticket"
    ))
))]
mod imp {
    pub(crate) type SweepGuard<'a> = parking_lot::MutexGuard<'a, ()>;

    pub(crate) struct SweepLock(parking_lot::Mutex<()>);

    impl SweepLock {
        pub(crate) const fn new() -> SweepLock {
            SweepLock(parking_lot::Mutex::new(()))
        }

        #[inline]
        pub(crate) fn lock(&self) -> SweepGuard<'_> {
            self.0.lock()
        }
    }
}

/// `parking_lot`'s mutex, handed straight to the next waiter on every unlock.
#[cfg(all(not(lithia_loom), lithia_sweep_lock = "fair"))]
mod imp {
    pub(crate) struct SweepGuard<'a>(Option<parking_lot::MutexGuard<'a, ()>>);

    impl Drop for SweepGuard<'_> {
        fn drop(&mut self) {
            if let Some(guard) = self.0.take() {
                parking_lot::MutexGuard::unlock_fair(guard);
            }
        }
    }

    pub(crate) struct SweepLock(parking_lot::Mutex<()>);

    impl SweepLock {
        pub(crate) const fn new() -> SweepLock {
            SweepLock(parking_lot::Mutex::new(()))
        }

        #[inline]
        pub(crate) fn lock(&self) -> SweepGuard<'_> {
            SweepGuard(Some(self.0.lock()))
        }
    }
}

/// The standard library's mutex: a futex on Linux, an SRW lock on Windows.
#[cfg(all(not(lithia_loom), lithia_sweep_lock = "std"))]
mod imp {
    pub(crate) type SweepGuard<'a> = std::sync::MutexGuard<'a, ()>;

    pub(crate) struct SweepLock(std::sync::Mutex<()>);

    impl SweepLock {
        pub(crate) const fn new() -> SweepLock {
            SweepLock(std::sync::Mutex::new(()))
        }

        #[inline]
        pub(crate) fn lock(&self) -> SweepGuard<'_> {
            // A panic under the lock leaves nothing half-done that the next holder must know of.
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }
}

/// `parking_lot`'s mutex, tried for about ten microseconds before the thread parks.
#[cfg(all(not(lithia_loom), lithia_sweep_lock = "spin"))]
mod imp {
    pub(crate) type SweepGuard<'a> = parking_lot::MutexGuard<'a, ()>;

    pub(crate) struct SweepLock(parking_lot::Mutex<()>);

    /// Tries before parking; with the backoff below, roughly ten microseconds on a desktop core.
    const TRIES: u32 = 200;

    impl SweepLock {
        pub(crate) const fn new() -> SweepLock {
            SweepLock(parking_lot::Mutex::new(()))
        }

        #[inline]
        pub(crate) fn lock(&self) -> SweepGuard<'_> {
            for attempt in 0..TRIES {
                if let Some(guard) = self.0.try_lock() {
                    return guard;
                }
                for _ in 0..(1u32 << attempt.min(6)) {
                    std::hint::spin_loop();
                }
            }
            self.0.lock()
        }
    }
}

/// A ticket lock: first come, first served, spinning on a line of its own and yielding when a wait
/// grows long; it never parks.
#[cfg(all(not(lithia_loom), lithia_sweep_lock = "ticket"))]
mod imp {
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::sync::Padded;

    pub(crate) struct SweepLock {
        next: Padded<AtomicU32>,
        serving: Padded<AtomicU32>,
    }

    pub(crate) struct SweepGuard<'a>(&'a SweepLock);

    impl Drop for SweepGuard<'_> {
        fn drop(&mut self) {
            self.0.serving.fetch_add(1, Ordering::Release);
        }
    }

    /// Spins before a waiter starts yielding its thread between checks.
    const SPINS: u32 = 1 << 12;

    impl SweepLock {
        pub(crate) const fn new() -> SweepLock {
            SweepLock { next: Padded(AtomicU32::new(0)), serving: Padded(AtomicU32::new(0)) }
        }

        #[inline]
        pub(crate) fn lock(&self) -> SweepGuard<'_> {
            let ticket: u32 = self.next.fetch_add(1, Ordering::Relaxed);
            let mut spins: u32 = 0;
            while self.serving.load(Ordering::Acquire) != ticket {
                if spins < SPINS {
                    spins += 1;
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
            SweepGuard(self)
        }
    }
}
