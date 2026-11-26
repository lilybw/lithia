// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Random graphs under concurrent writes, checked against their definition once they settle.
//! `LITHIA_RANDOM_SEEDS=<n>` runs more graphs; `LITHIA_RANDOM_SEED=<seed>` replays one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use lithia::{Accessor, DerivedNode, SourceNode, node};

/// xorshift64*: deterministic, so a seed names one graph and one sequence of writes per task.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// What every derived node computes from its parents' values; few outputs, so equal values stop
/// propagation now and then.
fn mix(values: &[u64]) -> u64 {
    let hash: u64 = values.iter().fold(0xCBF2_9CE4_8422_2325, |h: u64, v: &u64| (h ^ v).wrapping_mul(0x0100_0000_01B3));
    hash % 64
}

/// Records `value` as the last a node computed, and returns it.
fn put(slot: &AtomicU64, value: u64) -> u64 {
    slot.store(value, Ordering::SeqCst);
    value
}

/// Counts a body alive until the body is dropped with its node.
struct Token(Arc<AtomicUsize>);

impl Token {
    fn new(alive: &Arc<AtomicUsize>) -> Arc<Token> {
        alive.fetch_add(1, Ordering::SeqCst);
        Arc::new(Token(alive.clone()))
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

enum Kind {
    Sync,
    Async,
    Whole,
    /// Creates a child on its parent in every run, rebuilt or, keyed, reclaimed.
    Creator {
        keyed: bool,
    },
    /// Writes what it computes into a source node of its own, which a reader watches.
    Writer,
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Sync => "sync",
            Kind::Async => "async",
            Kind::Whole => "whole-batches",
            Kind::Creator { keyed: false } => "creator",
            Kind::Creator { keyed: true } => "keyed creator",
            Kind::Writer => "writer",
        }
    }
}

struct Derived {
    parents: Vec<usize>,
    kind: &'static str,
    /// The value its last run computed; runs of one node never overlap.
    slot: Arc<AtomicU64>,
    /// A creator's child's last value.
    child: Option<Arc<AtomicU64>>,
    /// A writer's target, and the last value of the reader watching it.
    out: Option<(SourceNode<u64>, Arc<AtomicU64>)>,
    handle: Option<DerivedNode<u64>>,
}

struct Graph {
    sources: Vec<SourceNode<u64>>,
    derived: Vec<Derived>,
    /// Bodies alive: every derived node's and every creator's current child's.
    alive: Arc<AtomicUsize>,
}

impl Graph {
    fn accessor(&self, at: usize) -> Accessor<u64> {
        match at.checked_sub(self.sources.len()) {
            None => self.sources[at].accessor(),
            Some(d) => self.derived[d].handle.as_ref().expect("parents keep their handles").accessor(),
        }
    }

    fn value(&self, at: usize) -> u64 {
        match at.checked_sub(self.sources.len()) {
            None => *self.sources[at].get(),
            Some(d) => self.derived[d].slot.load(Ordering::SeqCst),
        }
    }

    /// The bodies alive once dropped nodes' bodies are gone: one per derived node, child and reader.
    fn bodies(&self) -> usize {
        let extra: usize = self.derived.iter().filter(|n: &&Derived| n.child.is_some() || n.out.is_some()).count();
        self.derived.len() + extra
    }

    fn build(rng: &mut Rng) -> Graph {
        let sources: Vec<SourceNode<u64>> = (0..2 + rng.below(3)).map(|_| node!(rng.below(64))).collect();
        let mut graph: Graph = Graph { sources, derived: Vec::new(), alive: Arc::new(AtomicUsize::new(0)) };
        for _ in 0..6 + rng.below(9) {
            let count: usize = graph.sources.len() + graph.derived.len();
            let kind: Kind = match rng.below(20) {
                0..=10 => Kind::Sync,
                11..=13 => Kind::Async,
                14 | 15 => Kind::Whole,
                16 => Kind::Creator { keyed: false },
                17 => Kind::Creator { keyed: true },
                _ => Kind::Writer,
            };
            let arity: usize = match kind {
                Kind::Creator { .. } => 1,
                Kind::Writer => 1 + rng.below(2.min(count as u64)) as usize,
                Kind::Whole => 2 + rng.below(2.min(count as u64 - 1)) as usize,
                _ => 1 + rng.below(3.min(count as u64)) as usize,
            };
            let mut parents: Vec<usize> = Vec::new();
            while parents.len() < arity {
                // Biased towards recent nodes, for depth.
                let pick: usize = count - 1 - rng.below(count as u64).min(rng.below(count as u64)) as usize;
                if !parents.contains(&pick) {
                    parents.push(pick);
                }
            }
            let slot: Arc<AtomicU64> = Arc::new(AtomicU64::new(u64::MAX));
            let child: Option<Arc<AtomicU64>> =
                matches!(kind, Kind::Creator { .. }).then(|| Arc::new(AtomicU64::new(u64::MAX)));
            let out: Option<(SourceNode<u64>, Arc<AtomicU64>)> = matches!(kind, Kind::Writer).then(|| {
                let target: SourceNode<u64> = node!(u64::MAX);
                let seen: Arc<AtomicU64> = Arc::new(AtomicU64::new(u64::MAX));
                let (o, mine, last): (Accessor<u64>, Arc<Token>, Arc<AtomicU64>) =
                    (target.accessor(), Token::new(&graph.alive), seen.clone());
                node!(|o| {
                    let _ = &mine;
                    put(last, mix(&[*o.get(), 2]))
                });
                (target, seen)
            });
            let target: Option<SourceNode<u64>> =
                out.as_ref().map(|(t, _): &(SourceNode<u64>, Arc<AtomicU64>)| t.clone());
            let handle: DerivedNode<u64> = graph.node(&parents, &kind, slot.clone(), child.clone(), target);
            graph.derived.push(Derived { parents, kind: kind.name(), slot, child, out, handle: Some(handle) });
        }
        graph
    }

    fn node(
        &self,
        parents: &[usize],
        kind: &Kind,
        slot: Arc<AtomicU64>,
        child: Option<Arc<AtomicU64>>,
        target: Option<SourceNode<u64>>,
    ) -> DerivedNode<u64> {
        let p: Vec<Accessor<u64>> = parents.iter().map(|&at: &usize| self.accessor(at)).collect();
        let token: Arc<Token> = Token::new(&self.alive);
        let alive: Arc<AtomicUsize> = self.alive.clone();
        if let Some(target) = target {
            return match p.as_slice() {
                [p0] => {
                    let p0: Accessor<u64> = p0.clone();
                    node!(|p0| {
                        let _ = &token;
                        let value: u64 = mix(&[*p0.get()]);
                        target.set(value);
                        put(slot, value)
                    })
                }
                [p0, p1] => {
                    let (p0, p1): (Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone());
                    node!(|p0, p1| {
                        let _ = &token;
                        let value: u64 = mix(&[*p0.get(), *p1.get()]);
                        target.set(value);
                        put(slot, value)
                    })
                }
                _ => unreachable!("a writer has one or two parents"),
            };
        }
        match (kind, p.as_slice(), child) {
            (Kind::Creator { keyed: true }, [p0], Some(child)) => {
                let p0: Accessor<u64> = p0.clone();
                node!(|p0| {
                    let _ = &token;
                    let (child, mine): (Arc<AtomicU64>, Arc<Token>) = (child.clone(), Token::new(alive));
                    node!(
                        |p0| {
                            let _ = &mine;
                            put(child, mix(&[*p0.get(), 1]))
                        },
                        key = 0u8
                    );
                    put(slot, mix(&[*p0.get()]))
                })
            }
            (Kind::Creator { keyed: false }, [p0], Some(child)) => {
                let p0: Accessor<u64> = p0.clone();
                node!(|p0| {
                    let _ = &token;
                    let (child, mine): (Arc<AtomicU64>, Arc<Token>) = (child.clone(), Token::new(alive));
                    node!(|p0| {
                        let _ = &mine;
                        put(child, mix(&[*p0.get(), 1]))
                    });
                    put(slot, mix(&[*p0.get()]))
                })
            }
            (Kind::Async, [p0], None) => {
                let p0: Accessor<u64> = p0.clone();
                node!(async |p0| {
                    let _ = &token;
                    tokio::task::yield_now().await;
                    put(slot, mix(&[*p0.get()]))
                })
            }
            (Kind::Async, [p0, p1], None) => {
                let (p0, p1): (Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone());
                node!(async |p0, p1| {
                    let _ = &token;
                    tokio::task::yield_now().await;
                    put(slot, mix(&[*p0.get(), *p1.get()]))
                })
            }
            (Kind::Async, [p0, p1, p2], None) => {
                let (p0, p1, p2): (Accessor<u64>, Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone(), p2.clone());
                node!(async |p0, p1, p2| {
                    let _ = &token;
                    tokio::task::yield_now().await;
                    put(slot, mix(&[*p0.get(), *p1.get(), *p2.get()]))
                })
            }
            (Kind::Whole, [p0, p1], None) => {
                let (p0, p1): (Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone());
                node!(
                    |p0, p1| {
                        let _ = &token;
                        put(slot, mix(&[*p0.get(), *p1.get()]))
                    },
                    batches = WHOLE
                )
            }
            (Kind::Whole, [p0, p1, p2], None) => {
                let (p0, p1, p2): (Accessor<u64>, Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone(), p2.clone());
                node!(
                    |p0, p1, p2| {
                        let _ = &token;
                        put(slot, mix(&[*p0.get(), *p1.get(), *p2.get()]))
                    },
                    batches = WHOLE
                )
            }
            (_, [p0], None) => {
                let p0: Accessor<u64> = p0.clone();
                node!(|p0| {
                    let _ = &token;
                    put(slot, mix(&[*p0.get()]))
                })
            }
            (_, [p0, p1], None) => {
                let (p0, p1): (Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone());
                node!(|p0, p1| {
                    let _ = &token;
                    put(slot, mix(&[*p0.get(), *p1.get()]))
                })
            }
            (_, [p0, p1, p2], None) => {
                let (p0, p1, p2): (Accessor<u64>, Accessor<u64>, Accessor<u64>) = (p0.clone(), p1.clone(), p2.clone());
                node!(|p0, p1, p2| {
                    let _ = &token;
                    put(slot, mix(&[*p0.get(), *p1.get(), *p2.get()]))
                })
            }
            _ => unreachable!("a creator has one parent, the others one to three"),
        }
    }

    /// Every derived node's last run, and every child's, computed its definition from the current values.
    fn check(&self, seed: u64, phase: &str) {
        for (d, node) in self.derived.iter().enumerate() {
            let inputs: Vec<u64> = node.parents.iter().map(|&at: &usize| self.value(at)).collect();
            let last: u64 = node.slot.load(Ordering::SeqCst);
            assert_eq!(
                last,
                mix(&inputs),
                "seed {seed}, {phase}: {} node {d} on {:?} is stale",
                node.kind,
                node.parents
            );
            if let Some(handle) = &node.handle {
                assert_eq!(*handle.get(), last, "seed {seed}, {phase}: node {d}'s value is not its last run's");
            }
            if let Some((target, seen)) = &node.out {
                assert_eq!(*target.get(), last, "seed {seed}, {phase}: writer {d}'s target is stale");
                let want: u64 = mix(&[last, 2]);
                assert_eq!(seen.load(Ordering::SeqCst), want, "seed {seed}, {phase}: writer {d}'s reader is stale");
            }
            if let Some(child) = &node.child {
                let want: u64 = mix(&[inputs[0], 1]);
                assert_eq!(
                    child.load(Ordering::SeqCst),
                    want,
                    "seed {seed}, {phase}: {} {d}'s child is stale",
                    node.kind
                );
            }
        }
    }
}

/// Waits until exactly `want` bodies are alive: a freed node's body is dropped off the freeing thread.
async fn expect_bodies(alive: &AtomicUsize, want: usize, seed: u64, phase: &str) {
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + Duration::from_secs(5);
    while alive.load(Ordering::SeqCst) != want && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(alive.load(Ordering::SeqCst), want, "seed {seed}, {phase}: bodies alive");
}

/// Writes from `tasks` lithia tasks at once, each its own sequence from `seed`, and waits until every
/// run they caused has ended, async ones included (`flush` does not wait for those).
async fn write(sources: &[SourceNode<u64>], seed: u64, tasks: u64) {
    let mut joins: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for task in 0..tasks {
        let sources: Vec<SourceNode<u64>> = sources.to_vec();
        joins.push(lithia::spawn(async move {
            let mut rng: Rng = Rng::new(seed ^ (task + 1).wrapping_mul(0xA24B_AED4_963E_E407));
            for _ in 0..10 + rng.below(30) {
                let at: usize = rng.below(sources.len() as u64) as usize;
                sources[at].set(rng.below(64));
                if rng.below(4) == 0 {
                    tokio::task::yield_now().await;
                }
            }
            lithia::flush().await;
        }));
    }
    for join in joins {
        join.await.unwrap();
    }
    settle(sources).await;
}

/// Gives every source a new value, so every node downstream runs again.
async fn rewrite(sources: &[SourceNode<u64>]) {
    for source in sources {
        source.update(|v: &u64| (v + 1) % 64);
    }
    settle(sources).await;
}

/// Waits until every run downstream of `sources` has ended.
async fn settle(sources: &[SourceNode<u64>]) {
    lithia::flush().await;
    for source in sources {
        lithia::test::settled(source).await;
    }
}

fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("LITHIA_RANDOM_SEED").ok().and_then(|s: String| s.parse::<u64>().ok()) {
        return vec![seed];
    }
    let count: u64 = std::env::var("LITHIA_RANDOM_SEEDS").ok().and_then(|s: String| s.parse().ok()).unwrap_or(24);
    (1..=count).collect()
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_graphs_settle_on_their_definition_and_free_everything() {
    for seed in seeds() {
        let base: usize = lithia::test::live_nodes().await;
        let mut rng: Rng = Rng::new(seed);
        let mut graph: Graph = Graph::build(&mut rng);
        let alive: Arc<AtomicUsize> = graph.alive.clone();
        settle(&graph.sources).await;
        graph.check(seed, "built");

        write(&graph.sources, seed, 3).await;
        graph.check(seed, "written");
        expect_bodies(&alive, graph.bodies(), seed, "written").await;

        // Handles go from nodes nothing depends on: their anchored sources keep them running.
        let first: usize = graph.sources.len();
        for d in 0..graph.derived.len() {
            let leaf: bool = !graph.derived.iter().any(|n: &Derived| n.parents.contains(&(first + d)));
            if leaf && rng.below(2) == 0 {
                graph.derived[d].handle = None;
            }
        }
        write(&graph.sources, seed.wrapping_add(1 << 32), 3).await;
        rewrite(&graph.sources).await;
        graph.check(seed, "leaves without handles");
        rewrite(&graph.sources).await;
        graph.check(seed, "leaves without handles, again");
        expect_bodies(&alive, graph.bodies(), seed, "leaves without handles").await;

        drop(graph);
        assert_eq!(lithia::test::live_nodes().await, base, "seed {seed}: nodes outlived every handle");
        expect_bodies(&alive, 0, seed, "dropped").await;
    }
}
