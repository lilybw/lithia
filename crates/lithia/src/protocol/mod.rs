// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The scheduling protocol: framing, releases, runs and settling (spec §7, §11.4).
//! Its locking and release invariants are stated in spec §11.4.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::{AtomicUsize, Mutex, MutexGuard};

use crate::context::{self, Batch, Deferred, Frame, Target};
use crate::diag::Diag;
use crate::graph::epoch::{Epoch, Epochs, Scope, ScopeToken, Tokens};
use crate::graph::ids::{self, EpochId, NodeId, RunId, Stamp, WriterId};
use crate::graph::node::{
    Children, Due, Entries, Entry, Header, Kind, NodeRef, Parents, RunSlot, Sched, SchedGuard, Served, ServedList,
    merge_served, phase,
};
use crate::graph::sets::WriterSet;
use crate::options::resilience::{Fails, Resilience};
use crate::run::{RunControl, TaskFuture};
use crate::{Failure, Site, diag, testing::progress};

mod defer;
mod deliver;
mod lifecycle;
mod link;
mod propagate;
mod work;

pub(crate) use defer::*;
pub(crate) use deliver::*;
pub(crate) use lifecycle::*;
pub(crate) use link::*;
pub(crate) use propagate::*;
pub(crate) use work::*;
