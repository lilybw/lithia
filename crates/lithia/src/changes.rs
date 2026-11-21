// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The `changes()` stream (spec §3.6, §10.5).

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use futures_core::Stream;
use tokio_stream::wrappers::WatchStream;

use crate::NodeValue;
use crate::node::Node;

/// `Unpin` `Stream<Item = Arc<K>>` of a node's values, current value first; holds the node weakly.
/// `while let Some(v) = changes.next().await { println!("{v}"); }`
pub struct Changes<K: NodeValue> {
    node: Weak<Node<K>>,
    inner: Option<WatchStream<u64>>,
}

impl<K: NodeValue> Changes<K> {
    pub(crate) fn new(node: &Arc<Node<K>>) -> Changes<K> {
        let receiver: Option<tokio::sync::watch::Receiver<u64>> = {
            let mut slot: crate::sync::MutexGuard<'_, Option<tokio::sync::watch::Sender<u64>>> =
                node.header.changes.lock();
            // The channel is made by the first stream. Disposal marks the node a ghost before it
            // takes the sender under this lock, so no stream starts on a channel it never ends.
            if node.header.is_ghost() {
                None
            } else {
                let sender: &tokio::sync::watch::Sender<u64> =
                    slot.get_or_insert_with(|| tokio::sync::watch::channel(0u64).0);
                Some(sender.subscribe())
            }
        };
        Changes { node: Arc::downgrade(node), inner: receiver.map(WatchStream::new) }
    }

    /// Next value, or `None` once the node is freed or disposed.
    pub async fn next(&mut self) -> Option<Arc<K>> {
        poll_fn(|cx: &mut Context<'_>| Pin::new(&mut *self).poll_next(cx)).await
    }
}

impl<K: NodeValue> Stream for Changes<K> {
    type Item = Arc<K>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Arc<K>>> {
        let this: &mut Changes<K> = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.inner = None;
                Poll::Ready(None)
            }
            Poll::Ready(Some(_)) => {
                let value: Option<Arc<K>> = this
                    .node
                    .upgrade()
                    .filter(|node: &Arc<Node<K>>| !node.header.is_ghost())
                    .and_then(|node: Arc<Node<K>>| node.value.load_full());
                if value.is_none() {
                    this.inner = None;
                }
                Poll::Ready(value)
            }
        }
    }
}

impl<K: NodeValue> Drop for Changes<K> {
    fn drop(&mut self) {
        // The last stream of a memo may leave it unread (spec §12).
        if self.inner.take().is_some()
            && let Some(node) = self.node.upgrade()
        {
            crate::support::check_unread(&node.header);
        }
    }
}

impl<K: NodeValue> std::fmt::Debug for Changes<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changes").field("ended", &self.inner.is_none()).finish()
    }
}
