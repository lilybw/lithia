// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The hidden traits `node!`'s expansions dispatch on: initial values, captures and moves (spec §4.2).

/// Supplies the initial value of a node without `init`, through `Default`.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no `Default`, so this node needs an initial value",
    label = "add `init = …` to the node! options",
    note = "derived nodes and async source nodes start with `init`, or `K::default()` when it is left out"
)]
pub trait InitDefault: Sized {
    /// The value a node starts with when `init` is left out (spec §4.4).
    fn __lithia_init() -> Self;
}

impl<T: Default> InitDefault for T {
    fn __lithia_init() -> Self {
        T::default()
    }
}

/// Bound on values a `node!` body captures by cloning (spec §4.2).
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is captured by a node! body but is not `Clone`",
    label = "captured here",
    note = "wrap the part that is not `Clone` in `Arc` and capture node handles beside it, or write `move` and manage the capture yourself"
)]
pub trait Capture: Clone {}

#[diagnostic::do_not_recommend]
impl<T: Clone> Capture for T {}

/// Bound on values a `node!` body captures: a body runs on any worker thread (spec §4.2).
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is captured by a node! body but is not `Send + Sync + 'static`",
    label = "captured here",
    note = "a body runs on any worker thread: replace `Rc` with `Arc`, and `RefCell` or `Cell` with `Mutex`, `RwLock` or an atomic"
)]
pub trait CaptureSend: Send + Sync + 'static {}

#[diagnostic::do_not_recommend]
impl<T: Send + Sync + 'static> CaptureSend for T {}

/// Clones a captured value; method syntax looks through one reference, so a capture of a
/// capture clones the value rather than the reference.
#[doc(hidden)]
pub trait CaptureAny {
    /// Clones the value for the body; requires `Capture` and `CaptureSend`.
    fn __lithia_capture(&self) -> Self
    where
        Self: Capture + CaptureSend;

    /// Like `__lithia_capture`; handle types shadow it to record a read-only capture.
    fn __lithia_capture_ro(&self) -> Self
    where
        Self: Capture + CaptureSend;

    /// The value a capture would clone, borrowed, for a keyed node's probe.
    #[inline]
    fn __lithia_capture_ref(&self) -> &Self {
        self
    }
}

impl<T> CaptureAny for T {
    #[inline]
    fn __lithia_capture(&self) -> Self
    where
        Self: Capture + CaptureSend,
    {
        self.clone()
    }

    #[inline]
    fn __lithia_capture_ro(&self) -> Self
    where
        Self: Capture + CaptureSend,
    {
        self.clone()
    }
}

/// Marks a value a `move` body is about to take; handle types shadow it to flag the handle
/// internal. Called as `(&name).__lithia_move()`, so a reference to a handle is left alone.
#[doc(hidden)]
pub trait MoveAny {
    /// Does nothing.
    #[inline]
    fn __lithia_move(&self) {}

    /// Like `__lithia_move`; handle types shadow it to record a read-only capture.
    #[inline]
    fn __lithia_move_ro(&self) {}
}

impl<T: ?Sized> MoveAny for T {}

/// Classifies a field path of a captured value; handle types shadow both methods.
#[doc(hidden)]
pub trait FieldAny {
    /// Whether the path's value is a handle.
    fn __lithia_is_handle(&self) -> bool {
        false
    }

    /// Records the path's node as one the body may write.
    fn __lithia_field(&self) {}
}

impl<T: ?Sized> FieldAny for T {}
