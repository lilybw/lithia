//! Engine of lithia: nodes, handles, propagation and diagnostics.
//! Depend on the `lithia` crate, which re-exports this one together with `effect!`.

mod api;
mod changes;
mod context;
mod create;
mod diag;
mod dispose;
mod drops;
mod epoch;
mod handles;
mod ids;
mod node;
mod options;
mod protocol;
mod run;
mod sets;
mod settle_wait;
mod site;
mod status;
mod value;

pub use api::{defer, dispose, eq, flush, reflow, spawn, spawn_blocking, status, try_reflow};
pub use changes::Changes;
pub use handles::{Accessor, DerivedNode, Dispose, Readable, Reflow, Setter, SourceNode, Writable};
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

/// Items used by `effect!` and the attribute macros; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::api::{block_on_main, block_on_test};
    pub use crate::create::{derived, derived_async, derived_gen, node_ref, source, source_async, source_gen};
    pub use crate::handles::NodeHandle;
    pub use crate::options::Options;
    pub use crate::run::{TaskFuture, YieldItem, Yielder};
    pub use crate::site::fn_path;
    pub use crate::value::{Capture, InitDefault};

    /// Option constants, glob-imported around option values so they need no import.
    pub mod consts {
        pub use crate::options::{
            ALWAYS_PROPAGATE, FRAGILE, ON_GHOST_CAUSE_FAILURE, ON_GHOST_EMIT_LATEST, ON_GHOST_SET_AS_PENDING,
            SEMI_FRAGILE,
        };
    }
}
