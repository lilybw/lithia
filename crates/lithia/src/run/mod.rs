// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Runs of node bodies, generator yields, and lithia tasks (spec §3.6, §6.2, §7.3, §9.1).

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Waker};

use crate::sync::{AtomicBool, Mutex, MutexGuard};

use crate::NodeValue;
use crate::context::{self, Batch, Frame, Target};
use crate::graph::epoch::{Scope, Tokens};
use crate::graph::ids::{Atomic, NodeId, RunId, Stamp};
use crate::graph::node::{
    AsyncFn, Body, BodySlot, BoxFut, Commit, Displaced, GenFn, Header, Node, NodeRef, SyncFn, TypedNodeRef,
    WeakNodeRef, WeakTypedNodeRef,
};
use crate::protocol::{self, End, Outcome};
use crate::site::SiteKey;

mod control;
mod future;
mod task;
mod yielder;

pub(crate) use control::*;
pub(crate) use future::*;
pub use task::*;
pub use yielder::*;
