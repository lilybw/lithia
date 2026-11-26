\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---------------------------- MODULE Lifetimes ----------------------------
(***************************************************************************)
(* lithia's node lifetimes (design spec 10.1-10.5) at the grain of the     *)
(* engine's lock sections: outside handles and their anchor counts,        *)
(* internal handles and leases, support's self-reference, generations,     *)
(* keyed nodes and holdovers, writer edges, scoping, freeing, disposal and *)
(* ghosts, and sweeps under the sweep lock.                                *)
(*                                                                         *)
(* Each step is one atomic operation or one lock section of                *)
(* crates/lithia/src/lifetime/, handles.rs, expand/capture.rs,             *)
(* expand/create.rs, expand/keyed.rs or graph/node.rs. A sweep reads       *)
(* anchor counts while other threads drop handles, so it may see any       *)
(* anchor that fell during it. Freeing is a step of its own, taken by any  *)
(* thread once nothing holds the node.                                     *)
(*                                                                         *)
(* Left out: values, scheduling (Scheduling.tla), and children linked to a *)
(* node while disposal tears it down.                                      *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
    N,          \* nodes are 1..N, numbered in NodeId order: parents are older than their children
    T,          \* threads are 1..T
    Parents,    \* [1..N -> SUBSET 1..N]
    Targets,    \* [1..N -> SUBSET 1..N]: what a node's body may write
    Creator,    \* [1..N -> 0..N]: the node whose run creates it; 0 for a top-level node
    Body,       \* nodes with a body: derived nodes and effects
    Born,       \* nodes alive at the start
    Hold0,      \* [1..T -> [1..N -> Nat]]: outside handles at the start, by thread
    Leased0,    \* [1..T -> [1..N -> Nat]]: handles under a block's lease at the start, by thread
    Scopable,   \* nodes a thread holding a handle may scope
    Keyed,      \* created nodes with a key: their creator's next run holds them over and may reclaim them
    Disposable, \* nodes a thread holding a handle may dispose of
    MaxHandles, \* outside handles per thread and node, at most
    Budget      \* outside actions per behaviour

None == 0
Node == 1..N
Thread == 1..T
Captures(n) == IF n \in Body THEN Parents[n] \cup Targets[n] ELSE {}
Supportable(n) == Creator[n] = None /\ Parents[n] # {} /\ n \in Body
Least(S) == CHOOSE x \in S : \A y \in S : x <= y

RECURSIVE SumF(_, _)
SumF(S, f) == IF S = {} THEN 0 ELSE LET x == CHOOSE y \in S : TRUE IN f[x] + SumF(S \ {x}, f)
\* The handles of node n in h, over all threads.
Total(h, n) == SumF(Thread, [t \in Thread |-> h[t][n]])

VARIABLES
    made,       \* nodes created so far
    live,       \* nodes not freed yet
    hold,       \* outside handles, by thread and node
    leased,     \* handles a block took over, internal until its lease ends
    ended,      \* the lease has ended
    anchors,    \* each node's atomic anchor count
    support,    \* nodes support holds through their self-reference
    letGo,      \* nodes support let go of while they live (the LET_GO_ALIVE count)
    heldBy,     \* the creator whose generation holds a node
    gen,        \* each creator's generation, keyed nodes and holdover
    hover,      \* each creator's holdover: keyed nodes of a released generation
    reg,        \* writers registered in their targets' writer lists
    scoped,
    bodies,     \* freed nodes whose bodies, and the captures in them, are not dropped yet
    creating,   \* the node a thread is creating, and the step it is at
    queue,      \* starting points raised on a thread
    taken,      \* starting points of the sweep under way on a thread
    keep,       \* references a thread's sweep holds until after the lock
    lock,       \* the thread holding the sweep lock for a sweep
    snap,       \* anchors when the sweep under way began
    ghost,      \* disposed nodes still alive
    disposing,  \* the ghosts a thread's disposal is still to tear down
    budget

vars == <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg, scoped,
          bodies, creating, queue, taken, keep, lock, snap, ghost, disposing, budget>>

-----------------------------------------------------------------------------
(* Strong references, and what lithia can see. *)

\* Ghosts torn down: they have left their parents and dropped their bodies.
Torn == ghost \ UNION {disposing[t] : t \in Thread}
PP(n) == IF n \in Torn THEN {} ELSE Parents[n]

Refs(n) ==
      Total(hold, n) + Total(leased, n)
    + Cardinality({c \in live : n \in PP(c)})
    + Cardinality({w \in (live \ Torn) \cup bodies : n \in Captures(w)})
    + (IF n \in support THEN 1 ELSE 0)
    + Cardinality({c \in live : n \in gen[c]})
    + Cardinality({t \in Thread : n \in queue[t] \cup taken[t] \cup keep[t] \cup disposing[t]})
    + Cardinality({t \in Thread : creating[t].node # None /\ Creator[creating[t].node] = n})

\* A node whose last strong reference has gone can no longer be upgraded, though its Drop has not run.
Vis == {n \in live : Refs(n) > 0}

\* Edges support flows along: parent to child, creator to generation, writer to target.
Succ(n) == {c \in Vis : n \in PP(c)} \cup (gen[n] \cap Vis) \cup (Targets[n] \cap Vis)
Pred(n) == (PP(n) \cap Vis) \cup {w \in reg \cap Vis : n \in Targets[w]} \cup ({heldBy[n]} \cap Vis)

\* Nodes that depend on a scoped node with no anchor, directly or through nodes without one.
BlockedBy(A) ==
    LET B[n \in Node] == n \notin A /\ \E p \in PP(n) : (p \in scoped /\ p \notin A) \/ B[p]
    IN {n \in Node : B[n]}

RECURSIVE Fix(_, _)
\* The least superset of S that holds every node of Elig with a predecessor in it.
Fix(S, Elig) ==
    LET U == S \cup {n \in Elig : Pred(n) \cap S # {}} IN IF U = S THEN S ELSE Fix(U, Elig)

\* Support from the anchors A, and from nodes counted as supported without a search.
SupportedBy(A, trusted) ==
    Fix((A \cup trusted) \cap Vis, {n \in Vis : n \notin scoped /\ n \notin BlockedBy(A)})

\* What the specification means: a node with an outside handle anchors, unless it is a ghost.
TrueAnchors == {n \in Vis \ ghost : Total(hold, n) > 0}
TrueSup == SupportedBy(TrueAnchors, {})

\* What the engine reads: the anchor counts of nodes that are not ghosts.
Anchors == {n \in Vis \ ghost : anchors[n] # 0}

RECURSIVE Down(_, _)
\* S and what lies downstream of it, not entering Stop.
Down(S, Stop) ==
    LET U == S \cup {c \in UNION {Succ(m) : m \in S} : c \notin Stop} IN IF U = S THEN S ELSE Down(U, Stop)

\* What a walk from n holds again under the anchors A: n and the nodes downstream of it that support
\* would hold and does not, stopping at nodes it holds, ghosts, scoped nodes and nodes a scoped node blocks.
Rehold(n, A) ==
    LET Stop == scoped \cup support \cup BlockedBy(A) \cup ghost
    IN {m \in Down({c \in Succ(n) : c \notin Stop}, Stop) \cup ({n} \ Stop) : Supportable(m)}

NeedsSweep(n, l) == n \in support \/ Targets[n] # {} \/ (\E c \in l : n \in PP(c)) \/ gen[n] # {}

Idle(t) == queue[t] = {} /\ taken[t] = {} /\ keep[t] = {} /\ creating[t].node = None /\ disposing[t] = {}
Outside(t) == Idle(t) /\ budget > 0 /\ budget' = budget - 1

-----------------------------------------------------------------------------
(* Outside actions. *)

Clone(t, n) ==
    /\ Outside(t)
    /\ hold[t][n] > 0 /\ hold[t][n] < MaxHandles
    /\ hold' = [hold EXCEPT ![t][n] = @ + 1]
    /\ anchors' = [anchors EXCEPT ![n] = @ + 1]
    /\ UNCHANGED <<made, live, leased, ended, support, letGo, heldBy, gen, hover, reg, scoped, bodies,
                   creating, queue, taken, keep, lock, snap, ghost, disposing>>

Send(t, u, n) ==
    /\ Outside(t)
    /\ t # u /\ hold[t][n] > 0 /\ hold[u][n] < MaxHandles
    /\ hold' = [hold EXCEPT ![t][n] = @ - 1, ![u][n] = @ + 1]
    /\ UNCHANGED <<made, live, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg, scoped,
                   bodies, creating, queue, taken, keep, lock, snap, ghost, disposing>>

\* The last anchoring handle's drop raises a starting point; the sweep runs once the step ends.
Drop(t, n) ==
    /\ Outside(t)
    /\ hold[t][n] > 0
    /\ hold' = [hold EXCEPT ![t][n] = @ - 1]
    /\ anchors' = [anchors EXCEPT ![n] = @ - 1]
    /\ queue' = [queue EXCEPT ![t] = IF anchors[n] = 1 /\ NeedsSweep(n, live) THEN @ \cup {n} ELSE @]
    /\ UNCHANGED <<made, live, leased, ended, support, letGo, heldBy, gen, hover, reg, scoped, bodies,
                   creating, taken, keep, lock, snap, ghost, disposing>>

\* A count that rises from zero does so under the sweep lock, which then walks from the node if
\* support has let go of anything still alive; a ghost's count rises without a walk.
Rise(R) ==
    LET A == Anchors \cup R
        re == IF letGo # {} THEN UNION {Rehold(n, A) : n \in R} ELSE {}
    IN /\ support' = support \cup re
       /\ letGo' = letGo \ re

\* A body clones a handle it captured: an outside handle from then on.
BodyClone(t, w, n) ==
    /\ Outside(t)
    /\ w \in Vis \ Torn /\ n \in Captures(w) /\ hold[t][n] < MaxHandles
    /\ hold' = [hold EXCEPT ![t][n] = @ + 1]
    /\ anchors' = [anchors EXCEPT ![n] = @ + 1]
    /\ IF anchors[n] # 0
       THEN UNCHANGED <<support, letGo>>
       ELSE lock = None /\ Rise({n} \ ghost)
    /\ UNCHANGED <<made, live, leased, ended, heldBy, gen, hover, reg, scoped, bodies, creating, queue,
                   taken, keep, lock, snap, ghost, disposing>>

\* A leased handle dropped while the lease runs counts nowhere.
LeasedDrop(t, n) ==
    /\ Outside(t)
    /\ ~ended /\ leased[t][n] > 0
    /\ leased' = [leased EXCEPT ![t][n] = @ - 1]
    /\ UNCHANGED <<made, live, hold, ended, anchors, support, letGo, heldBy, gen, hover, reg, scoped,
                   bodies, creating, queue, taken, keep, lock, snap, ghost, disposing>>

\* The block ends: under the lease's lock, every leased handle still alive counts as an outside
\* handle before any of them can be dropped as one.
LeaseEnd(t) ==
    /\ Outside(t)
    /\ ~ended
    /\ LET add(n) == Total(leased, n)
       IN /\ lock = None \/ \A n \in Node : add(n) > 0 => anchors[n] # 0
          /\ anchors' = [n \in Node |-> anchors[n] + add(n)]
          /\ Rise({n \in Node \ ghost : add(n) > 0 /\ anchors[n] = 0})
    /\ hold' = [u \in Thread |-> [n \in Node |-> hold[u][n] + leased[u][n]]]
    /\ leased' = [u \in Thread |-> [n \in Node |-> 0]]
    /\ ended' = TRUE
    /\ UNCHANGED <<made, live, heldBy, gen, hover, reg, scoped, bodies, creating, queue, taken, keep, lock,
                   snap, ghost, disposing>>

\* lifetime/support.rs scope: support and the creator let go of the node, wherever the creator holds
\* it, and it is a starting point.
Scope(t, n) ==
    /\ Outside(t)
    /\ n \in Scopable /\ n \notin scoped /\ hold[t][n] > 0
    /\ scoped' = scoped \cup {n}
    /\ support' = support \ {n}
    /\ heldBy' = [heldBy EXCEPT ![n] = None]
    /\ gen' = IF heldBy[n] # None THEN [gen EXCEPT ![heldBy[n]] = @ \ {n}] ELSE gen
    /\ hover' = IF heldBy[n] # None THEN [hover EXCEPT ![heldBy[n]] = @ \ {n}] ELSE hover
    /\ queue' = [queue EXCEPT ![t] =
                    IF Targets[n] # {} \/ (\E c \in live : n \in PP(c)) \/ gen[n] # {}
                    THEN @ \cup {n} ELSE @]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, letGo, reg, bodies, creating, taken,
                   keep, lock, snap, ghost, disposing>>

\* A creator's next run releases its generation: each unkeyed member loses its creation edge and is a
\* starting point; keyed members join the holdover, still held (graph/sched.rs take_generation).
CreatorRun(t, c) ==
    /\ Outside(t)
    /\ c \in Vis \ ghost /\ gen[c] \ hover[c] # {}
    /\ LET rel == gen[c] \ Keyed IN
       /\ heldBy' = [n \in Node |-> IF n \in rel THEN None ELSE heldBy[n]]
       /\ gen' = [gen EXCEPT ![c] = @ \cap Keyed]
       /\ hover' = [hover EXCEPT ![c] = gen[c] \cap Keyed]
       /\ queue' = [queue EXCEPT ![t] = @ \cup {m \in rel : NeedsSweep(m, live)}]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, reg, scoped, bodies,
                   creating, taken, keep, lock, snap, ghost, disposing>>

\* expand/keyed.rs reclaim: the creator's run creates the key again and takes the node back.
Reclaim(t, c, n) ==
    /\ Outside(t)
    /\ n \in hover[c]
    /\ hover' = [hover EXCEPT ![c] = @ \ {n}]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, reg, scoped,
                   bodies, creating, queue, taken, keep, lock, snap, ghost, disposing>>

\* protocol/lifecycle.rs finalize: the run ended without a successor, and what it did not reclaim
\* is released.
RunEnd(t, c) ==
    /\ Outside(t)
    /\ hover[c] # {}
    /\ LET rel == hover[c] IN
       /\ heldBy' = [n \in Node |-> IF n \in rel THEN None ELSE heldBy[n]]
       /\ gen' = [gen EXCEPT ![c] = @ \ rel]
       /\ hover' = [hover EXCEPT ![c] = {}]
       /\ queue' = [queue EXCEPT ![t] = @ \cup {m \in rel : NeedsSweep(m, live)}]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, reg, scoped, bodies,
                   creating, taken, keep, lock, snap, ghost, disposing>>

\* expand/create.rs: the node, linked to its parents, with its handle; its creator's run, or the creating
\* code, holds what its body captures and the creator while it is created. A node whose parents are
\* all torn-down ghosts is disposed of once it is made.
CreateAlloc(t, n) ==
    /\ Outside(t)
    /\ n \notin made
    /\ Captures(n) \cup Parents[n] \subseteq Vis
    /\ Creator[n] = None \/ Creator[n] \in Vis
    /\ made' = made \cup {n}
    /\ live' = live \cup {n}
    /\ anchors' = [anchors EXCEPT ![n] = 1]
    /\ hold' = [hold EXCEPT ![t][n] = 1]
    /\ creating' = [creating EXCEPT ![t] = [node |-> n, step |-> "hold",
                                            orphan |-> Parents[n] # {} /\ Parents[n] \subseteq Torn]]
    /\ UNCHANGED <<leased, ended, support, letGo, heldBy, gen, hover, reg, scoped, bodies, queue, taken,
                   keep, lock, snap, ghost, disposing>>

NoCreation == [node |-> None, step |-> "none", orphan |-> FALSE]
AfterHold(c) == IF c.orphan THEN [c EXCEPT !.step = "orphan"] ELSE NoCreation

\* The creator's generation, unless the creator was torn down, then support's self-reference, unless
\* disposal made the node a ghost meanwhile.
CreateHold(t) ==
    /\ creating[t].node # None /\ creating[t].step = "hold"
    /\ LET n == creating[t].node
           c == Creator[n]
           held == c # None /\ c \notin Torn
       IN /\ heldBy' = IF held THEN [heldBy EXCEPT ![n] = c] ELSE heldBy
          /\ gen' = IF held THEN [gen EXCEPT ![c] = @ \cup {n}] ELSE gen
          /\ support' = IF Supportable(n) /\ n \notin ghost THEN support \cup {n} ELSE support
          /\ creating' = [creating EXCEPT ![t] =
                            IF Targets[n] = {} THEN AfterHold(creating[t]) ELSE [@ EXCEPT !.step = "register"]]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, letGo, hover, reg, scoped, bodies, queue,
                   taken, keep, lock, snap, ghost, disposing, budget>>

\* Under the sweep lock, the writer joins its targets' writer lists and, if it is supported, holds
\* again what support let go of downstream.
CreateRegister(t) ==
    /\ creating[t].node # None /\ creating[t].step = "register" /\ lock = None
    /\ LET n == creating[t].node
           re == IF letGo # {} /\ n \in SupportedBy(Anchors, {}) THEN Rehold(n, Anchors) ELSE {}
       IN /\ reg' = reg \cup {n}
          /\ support' = support \cup re
          /\ letGo' = letGo \ re
    /\ creating' = [creating EXCEPT ![t] = AfterHold(creating[t])]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, heldBy, gen, hover, scoped, bodies, queue,
                   taken, keep, lock, snap, ghost, disposing, budget>>

\* lifetime/dispose.rs: the node and the descendants, in NodeId order, whose parents are all freed or
\* ghosts become ghosts, which are never anchors from then on.
Kids(n) == {c \in live : n \in PP(c)}
RECURSIVE Desc(_)
Desc(n) == Kids(n) \cup UNION {Desc(c) : c \in Kids(n)}
RECURSIVE FreeWalk(_, _)
FreeWalk(cs, fs) ==
    IF cs = {} THEN fs
    ELSE LET x == Least(cs)
             free == x \notin ghost /\ \A q \in Parents[x] : q \in fs \/ q \in ghost
         IN FreeWalk(cs \ {x}, IF free THEN fs \cup {x} ELSE fs)

DisposeMark(t, r) ==
    /\ Outside(t)
    /\ r \in Disposable /\ r \in Vis \ ghost /\ hold[t][r] > 0
    /\ LET F == FreeWalk(Desc(r), {r}) IN
       /\ ghost' = ghost \cup F
       /\ disposing' = [disposing EXCEPT ![t] = F]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg,
                   scoped, bodies, creating, queue, taken, keep, lock, snap>>

\* Tearing the ghosts down: each drops its support and its body, leaves its parents, and releases its
\* generation, keyed nodes and holdover; the members, the ghosts' targets and the surviving
\* descendants are starting points, swept once the operation ends.
TearDown(t, F, surv) ==
    LET members == UNION {gen[f] : f \in F}
        torn == Torn \cup F
        PP2(c) == IF c \in torn THEN {} ELSE Parents[c]
        Needs(x) == (x \in support \ F) \/ Targets[x] # {} \/ (\E c \in live : x \in PP2(c))
                    \/ (x \notin F /\ gen[x] # {})
        raised == {x \in members \cup ((UNION {Targets[f] : f \in F}) \cap Vis) \cup surv : Needs(x)}
    IN /\ support' = support \ F
       /\ heldBy' = [m \in Node |-> IF m \in members THEN None ELSE heldBy[m]]
       /\ gen' = [c \in Node |-> IF c \in F THEN {} ELSE gen[c]]
       /\ hover' = [c \in Node |-> IF c \in F THEN {} ELSE hover[c]]
       /\ bodies' = bodies \cup {f \in F : Captures(f) # {}}
       /\ queue' = [queue EXCEPT ![t] = @ \cup raised]

DisposeTear(t) ==
    /\ disposing[t] # {}
    /\ LET F == disposing[t] IN TearDown(t, F, (UNION {Desc(f) : f \in F}) \ ghost)
    /\ disposing' = [disposing EXCEPT ![t] = {}]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, letGo, reg, scoped, creating, taken, keep,
                   lock, snap, ghost, budget>>

\* dispose_unchecked on a new node whose parents are all ghosts.
CreateOrphan(t) ==
    /\ creating[t].node # None /\ creating[t].step = "orphan"
    /\ LET n == creating[t].node IN
       /\ ghost' = ghost \cup {n}
       /\ TearDown(t, {n}, {})
    /\ creating' = [creating EXCEPT ![t] = NoCreation]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, letGo, reg, scoped, taken, keep, lock, snap,
                   disposing, budget>>

-----------------------------------------------------------------------------
(* lithia's own steps. *)

\* graph/node.rs Drop on the thread that let go of the last reference: it leaves its parents, its body
\* is queued to drop unless disposal dropped it, its targets are starting points, and its generation,
\* keyed nodes and holdover are released.
Free(t, n) ==
    /\ n \in live /\ Refs(n) = 0
    /\ LET l == live \ {n}
           members == gen[n]
           raised == {x \in (Targets[n] \cap Vis) \cup members : NeedsSweep(x, l)}
       IN /\ live' = l
          /\ heldBy' = [m \in Node |-> IF m \in members THEN None ELSE heldBy[m]]
          /\ gen' = [gen EXCEPT ![n] = {}]
          /\ hover' = [hover EXCEPT ![n] = {}]
          /\ queue' = [queue EXCEPT ![t] = @ \cup raised]
    /\ letGo' = letGo \ {n}
    /\ reg' = reg \ {n}
    /\ bodies' = IF Captures(n) # {} /\ n \notin Torn THEN bodies \cup {n} ELSE bodies
    /\ UNCHANGED <<made, hold, leased, ended, anchors, support, scoped, creating, taken, keep,
                   lock, snap, ghost, disposing, budget>>

\* The blocking pool drops a freed node's body, and the handles it captured.
BodyDrop(w) ==
    /\ w \in bodies
    /\ bodies' = bodies \ {w}
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg,
                   scoped, creating, queue, taken, keep, lock, snap, ghost, disposing, budget>>

SweepBegin(t) ==
    /\ queue[t] # {} /\ taken[t] = {} /\ keep[t] = {} /\ creating[t].node = None /\ disposing[t] = {}
    /\ lock = None
    /\ lock' = t
    /\ taken' = [taken EXCEPT ![t] = queue[t]]
    /\ queue' = [queue EXCEPT ![t] = {}]
    /\ snap' = Anchors
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg,
                   scoped, bodies, creating, keep, ghost, disposing, budget>>

\* lifetime/support.rs sweep. Counts only fall while the lock is held, so it reads every anchor it had at
\* some point: any of those that fell may still count. A start's exact check may give up.
SweepEnd(t) ==
    /\ lock = t
    /\ \E fell \in SUBSET (snap \ Anchors), gaveUp \in SUBSET taken[t] :
         LET A == (Anchors \cup fell) \cap Vis
             Elig == {n \in Vis : n \notin scoped /\ n \notin BlockedBy(A)}
             exact == Fix(A, Elig)
             lost == (taken[t] \ exact) \cup (gaveUp \ A)
             R == Down(lost, A)
             sup == Fix(A \cup ((support \cap Vis) \ R), Elig)
             L == {n \in R : n \in support /\ n \notin sup}
         IN /\ support' = support \ L
            /\ letGo' = letGo \cup L
            /\ keep' = [keep EXCEPT ![t] = taken[t] \cup R]
    /\ taken' = [taken EXCEPT ![t] = {}]
    /\ lock' = None
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, heldBy, gen, hover, reg, scoped, bodies,
                   creating, queue, snap, ghost, disposing, budget>>

\* After the lock, the sweep drops the references it held, which may free nodes.
KeepDrop(t) ==
    /\ keep[t] # {} /\ lock # t
    /\ keep' = [keep EXCEPT ![t] = {}]
    /\ UNCHANGED <<made, live, hold, leased, ended, anchors, support, letGo, heldBy, gen, hover, reg,
                   scoped, bodies, creating, queue, taken, lock, snap, ghost, disposing, budget>>

-----------------------------------------------------------------------------

Init ==
    /\ made = Born
    /\ live = Born
    /\ hold = Hold0
    /\ leased = Leased0
    /\ ended = FALSE
    /\ anchors = [n \in Node |-> Total(Hold0, n)]
    /\ support = {n \in Born : Supportable(n)}
    /\ letGo = {}
    /\ heldBy = [n \in Node |-> IF n \in Born THEN Creator[n] ELSE None]
    /\ gen = [c \in Node |-> {n \in Born : Creator[n] = c}]
    /\ hover = [c \in Node |-> {}]
    /\ reg = {n \in Born : Targets[n] # {}}
    /\ scoped = {}
    /\ bodies = {}
    /\ creating = [t \in Thread |-> NoCreation]
    /\ queue = [t \in Thread |-> {}]
    /\ taken = [t \in Thread |-> {}]
    /\ keep = [t \in Thread |-> {}]
    /\ lock = None
    /\ snap = {}
    /\ ghost = {}
    /\ disposing = [t \in Thread |-> {}]
    /\ budget = Budget

Internal ==
    \/ \E t \in Thread :
          \/ CreateHold(t) \/ CreateRegister(t) \/ CreateOrphan(t) \/ DisposeTear(t)
          \/ SweepBegin(t) \/ SweepEnd(t) \/ KeepDrop(t)
    \/ \E t \in Thread, n \in Node : Free(t, n)
    \/ \E w \in Node : BodyDrop(w)

Next ==
    \/ Internal
    \/ \E t \in Thread, n \in Node :
          \/ Clone(t, n) \/ Drop(t, n) \/ LeasedDrop(t, n) \/ Scope(t, n) \/ CreatorRun(t, n)
          \/ RunEnd(t, n) \/ CreateAlloc(t, n) \/ DisposeMark(t, n)
          \/ \E w \in Node : BodyClone(t, w, n) \/ Reclaim(t, w, n)
          \/ \E u \in Thread : Send(t, u, n)
    \/ \E t \in Thread : LeaseEnd(t)

Spec == Init /\ [][Next]_vars /\ WF_vars(Internal)

-----------------------------------------------------------------------------
(* Properties. *)

Quiescent ==
    /\ \A t \in Thread : Idle(t)
    /\ lock = None /\ bodies = {}
    /\ \A n \in live : Refs(n) > 0

\* Every anchor count is the number of outside handles.
CountsExact == \A n \in Node : anchors[n] = Total(hold, n)

\* A node's parents, captures and generation live while it does, until disposal tears it down.
RefsHeld == /\ \A n \in live \ Torn : Parents[n] \cup gen[n] \subseteq live
            /\ \A w \in (live \ Torn) \cup bodies : Captures(w) \subseteq live

\* Support never lets go of a node an anchor still reaches, unless it is scoped or disposed of.
LetGoSound == [][\A n \in support \ support' : n \in scoped' \/ n \in ghost' \/ n \notin TrueSup]_vars

\* Once lithia has nothing left to do, support holds exactly the supportable nodes an anchor reaches.
NoLeak == Quiescent => support \subseteq TrueSup
SupportHolds ==
    Quiescent => \A n \in Vis \ ghost : (Supportable(n) /\ n \notin scoped /\ n \in TrueSup) => n \in support

\* A node is in its creator's generation exactly when it names that creator, and a holdover holds
\* only keyed members of it.
GenerationsExact ==
    /\ \A c, m \in Node : m \in gen[c] <=> heldBy[m] = c
    /\ \A c \in Node : hover[c] \subseteq gen[c] \cap Keyed

\* A torn-down ghost holds nothing and support never holds it.
GhostsReleased == \A g \in Torn : g \notin support /\ gen[g] = {} /\ hover[g] = {}

Terminates == <>[]Quiescent
=============================================================================
