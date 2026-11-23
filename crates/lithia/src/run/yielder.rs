// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The yielder a generator body publishes items through (spec §8.2).

use super::*;

/// Publishes a generator body's items; `yield x` lowers to `yielder.item(x).await` (spec §16).
#[doc(hidden)]
pub struct Yielder<K: NodeValue> {
    pub(super) shared: Arc<RunControl>,
    pub(super) node: WeakTypedNodeRef<K>,
}

impl<K: NodeValue> Yielder<K> {
    /// Publishes `value` as the node's latest item, then ends the run's current poll.
    pub fn item(&self, value: K) -> YieldItem<'_, K> {
        YieldItem { yielder: self, value: Some(value) }
    }
}

/// Future of one `yield`: publishes on its first poll and completes on its second.
#[doc(hidden)]
pub struct YieldItem<'a, K: NodeValue> {
    pub(super) yielder: &'a Yielder<K>,
    pub(super) value: Option<K>,
}

impl<K: NodeValue> Unpin for YieldItem<'_, K> {}

impl<K: NodeValue> Future for YieldItem<'_, K> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this: &mut YieldItem<'_, K> = self.get_mut();
        match this.value.take() {
            Some(value) => {
                match this.yielder.node.upgrade() {
                    Some(node) => node.publish(&this.yielder.shared, value),
                    None => crate::lifetime::drops::drop_owned(value, None),
                }
                this.yielder.shared.yielded.store(true, Ordering::SeqCst);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            None => Poll::Ready(()),
        }
    }
}
