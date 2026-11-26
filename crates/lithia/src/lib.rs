// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! lithia: a thread-safe, fine-grained reactive engine for Rust on tokio, after Solid 2.0's signals.
//! `let doubled: DerivedNode<u32> = node!(|count| *count.get() * 2);` creates a memo over `count`.

// One `unsafe` block, in `context::with_frame`, is allowed where it stands.
#![deny(unsafe_code)]

mod api;
mod changes;
mod context;
mod diag;
mod exec;
mod expand;
mod graph;
mod handles;
mod lifetime;
mod options;
mod protocol;
mod run;
mod site;
mod status;
mod sync;
mod testing;
mod value;

pub use api::{defer, dispose, eq, flush, reflow, spawn, spawn_blocking, status, try_reflow};
pub use changes::Changes;
pub use handles::{Accessor, DerivedNode, Dispose, Readable, Reflow, Setter, SourceNode, Writable};
pub use lithia_macro::{main, node, test};
pub use options::{batches, disposed, resilience};
pub use site::Site;
pub use status::{Failure, Ghosted, Status};
pub use value::NodeValue;

/// Test utilities: `settled` and `live_nodes`. Unstable: may change in any release.
pub mod test {
    pub use crate::testing::settled::{live_nodes, settled};
}

/// Items used by the macro expansions; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::expand::capture::{Captured, Capturing, ReadOnly};
    pub use crate::expand::create::{
        derived, derived_async, derived_gen, move_dep, node_ref, output, source, source_async, source_gen,
    };
    pub use crate::expand::entry::{StartPaused, block_on_main, block_on_test};
    pub use crate::expand::keyed::{
        HandleId, Inputs, KeyRef, KeyValue, NodeKey, Probe, ProbeViaArc, ProbeViaEq, ProbeViaHandle, ProbeViaOpaque,
        Probed, Reclaim, Reclaimed, reclaim_derived, reclaim_source, reclaim_source_async,
    };
    pub use crate::expand::traits::{Capture, CaptureAny, CaptureSend, FieldAny, InitDefault, MoveAny};
    pub use crate::handles::NodeHandle;
    pub use crate::options::{Options, Shape};
    pub use crate::run::{TaskFuture, YieldItem, Yielder};
    pub use crate::site::{SiteCounter, fn_path};
    pub use tokio;

    /// Executor and harness for loom models; only under `--cfg lithia_loom`.
    #[cfg(lithia_loom)]
    pub mod model {
        pub use crate::exec::model::{ModelScope, quiet, reset, run_one, work_until};
    }
}
