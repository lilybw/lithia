//! LITHIA: a thread-safe, fine-grained reactive engine for Rust on tokio, after Solid 2.0's signals.
//! `let doubled: DerivedNode<u32> = effect!(|count| *count.get() * 2);` creates a memo over `count`.

mod api;
mod capture;
mod changes;
mod context;
mod create;
mod diag;
mod dispose;
mod drops;
mod epoch;
mod handles;
mod ids;
mod keyed;
mod node;
mod options;
mod protocol;
mod run;
mod sets;
mod settle_wait;
mod site;
mod status;
mod support;
mod sync;
mod value;

#[cfg(lithia_loom)]
mod model;

pub use api::{defer, dispose, eq, flush, reflow, spawn, spawn_blocking, status, try_reflow};
pub use changes::Changes;
pub use handles::{Accessor, DerivedNode, Dispose, Readable, Reflow, Setter, SourceNode, Writable};
pub use lithia_macro::{effect, main, test};
pub use options::{
    ALWAYS_PROPAGATE, FRAGILE, Flags, Fragility, GhostPolicy, ON_GHOST_CAUSE_FAILURE, ON_GHOST_EMIT_LATEST,
    ON_GHOST_SET_AS_PENDING, SEMI_FRAGILE,
};
pub use site::Site;
pub use status::{Failure, Ghosted, Status};
pub use value::NodeValue;

/// Test utilities: `settled` and `live_nodes`. Unstable: may change in any release.
pub mod test {
    pub use crate::api::test_utils::{live_nodes, settled};
}

/// Items used by the macro expansions; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::api::{block_on_main, block_on_test};
    pub use crate::capture::{Captured, Capturing};
    pub use crate::create::{
        derived, derived_async, derived_gen, move_dep, node_ref, output, source, source_async, source_gen,
    };
    pub use crate::handles::NodeHandle;
    pub use crate::keyed::{
        HandleId, Inputs, KeyRef, KeyValue, Probe, ProbeViaArc, ProbeViaEq, ProbeViaHandle, ProbeViaOpaque, Probed,
        Reuse, Reused, reuse_derived, reuse_source, reuse_source_async,
    };
    pub use crate::options::Options;
    pub use crate::run::{TaskFuture, YieldItem, Yielder};
    pub use crate::site::fn_path;
    pub use crate::value::{Capture, CaptureAny, FieldAny, InitDefault, MoveAny};
    pub use tokio;

    /// Executor and harness for loom models; only under `--cfg lithia_loom`.
    #[cfg(lithia_loom)]
    pub mod model {
        pub use crate::model::{ModelScope, quiet, reset, run_one, work_until};
    }

    /// Option constants, glob-imported around option values so they need no import.
    pub mod consts {
        pub use crate::options::{
            ALWAYS_PROPAGATE, FRAGILE, ON_GHOST_CAUSE_FAILURE, ON_GHOST_EMIT_LATEST, ON_GHOST_SET_AS_PENDING,
            SEMI_FRAGILE,
        };
    }
}
