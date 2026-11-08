//! LITHIA: a thread-safe, fine-grained reactive engine for Rust on tokio, after Solid 2.0's signals.
//! One primitive, `effect!`, creates signals, memos and effects with explicit dependencies.
//!
//! ```ignore
//! use lithia::{effect, DerivedNode, SourceNode};
//!
//! #[lithia::main]
//! async fn main() {
//!     let count: SourceNode<u32> = effect!(0);
//!     let doubled: DerivedNode<u32> = effect!(|count| *count.get() * 2);
//!     count.set(3);
//!     lithia::flush().await;
//!     assert_eq!(*doubled.get(), 6);
//! }
//! ```

pub use lithia_core::{
    ALWAYS_PROPAGATE, Accessor, Changes, DerivedNode, Dispose, FRAGILE, Failure, Flags, Fragility, GhostPolicy,
    Ghosted, NodeValue, ON_GHOST_CAUSE_FAILURE, ON_GHOST_EMIT_LATEST, ON_GHOST_SET_AS_PENDING, Readable, Reflow,
    SEMI_FRAGILE, Setter, Site, SourceNode, Status, Writable, defer, dispose, eq, flush, reflow, spawn,
    spawn_blocking, status, try_reflow,
};
pub use lithia_macro::{effect, main, test};

/// Test utilities: `settled` and `live_nodes`. Unstable: may change in any release.
pub mod test {
    pub use lithia_core::test::{live_nodes, settled};
}

/// Items used by the macro expansions; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use lithia_core::__private::*;
    pub use tokio;
}
