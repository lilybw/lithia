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
        let receiver: Option<tokio::sync::watch::Receiver<u64>> = if node.header.is_ghost() {
            None
        } else {
            node.header.changes.lock().as_ref().map(|sender: &tokio::sync::watch::Sender<u64>| sender.subscribe())
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

impl<K: NodeValue> std::fmt::Debug for Changes<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changes").field("ended", &self.inner.is_none()).finish()
    }
}
