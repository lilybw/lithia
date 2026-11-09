//! Bounds on node values (spec §11.1).

/// Bound on every node value; blanket-implemented so that the compile error names the fix.
/// `fn store<K: NodeValue>(v: K) -> SourceNode<K> { effect!(v) }`
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be a node value: values must be `Send + Sync + 'static`",
    note = "replace `Rc` with `Arc`, and `RefCell` or `Cell` with `Mutex`, `RwLock` or an atomic"
)]
pub trait NodeValue: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> NodeValue for T {}

/// Supplies the initial value of a node without `init`, through `Default`.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no `Default`, so this node needs an initial value",
    label = "add `init = …` to the effect! options",
    note = "derived nodes and async source nodes start with `init`, or `K::default()` when it is left out"
)]
pub trait InitDefault: Sized {
    /// The value a node starts with when `init` is left out.
    fn __lithia_init() -> Self;
}

impl<T: Default> InitDefault for T {
    fn __lithia_init() -> Self {
        T::default()
    }
}

/// Bound on values an `effect!` body captures by cloning (spec §4.2).
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is captured by an effect! body but is not `Clone`",
    label = "captured here",
    note = "wrap the value in `Arc`, or write `move` and manage the capture yourself"
)]
pub trait Capture: Clone {}

impl<T: Clone> Capture for T {}

/// Clones a captured value; method syntax looks through one reference, so a capture of a
/// capture clones the value rather than the reference.
#[doc(hidden)]
pub trait CaptureAny {
    /// Clones the value for the body; requires `Capture`.
    fn __lithia_capture(&self) -> Self
    where
        Self: Capture;

    /// Like `__lithia_capture`; handle types shadow it to record a read-only capture.
    fn __lithia_capture_ro(&self) -> Self
    where
        Self: Capture;
}

impl<T> CaptureAny for T {
    #[inline]
    fn __lithia_capture(&self) -> Self
    where
        Self: Capture,
    {
        self.clone()
    }

    #[inline]
    fn __lithia_capture_ro(&self) -> Self
    where
        Self: Capture,
    {
        self.clone()
    }
}

/// Takes a value a `move` body captures; handle types shadow it to flag the handle internal.
#[doc(hidden)]
pub trait MoveAny: Sized {
    /// Returns the value unchanged.
    fn __lithia_move(self) -> Self;
}

impl<T> MoveAny for T {
    #[inline]
    fn __lithia_move(self) -> Self {
        self
    }
}
