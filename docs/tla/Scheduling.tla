\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---------------------------- MODULE Scheduling ----------------------------
(***************************************************************************)
(* lithia's scheduling protocol (design spec 7.2-7.4, 8, 9, 10.4, 11.4) at *)
(* the grain of the engine's lock sections: framing, releases, deciding    *)
(* and settling entries, runs with supersession, handover and protection,  *)
(* generators, writer sets with cuts and cut reruns, the epoch tree's      *)
(* accounting, batches = WHOLE, creator order, panics and resilience,      *)
(* reflows, and disposal with its ghosts and survivors' reruns.            *)
(*                                                                         *)
(* Processes are outside writers (one batch each), run tasks (Slots per    *)
(* node), cut reruns and disposals. Each step below is one lock section or *)
(* one atomic operation of crates/lithia/src/protocol/, run/, graph/ and   *)
(* lifetime/dispose.rs; a process's pending frames, seeds, releases and    *)
(* resumes are sets, so they are handled in every order, a superset of the *)
(* engine's.                                                               *)
(*                                                                         *)
(* Left out: node creation and first runs, lifetimes (Lifetimes.tla),      *)
(* keyed reclaiming, async source blocks, runtime shutdown, defers and the *)
(* rerun rate. A created node stays held by its creator. Values are        *)
(* abstract: a body changes its value or not, and writes or reflows any of *)
(* its targets, chosen freely while the clock allows.                      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    N,          \* nodes are 1..N, numbered in NodeId (topological) order
    Parents,    \* [1..N -> SUBSET 1..N]: every parent is older than its child
    Sources,    \* nodes without a body, written from outside or by bodies
    Async,      \* derived nodes whose runs may suspend and poll again
    Generators, \* async nodes whose bodies yield items: a yield publishes and keeps the run's tokens
    Changing,   \* derived nodes whose runs may change their value; the others always compute an equal one
    Panics,     \* derived nodes whose runs may panic
    FailAny,    \* derived nodes under resilience = NONE: they fail while any parent is failed
    FailAll,    \* derived nodes under resilience = PARTIAL: they fail while every parent is failed
    Whole,      \* derived nodes under batches = WHOLE
    WholeBound, \* epochs a whole node lets become ready while it waits, before it runs anyway
    CreatedBy,  \* {<<creator, node>>}: nodes a run of their creator made, which it keeps
    Disposed,   \* nodes disposed of from outside, one disposal each
    GhostFails, \* nodes under disposed = FAIL: as ghosts, they count as failed parents
    Targets,    \* [1..N -> SUBSET 1..N]: the sources a body may write and the derived nodes it may reflow
    Batches,    \* <<B1, B2, ...>>: one outside writer per batch: it writes its sources, reflows its derived nodes
    MaxClock,   \* bodies change values only while the clock stays within this
    MaxEpochs,  \* epoch ids available; running out is flagged
    MaxPolls,   \* polls per run, at most
    Slots,      \* run tasks per node alive at once
    MaxHooks    \* cut reruns available; running out is flagged

Node == 1..N
Derived == Node \ Sources
Children(n) == {c \in Node : n \in Parents[c]}
Epoch == 1..MaxEpochs
Creator(n) == IF \E x \in CreatedBy : x[2] = n THEN (CHOOSE x \in CreatedBy : x[2] = n)[1] ELSE 0
Procs == {<<"W", i>> : i \in 1..Len(Batches)}
            \cup {<<"T", n, k>> : n \in Derived, k \in 1..Slots}
            \cup {<<"H", j>> : j \in 1..MaxHooks}
            \cup {<<"D", n>> : n \in Disposed}
NoBodyWrites == \A n \in Node : Targets[n] = {}
Resilient == FailAny \cup FailAll = {}

Max(a, b) == IF a >= b THEN a ELSE b
Least(S) == CHOOSE x \in S : \A y \in S : x <= y
EmptyF == [x \in {} |-> 0]
Put(f, k, v) == [x \in DOMAIN f \cup {k} |-> IF x = k THEN v ELSE f[x]]
Zero == [e \in Epoch |-> 0]
SetToSeq(S) == CHOOSE f \in [1..Cardinality(S) -> S] : \A i, j \in DOMAIN f : i # j => f[i] # f[j]
RECURSIVE SumP(_, _)
SumP(S, f) == IF S = {} THEN 0 ELSE LET x == CHOOSE y \in S : TRUE IN f[x] + SumP(S \ {x}, f)
RECURSIVE Desc(_)
Desc(n) == Children(n) \cup UNION {Desc(c) : c \in Children(n)}

\* A run in flight: the epochs it serves (sv), superseded (restart), protected (replaced),
\* its cancel flag (cx) and the nodes a cut rerun leaves alone (sup).
NoRun == [on |-> FALSE, sv |-> EmptyF, restart |-> FALSE, replaced |-> FALSE, cx |-> FALSE, sup |-> {}]
\* An entry: releases each parent still owes it (w), writer set, newest change, carry, forced, due for
\* a reflow, cut, runnable. A parent framed again in an epoch before its earlier release arrived owes two.
NoWait == [x \in Node |-> 0]
Ready(w) == \A x \in Node : w[x] = 0
NewEntry(w, wr, fo) == [w |-> w, wr |-> wr, nw |-> 0, ca |-> 0, fo |-> fo, rf |-> FALSE, cut |-> FALSE, rn |-> FALSE]
InitLoc(p) == [n |-> IF p[1] \in {"T", "D"} THEN p[2] ELSE 0, todo |-> IF p[1] = "W" THEN Batches[p[2]] ELSE {},
               ws |-> {}, own |-> 0, out |-> "end", polls |-> 0, first |-> TRUE, live |-> FALSE, held |-> FALSE,
               snap |-> {}, rel |-> {}, cx |-> FALSE, done |-> FALSE, prot |-> FALSE, fl |-> FALSE,
               e |-> 0, par |-> 0, wr |-> {}, dl |-> {}, rr |-> {}]

VARIABLES
    clock, version, ranAt, relAt,
    ent, run, inh, queued, wdef, failed, byPar, gated, spawned, suppIn,
    epOn, epPar, own, sub, settled, wlog, hooks, scope, nextE,
    undelivered,
    pc, fr, plan, rl, sp, hk, cl, cd, sd, ev, rs, loc,
    hookFree, anomaly, glitch,
    ghost, retired, reflowAt, dueAt, broken

timeV == <<clock, version, ranAt, relAt>>
nodeX == <<wdef, failed, byPar, gated>>
nodeV == <<ent, run, inh, queued, nodeX>>
epV == <<epOn, epPar, own, sub, settled, wlog, hooks, scope, nextE>>
workV == <<fr, plan, rl, sp, hk, cl, cd, sd, ev, rs>>
miscV == <<spawned, suppIn, undelivered, hookFree, anomaly, glitch>>
lifeV == <<ghost, retired, reflowAt, dueAt, broken>>
vars == <<timeV, nodeV, epV, workV, miscV, lifeV, pc, loc>>

-----------------------------------------------------------------------------
(* Node and work records, so Decide and Settle can be composed in one step. *)

NS(n) == [ent |-> ent[n], run |-> run[n], inh |-> inh[n], q |-> queued[n], wd |-> wdef[n],
          fd |-> failed[n], bp |-> byPar[n], gt |-> gated[n]]
WK(p) == [rl |-> rl[p], sp |-> sp[p], hk |-> hk[p], cl |-> cl[p], rs |-> rs[p]]
SetNS(n, s) ==
    /\ ent' = [ent EXCEPT ![n] = s.ent] /\ run' = [run EXCEPT ![n] = s.run]
    /\ inh' = [inh EXCEPT ![n] = s.inh] /\ queued' = [queued EXCEPT ![n] = s.q]
    /\ wdef' = [wdef EXCEPT ![n] = s.wd] /\ failed' = [failed EXCEPT ![n] = s.fd]
    /\ byPar' = [byPar EXCEPT ![n] = s.bp] /\ gated' = [gated EXCEPT ![n] = s.gt]
SetWK(p, w) ==
    /\ rl' = [rl EXCEPT ![p] = w.rl] /\ sp' = [sp EXCEPT ![p] = w.sp]
    /\ hk' = [hk EXCEPT ![p] = w.hk] /\ cl' = [cl EXCEPT ![p] = w.cl] /\ rs' = [rs EXCEPT ![p] = w.rs]

RelTo(n, e, ca, wr) == {[f |-> n, t |-> c, e |-> e, ca |-> ca, wr |-> wr] : c \in Children(n)}
RunOwes(s, e) == s.run.on /\ e \in DOMAIN s.run.sv /\ ~s.run.sv[e].rel
InhOwes(s, e) == e \in DOMAIN s.inh /\ ~s.inh[e].rel

\* whole_epochs_wait (protocol/propagate.rs): a whole node waits while an epoch that reached it is not ready.
WholeWait(n, s) == n \in Whole /\ s.wd < WholeBound /\ \E x \in DOMAIN s.ent : ~Ready(s.ent[x].w)
\* Entry::reflow_only: due only for a reflow, with no change newer than the last run's start.
ReflowOnly(en, n) == en.rf /\ ~en.fo /\ en.nw <= ranAt[n]
\* queue_whole: a whole node owes a run once nothing it waits for is left: a handover or a runnable entry.
WholeDue(n, s) == /\ n \in Whole /\ ~s.run.on /\ ~s.q /\ ~WholeWait(n, s)
                  /\ (s.inh # EmptyF \/ \E x \in DOMAIN s.ent : s.ent[x].rn)

\* settle (protocol/propagate.rs): close the entry, wake the nodes it gated, queue a whole node's run
\* once nothing it waits for is left; release the children unless a run still owes them. A join queues
\* a whole node's run the same way.
Settle(n, e, s, w) ==
    LET en == s.ent[e]
        Merge(r) == [r EXCEPT !.ca = Max(@, en.ca), !.wr = @ \cup en.wr]
        s0 == [s EXCEPT !.ent = [x \in DOMAIN s.ent \ {e} |-> s.ent[x]], !.gt = {},
                        !.wd = IF DOMAIN s.ent = {e} THEN 0 ELSE @]
        due == WholeDue(n, s0)
        s1 == IF due THEN [s0 EXCEPT !.q = TRUE] ELSE s0
        s2 == IF RunOwes(s, e) THEN [s1 EXCEPT !.run.sv[e] = Merge(@)]
              ELSE IF InhOwes(s, e) THEN [s1 EXCEPT !.inh[e] = Merge(@)]
              ELSE s1
        w0 == [w EXCEPT !.rs = @ \cup s.gt, !.sp = IF due THEN @ \cup {n} ELSE @]
        w1 == IF RunOwes(s, e) \/ InhOwes(s, e) THEN w0
              ELSE [w0 EXCEPT !.rl = @ \cup RelTo(n, e, en.ca, en.wr)]
    IN <<s2, [w1 EXCEPT !.cl[e] = @ + 1]>>

\* join_run (protocol/propagate.rs): the entry's epoch joins the run in flight, or the handover a
\* superseded one left, whose successor releases it with its first item or when it ends; the entry's
\* token goes with it.
Join(n, e, s, w) ==
    LET en == s.ent[e]
        s1 == [s EXCEPT !.ent = [x \in DOMAIN s.ent \ {e} |-> s.ent[x]], !.gt = {},
                        !.wd = IF DOMAIN s.ent = {e} THEN 0 ELSE @]
        sv == IF s.run.on THEN s.run.sv ELSE s.inh
        rec == IF e \in DOMAIN sv
               THEN [sv[e] EXCEPT !.tok = @ + 1, !.rel = FALSE, !.wr = @ \cup en.wr, !.ca = Max(@, en.ca)]
               ELSE [tok |-> 1, rel |-> FALSE, wr |-> en.wr, ca |-> en.ca]
        s2 == IF s.run.on THEN [s1 EXCEPT !.run.sv = Put(@, e, rec)] ELSE [s1 EXCEPT !.inh = Put(@, e, rec)]
        due == WholeDue(n, s2)
    IN <<IF due THEN [s2 EXCEPT !.q = TRUE] ELSE s2, [w EXCEPT !.rs = @ \cup s.gt, !.sp = IF due THEN @ \cup {n} ELSE @]>>

\* A run in flight that has not released yet, or a superseded one's handover that has not: its first
\* poll, or its successor's, is still to be marked.
Unreleased(s) ==
    IF s.run.on THEN \E f \in DOMAIN s.run.sv : ~s.run.sv[f].rel ELSE \E f \in DOMAIN s.inh : ~s.inh[f].rel

\* parent_failures (protocol/propagate.rs): a ghost parent counts as failed under disposed = FAIL.
FailedSeen(q) == IF q \in ghost THEN q \in GhostFails ELSE failed[q]
GivesWay(n) ==
    LET F == {q \in Parents[n] : FailedSeen(q)} IN F # {} /\ (n \in FailAny \/ (n \in FailAll /\ F = Parents[n]))
Supersede(r) == IF r.on /\ ~r.restart THEN [r EXCEPT !.restart = TRUE, !.cx = TRUE] ELSE r

\* decide (protocol/propagate.rs): a ready entry is cut, fails its node through its parents, settles
\* unchanged, starts a run or supersedes one. A node its parents no longer fail is forced to rerun if a
\* parent changed since its last run started, and otherwise restored. An unchanged entry that carries a
\* change absorbed by a run started after it joins that run, or the handover it left; once the run has
\* released, the entry settles with the node's version, so its children test the change. A whole node defers its run
\* while another entry of it waits, up to the bound.
Decide(n, e, s, w) ==
    LET en == s.ent[e] IN
    IF n \in ghost THEN Settle(n, e, s, w)
    ELSE IF en.cut
    THEN Settle(n, e, s, [w EXCEPT !.hk = @ \cup {[n |-> n, e |-> e, wr |-> en.wr]}])
    ELSE IF GivesWay(n)
    THEN Settle(n, e, [s EXCEPT !.fd = TRUE, !.bp = TRUE, !.run = Supersede(@)], w)
    ELSE LET force == s.bp /\ \E q \in Parents[n] : version[q] > ranAt[n]
             s0 == IF s.bp /\ ~force THEN [s EXCEPT !.fd = FALSE, !.bp = FALSE] ELSE s
             en0 == [en EXCEPT !.fo = @ \/ force]
             sx == [s0 EXCEPT !.ent[e] = en0]
         IN IF ~(en0.fo \/ en0.rf \/ en0.nw > ranAt[n])
            THEN IF en0.nw > 0 /\ Unreleased(sx)
                 THEN Join(n, e, sx, w)
                 ELSE IF en0.nw > 0
                 THEN Settle(n, e, [sx EXCEPT !.ent[e].ca = Max(@, version[n])], w)
                 ELSE Settle(n, e, sx, w)
            ELSE LET s1 == [sx EXCEPT !.ent[e].rn = TRUE]
                     defer == WholeWait(n, s1) /\ ~s1.run.on
                     wd2 == IF defer THEN s1.wd + 1 ELSE s1.wd
                     wait == WholeWait(n, s1) /\ ~(defer /\ wd2 >= WholeBound)
                     s2 == [s1 EXCEPT !.wd = wd2]
                 IN IF ~s2.run.on
                    THEN IF wait \/ s2.q THEN <<s2, w>> ELSE <<[s2 EXCEPT !.q = TRUE], [w EXCEPT !.sp = @ \cup {n}]>>
                    ELSE IF ~ReflowOnly(en0, n) /\ ~s2.run.restart /\ ~s2.run.replaced
                    THEN <<[s2 EXCEPT !.run.restart = TRUE, !.run.cx = TRUE], w>>
                    ELSE <<s2, w>>

RECURSIVE DecideAll(_, _, _, _)
DecideAll(n, es, s, w) ==
    IF es = <<>> THEN <<s, w>>
    ELSE LET e == Head(es)
             r == IF e \in DOMAIN s.ent /\ Ready(s.ent[e].w) /\ ~s.ent[e].rn THEN Decide(n, e, s, w) ELSE <<s, w>>
         IN DecideAll(n, Tail(es), r[1], r[2])

RECURSIVE SettleAll(_, _, _, _)
SettleAll(n, es, s, w) ==
    IF es = <<>> THEN <<s, w>> ELSE LET r == Settle(n, Head(es), s, w) IN SettleAll(n, Tail(es), r[1], r[2])

\* close_served (protocol/lifecycle.rs): unreleased records release their children unchanged; tokens close.
CloseServed(n, sv, w) ==
    [w EXCEPT !.rl = @ \cup UNION {RelTo(n, e, sv[e].ca, sv[e].wr) : e \in {x \in DOMAIN sv : ~sv[x].rel}},
              !.cl = [e \in Epoch |-> @[e] + (IF e \in DOMAIN sv THEN sv[e].tok ELSE 0)]]

\* retire (protocol/lifecycle.rs): a ghost's entries settle, its handover and run close, the run is cancelled.
Retire(n, s, w) ==
    LET r1 == SettleAll(n, SetToSeq(DOMAIN s.ent), s, w)
        s1 == r1[1]
        w1 == CloseServed(n, s1.inh, r1[2])
        w2 == IF s1.run.on THEN CloseServed(n, s1.run.sv, w1) ELSE w1
    IN <<[s1 EXCEPT !.inh = EmptyF, !.q = FALSE, !.gt = {}, !.run = NoRun], [w2 EXCEPT !.rs = @ \cup s1.gt]>>

\* Sched::decides (graph/sched.rs): the creator may still replace or keep what it created.
Decides(c) ==
    \/ \E e \in DOMAIN ent[c] : ~ent[c][e].rn
    \/ ~run[c].on /\ (queued[c] \/ inh[c] # EmptyF \/ \E e \in DOMAIN ent[c] : ent[c][e].rn)
    \/ run[c].on /\ run[c].restart

\* dispose (lifetime/dispose.rs): the descendants, in NodeId order, whose parents are all freed or ghosts.
RECURSIVE FreeWalk(_, _)
FreeWalk(cs, fs) ==
    IF cs = {} THEN fs
    ELSE LET x == Least(cs)
             free == x \notin ghost /\ \A q \in Parents[x] : q \in fs \/ q \in ghost
         IN FreeWalk(cs \ {x}, IF free THEN fs \cup {x} ELSE fs)

-----------------------------------------------------------------------------
(* The epoch tree (graph/epoch.rs): own counts open entries, sub adds      *)
(* child epochs with open work, and the scope counts root epochs with open *)
(* work.                                                                   *)

RECURSIVE SubInc(_, _, _, _)
SubInc(s, sc, e, par) ==
    IF s[e] = 0
    THEN IF par[e] = 0 THEN <<[s EXCEPT ![e] = 1], sc + 1>> ELSE SubInc([s EXCEPT ![e] = 1], sc, par[e], par)
    ELSE <<[s EXCEPT ![e] = @ + 1], sc>>

RECURSIVE SubDec(_, _, _, _, _)
SubDec(s, sc, e, k, par) ==
    IF s[e] = k
    THEN IF par[e] = 0 THEN <<[s EXCEPT ![e] = 0], sc - 1>> ELSE SubDec([s EXCEPT ![e] = 0], sc, par[e], 1, par)
    ELSE <<[s EXCEPT ![e] = @ - k], sc>>

RECURSIVE SubIncAll(_, _, _, _)
SubIncAll(s, sc, es, par) ==
    IF es = <<>> THEN <<s, sc>> ELSE LET r == SubInc(s, sc, Head(es), par) IN SubIncAll(r[1], r[2], Tail(es), par)

RECURSIVE Line(_)
Line(e) == IF e = 0 THEN {} ELSE {e} \cup Line(epPar[e])
\* Epoch.written_by: what n wrote in these epochs and the epochs they descend from.
WrittenBy(es, n) == {t \in Node : \E a \in UNION {Line(e) : e \in es} : <<n, t>> \in wlog[a]}

\* deliver_into (protocol/deliver.rs): the writes and reflows `ws` into the epochs of `tg` (<<epoch,
\* writers>> pairs), routed with `wr` added: frames from each written source, a seed on each reflowed
\* derived node, then the sources' releases and the reflowed nodes' evaluation.
Wrote(ws) == {x \in ws : x[1] \in Sources}
Reflowed(ws) == {x \in ws : x[1] \in Derived}
DelivFr(ws, tg) == UNION {{<<x[1], c, y[1]>> : c \in Children(x[1])} : x \in Wrote(ws), y \in tg}
DelivSd(ws, tg, wr) ==
    {[n |-> x[1], e |-> y[1], ca |-> x[2], wr |-> y[2] \cup wr, k |-> "reflow"] : x \in Reflowed(ws), y \in tg}
DelivPlan(ws, tg, wr) == {[f |-> x[1], e |-> y[1], ca |-> x[2], wr |-> y[2] \cup wr] : x \in Wrote(ws), y \in tg}
DelivEv(ws, tg) == {<<x[1], y[1]>> : x \in Reflowed(ws), y \in tg}

-----------------------------------------------------------------------------
(* Generic steps: framing, seeding, releasing, resuming, the end of a      *)
(* drain, closing tokens.                                                  *)

FramePC == {"w_frame", "t_frame", "h_frame", "d_frame"}
DrainPC == {"w_drain", "t_drain", "t_mdrain", "t_fdrain", "t_sdrain", "h_drain", "d_rdrain", "d_drain"}
ClosePC == {"w_close", "t_close", "t_end", "h_close", "d_rclose", "d_close"}

\* frame (protocol/propagate.rs), one child under its lock; a ghost child or parent is skipped, since
\* a ghost's retire may have released the child already.
FrameStep(p) ==
    /\ pc[p] \in FramePC
    /\ \E x \in fr[p] :
        LET a == x[1]  c == x[2]  e == x[3] IN
        IF c \in ghost \/ a \in ghost
        THEN /\ fr' = [fr EXCEPT ![p] = @ \ {x}]
             /\ UNCHANGED <<ent, own, sub, scope>>
        ELSE IF e \in DOMAIN ent[c]
        THEN /\ ent' = [ent EXCEPT ![c][e].w[a] = @ + 1, ![c][e].rn = FALSE]
             /\ fr' = [fr EXCEPT ![p] = @ \ {x}]
             /\ UNCHANGED <<own, sub, scope>>
        ELSE LET owes == RunOwes(NS(c), e) \/ InhOwes(NS(c), e)
                 si == SubInc(sub, scope, e, epPar)
             IN /\ ent' = [ent EXCEPT ![c] = Put(@, e, NewEntry([NoWait EXCEPT ![a] = 1], {}, FALSE))]
                /\ own' = [own EXCEPT ![e] = @ + 1]
                /\ sub' = si[1] /\ scope' = si[2]
                /\ fr' = [fr EXCEPT ![p] = (@ \ {x}) \cup (IF owes THEN {} ELSE {<<c, g, e>> : g \in Children(c)})]
    /\ UNCHANGED <<timeV, run, inh, queued, nodeX, epOn, epPar, settled, wlog, hooks, nextE>>
    /\ UNCHANGED <<plan, rl, sp, hk, cl, cd, sd, ev, rs, miscV, lifeV, pc, loc>>

\* seed and seed_reflow (protocol/propagate.rs): a survivor's forced rerun or a derived node's reflow,
\* cut if its writer is on the chain; a new entry frames the node's children.
SeedStep(p) ==
    /\ pc[p] \in FramePC
    /\ \E x \in sd[p] :
        LET n == x.n  e == x.e  s == NS(n)
            Mod(en) == [en EXCEPT !.fo = @ \/ x.k = "rerun", !.rf = @ \/ x.k = "reflow", !.ca = Max(@, x.ca),
                                  !.wr = @ \cup x.wr, !.cut = @ \/ (x.k = "reflow" /\ n \in x.wr)]
        IN /\ sd' = [sd EXCEPT ![p] = @ \ {x}]
           /\ IF n \in ghost
              THEN UNCHANGED <<ent, own, sub, scope, fr>>
              ELSE IF e \in DOMAIN s.ent
              THEN /\ ent' = [ent EXCEPT ![n][e] = Mod(@)]
                   /\ UNCHANGED <<own, sub, scope, fr>>
              ELSE LET owes == RunOwes(s, e) \/ InhOwes(s, e)
                       si == SubInc(sub, scope, e, epPar)
                   IN /\ ent' = [ent EXCEPT ![n] = Put(@, e, Mod(NewEntry(NoWait, {}, FALSE)))]
                      /\ own' = [own EXCEPT ![e] = @ + 1]
                      /\ sub' = si[1] /\ scope' = si[2]
                      /\ fr' = [fr EXCEPT ![p] = @ \cup (IF owes THEN {} ELSE {<<n, c, e>> : c \in Children(n)})]
    /\ UNCHANGED <<timeV, run, inh, queued, nodeX, epOn, epPar, settled, wlog, hooks, nextE>>
    /\ UNCHANGED <<plan, rl, sp, hk, cl, cd, ev, rs, miscV, lifeV, pc, loc>>

\* The whole region is framed and seeded: the delivery's own releases and evaluations start.
FrameEnd(p) ==
    /\ pc[p] \in FramePC /\ fr[p] = {} /\ sd[p] = {}
    /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(x.f, x.e, x.ca, x.wr) : x \in plan[p]})
                                 \cup {[f |-> 0, t |-> y[1], e |-> y[2], ca |-> 0, wr |-> {}] : y \in ev[p]}]
    /\ plan' = [plan EXCEPT ![p] = {}]
    /\ ev' = [ev EXCEPT ![p] = {}]
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "w_frame" -> "w_drain"
                                 [] pc[p] = "t_frame" -> "t_drain"
                                 [] pc[p] = "d_frame" -> "d_drain"
                                 [] OTHER -> "h_eval"]
    /\ UNCHANGED <<timeV, nodeV, epV, fr, sp, hk, cl, cd, sd, rs, miscV, lifeV, loc>>

\* release_children (protocol/propagate.rs), one child under its lock; or evaluate, for a seeded node.
ReleaseStep(p) ==
    /\ pc[p] \in DrainPC
    /\ \E m \in rl[p] :
        LET c == m.t  e == m.e  w0 == [WK(p) EXCEPT !.rl = @ \ {m}] IN
        IF m.f = 0
        THEN LET r == DecideAll(c, SetToSeq(DOMAIN ent[c]), NS(c), w0) IN SetNS(c, r[1]) /\ SetWK(p, r[2])
        ELSE IF e \in DOMAIN ent[c] /\ ent[c][e].w[m.f] > 0
        THEN LET en == ent[c][e]
                 wr2 == en.wr \cup m.wr
                 en2 == [en EXCEPT !.w[m.f] = @ - 1, !.wr = wr2, !.nw = Max(@, m.ca), !.cut = @ \/ c \in wr2]
                 s2 == [NS(c) EXCEPT !.ent[e] = en2]
                 r == IF Ready(en2.w) THEN Decide(c, e, s2, w0) ELSE <<s2, w0>>
             IN SetNS(c, r[1]) /\ SetWK(p, r[2])
        ELSE /\ rl' = [rl EXCEPT ![p] = @ \ {m}]
             /\ UNCHANGED <<nodeV, sp, hk, cl, rs>>
    /\ UNCHANGED <<timeV, epV, fr, plan, cd, sd, ev, miscV, lifeV, pc, loc>>

\* drain's resumes (protocol/work.rs queue_if_runnable): a gated node with work left is queued again.
ResumeStep(p) ==
    /\ pc[p] \in DrainPC /\ rl[p] = {}
    /\ \E n \in rs[p] :
        LET ready == inh[n] # EmptyF \/ \E e \in DOMAIN ent[n] : ent[n][e].rn /\ Ready(ent[n][e].w)
            go == ready /\ ~run[n].on /\ ~queued[n] /\ n \notin ghost
        IN /\ rs' = [rs EXCEPT ![p] = @ \ {n}]
           /\ queued' = IF go THEN [queued EXCEPT ![n] = TRUE] ELSE queued
           /\ sp' = IF go THEN [sp EXCEPT ![p] = @ \cup {n}] ELSE sp
    /\ UNCHANGED <<timeV, ent, run, inh, nodeX, epV, fr, plan, rl, hk, cl, cd, sd, ev, miscV, lifeV, pc, loc>>

\* The end of drain_deferred: cut reruns wait for their epoch, spawned tasks become runnable.
DrainEnd(p) ==
    /\ pc[p] \in DrainPC /\ rl[p] = {} /\ rs[p] = {}
    /\ hooks' = [e \in Epoch |-> hooks[e] \cup {[n |-> h.n, wr |-> h.wr] : h \in {h2 \in hk[p] : h2.e = e}}]
    /\ anomaly' = IF \E h \in hk[p] : own[h.e] = 0 THEN anomaly \cup {"cut rerun waits on a settled epoch"} ELSE anomaly
    /\ spawned' = [n \in Node |-> spawned[n] \/ n \in sp[p]]
    /\ hk' = [hk EXCEPT ![p] = {}]
    /\ sp' = [sp EXCEPT ![p] = {}]
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "w_drain" -> "w_close"
                                 [] pc[p] = "t_drain" -> "t_close"
                                 [] pc[p] = "t_mdrain" -> "t_aftermark"
                                 [] pc[p] = "t_fdrain" -> "t_end"
                                 [] pc[p] = "t_sdrain" -> "t_poll"
                                 [] pc[p] = "d_rdrain" -> "d_rclose"
                                 [] pc[p] = "d_drain" -> "d_close"
                                 [] OTHER -> "h_close"]
    /\ UNCHANGED <<timeV, nodeV, epOn, epPar, own, sub, settled, wlog, scope, nextE>>
    /\ UNCHANGED <<fr, plan, rl, cl, cd, sd, ev, rs, suppIn, undelivered, hookFree, glitch, lifeV, loc>>

\* Tokens a close step may close: a writer, a cut rerun and a disposal's reruns close their drain's
\* tokens before their guard; deliver_into closes its drain's at once; a run task closes the rest at its
\* poll's end; a disposal closes what each retire closed.
Pending(p, e) ==
    CASE pc[p] \in {"w_close", "h_close", "d_close"} -> IF cl[p] # Zero THEN cl[p][e] ELSE cd[p][e]
      [] pc[p] \in {"t_close", "d_rclose"} -> cl[p][e]
      [] OTHER -> cl[p][e] + cd[p][e]

\* close_tokens (graph/epoch.rs): the last open entry settles the epoch, which starts its cut reruns,
\* each in a new child epoch, before the epoch's own count leaves the tree.
CloseStep(p) ==
    /\ pc[p] \in ClosePC
    /\ \E e \in Epoch :
        LET k == Pending(p, e)
            o == own[e] - k
            L == IF o = 0 THEN SetToSeq(hooks[e]) ELSE <<>>
            slots == SetToSeq(hookFree)
            ok == Len(L) <= Len(slots) /\ nextE + Len(L) - 1 <= MaxEpochs
            cnt == IF ok THEN Len(L) ELSE 0
            New(x) == x >= nextE /\ x < nextE + cnt
            Hp(i) == <<"H", slots[i]>>
            HP == {Hp(i) : i \in 1..cnt}
            Ix(q) == CHOOSE i \in 1..cnt : Hp(i) = q
            par2 == [x \in Epoch |-> IF New(x) THEN e ELSE epPar[x]]
            sub2 == [x \in Epoch |-> IF New(x) THEN 1 ELSE IF x = e THEN sub[e] + cnt ELSE sub[x]]
            sd2 == SubDec(sub2, scope, e, k, par2)
            ClearCl(q) == pc[p] \in {"t_close", "t_end", "d_rclose"} \/ (pc[p] \in {"w_close", "h_close", "d_close"} /\ cl[p] # Zero)
            ClearCd(q) == pc[p] = "t_end" \/ (pc[p] \in {"w_close", "h_close", "d_close"} /\ cl[p] = Zero)
        IN /\ k > 0
           /\ own' = [x \in Epoch |-> IF x = e THEN o ELSE IF New(x) THEN 1 ELSE own[x]]
           /\ sub' = sd2[1] /\ scope' = sd2[2]
           /\ settled' = IF o = 0 THEN [settled EXCEPT ![e] = TRUE] ELSE settled
           /\ hooks' = IF o = 0 /\ ok THEN [hooks EXCEPT ![e] = {}] ELSE hooks
           /\ epOn' = epOn \cup {x \in Epoch : New(x)}
           /\ epPar' = par2
           /\ nextE' = nextE + cnt
           /\ hookFree' = hookFree \ {slots[i] : i \in 1..cnt}
           /\ anomaly' = IF ok THEN anomaly ELSE anomaly \cup {"out of epochs or cut-rerun slots"}
           /\ cl' = [cl EXCEPT ![p][e] = IF ClearCl(p) THEN 0 ELSE @]
           /\ cd' = [q \in Procs |-> IF q \in HP THEN [Zero EXCEPT ![nextE + Ix(q) - 1] = 1]
                                     ELSE IF q = p /\ ClearCd(p) THEN [cd[p] EXCEPT ![e] = 0] ELSE cd[q]]
           /\ pc' = [q \in Procs |-> IF q \in HP THEN "h_supp" ELSE pc[q]]
           /\ loc' = [q \in Procs |-> IF q \in HP
                                      THEN [loc[q] EXCEPT !.n = L[Ix(q)].n, !.wr = L[Ix(q)].wr,
                                                          !.e = nextE + Ix(q) - 1, !.par = e]
                                      ELSE loc[q]]
    /\ UNCHANGED <<timeV, nodeV, wlog, fr, plan, rl, sp, hk, sd, ev, rs, spawned, suppIn, undelivered, glitch, lifeV>>

CloseEnd(p) ==
    /\ pc[p] \in ClosePC
    /\ \A e \in Epoch : Pending(p, e) = 0
    /\ pc[p] \in {"w_close", "h_close", "d_close"} => cd[p] = Zero
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "t_close" -> "t_mark"
                                 [] pc[p] = "t_end" -> IF loc[p].live THEN "t_susp" ELSE "idle"
                                 [] pc[p] = "d_rclose" -> "d_retire"
                                 [] OTHER -> "done"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, lifeV, loc>>

-----------------------------------------------------------------------------
(* Outside writers: write the batch, then deliver it as a root epoch. A    *)
(* derived node in a batch is reflowed: its version takes a new stamp, and *)
(* it reruns in the epoch (api.rs reflow).                                 *)

W_Write(p) ==
    /\ pc[p] = "w_apply"
    /\ \E s \in loc[p].todo :
        IF s \in ghost
        THEN /\ loc' = [loc EXCEPT ![p].todo = @ \ {s}]
             /\ UNCHANGED <<clock, version, undelivered, reflowAt>>
        ELSE /\ clock' = clock + 1
             /\ version' = [version EXCEPT ![s] = clock + 1]
             /\ undelivered' = undelivered \cup {<<s, clock + 1>>}
             /\ reflowAt' = IF s \in Derived THEN [reflowAt EXCEPT ![s] = clock + 1] ELSE reflowAt
             /\ loc' = [loc EXCEPT ![p].todo = @ \ {s}, ![p].ws = @ \cup {<<s, clock + 1>>}]
    /\ UNCHANGED <<ranAt, relAt, nodeV, epV, workV, spawned, suppIn, hookFree, anomaly, glitch>>
    /\ UNCHANGED <<ghost, retired, dueAt, broken, pc>>

W_Deliver(p) ==
    /\ pc[p] = "w_apply" /\ loc[p].todo = {}
    /\ LET e == nextE  ws == loc[p].ws  tg == {<<e, {}>>} IN
       IF ws = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "done"]
            /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, lifeV, loc>>
       ELSE IF e > MaxEpochs
       THEN /\ anomaly' = anomaly \cup {"out of epochs or cut-rerun slots"}
            /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, suppIn, undelivered, hookFree, glitch, lifeV, pc, loc>>
       ELSE /\ nextE' = e + 1
            /\ epOn' = epOn \cup {e}
            /\ own' = [own EXCEPT ![e] = 1]
            /\ sub' = [sub EXCEPT ![e] = 1]
            /\ scope' = scope + 1
            /\ cd' = [cd EXCEPT ![p][e] = 1]
            /\ wlog' = [wlog EXCEPT ![e] = {<<0, x[1], x[2]>> : x \in ws}]
            /\ fr' = [fr EXCEPT ![p] = DelivFr(ws, tg)]
            /\ sd' = [sd EXCEPT ![p] = DelivSd(ws, tg, {})]
            /\ plan' = [plan EXCEPT ![p] = DelivPlan(ws, tg, {})]
            /\ ev' = [ev EXCEPT ![p] = DelivEv(ws, tg)]
            /\ undelivered' = undelivered \ ws
            /\ pc' = [pc EXCEPT ![p] = "w_frame"]
            /\ UNCHANGED <<timeV, nodeV, epPar, settled, hooks, rl, sp, hk, cl, rs>>
            /\ UNCHANGED <<spawned, suppIn, hookFree, anomaly, glitch, lifeV, loc>>

-----------------------------------------------------------------------------
(* Run tasks (run/future.rs RunFuture; protocol/lifecycle.rs start_run,    *)
(* after_poll, protect_ends, finalize).                                    *)

\* q still owes c a release in e: an entry, a run or handover holding e, or one under way.
OwesTo(q, c, e) ==
    \/ e \in DOMAIN ent[q] \/ RunOwes(NS(q), e) \/ InhOwes(NS(q), e)
    \/ \E p \in Procs : (\E m \in rl[p] : m.f = q /\ m.t = c /\ m.e = e) \/ (\E x \in plan[p] : x.f = q /\ x.e = e)

T_Pick(p) ==
    /\ pc[p] = "idle" /\ spawned[loc[p].n]
    /\ spawned' = [spawned EXCEPT ![loc[p].n] = FALSE]
    /\ pc' = [pc EXCEPT ![p] = "t_start"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, suppIn, undelivered, hookFree, anomaly, glitch, lifeV, loc>>

\* start_run: a node its creator may still replace or keep waits, registered with the creator; a whole
\* node waits while an epoch that reached it is not ready. Otherwise it takes over the handover and its
\* ready entries: a ghost closes them, a node its parents fail fails and closes them, any other starts.
\* Either way it wakes the nodes it gated.
T_Start(p) ==
    /\ pc[p] = "t_start"
    /\ LET n == loc[p].n
           s == NS(n)
           cr == Creator(n)
           st == IF s.run.on \/ cr = 0 \/ cr \in ghost THEN "free"
                 ELSE IF n \in ghost \/ ~Decides(cr) THEN "kept" ELSE "gated"
           take == {e \in DOMAIN s.ent : s.ent[e].rn /\ Ready(s.ent[e].w)}
           Rec(e) == IF e \in DOMAIN s.inh /\ e \in take
                     THEN [tok |-> s.inh[e].tok + 1, rel |-> FALSE, wr |-> s.inh[e].wr \cup s.ent[e].wr,
                           ca |-> Max(s.inh[e].ca, s.ent[e].ca)]
                     ELSE IF e \in take THEN [tok |-> 1, rel |-> FALSE, wr |-> s.ent[e].wr, ca |-> s.ent[e].ca]
                     ELSE s.inh[e]
           served == [e \in DOMAIN s.inh \cup take |-> Rec(e)]
           try == ~s.run.on /\ ~WholeWait(n, s) /\ st # "gated" /\ DOMAIN served # {}
           fails == try /\ n \notin ghost /\ GivesWay(n)
           go == try /\ n \notin ghost /\ ~fails
           wake == gated[n] # {}
       IN /\ queued' = [queued EXCEPT ![n] = FALSE]
          /\ gated' = [x \in Node |-> IF x = n THEN {} ELSE IF x = cr /\ st = "gated" THEN gated[x] \cup {n} ELSE gated[x]]
          /\ rs' = [rs EXCEPT ![p] = @ \cup gated[n]]
          /\ IF go
             THEN /\ ent' = [ent EXCEPT ![n] = [e \in DOMAIN s.ent \ take |-> s.ent[e]]]
                  /\ inh' = [inh EXCEPT ![n] = EmptyF]
                  /\ run' = [run EXCEPT ![n] = [on |-> TRUE, sv |-> served, restart |-> FALSE,
                                                replaced |-> DOMAIN s.inh # {}, cx |-> FALSE,
                                                sup |-> WrittenBy(suppIn[n], n)]]
                  /\ suppIn' = [suppIn EXCEPT ![n] = {}]
                  /\ ranAt' = [ranAt EXCEPT ![n] = clock]
                  /\ failed' = [failed EXCEPT ![n] = FALSE]
                  /\ byPar' = [byPar EXCEPT ![n] = FALSE]
                  /\ wdef' = [wdef EXCEPT ![n] = 0]
                  /\ glitch' = (glitch \/ \E e \in take, q \in Parents[n] \ ghost : OwesTo(q, n, e))
                  /\ broken' = broken
                        \cup (IF n \in Whole /\ s.wd < WholeBound /\ \E e \in DOMAIN s.ent \ take : ~Ready(s.ent[e].w)
                              THEN {"whole"} ELSE {})
                        \cup (IF cr # 0 /\ cr \notin ghost /\ Decides(cr) THEN {"creator"} ELSE {})
                  /\ loc' = [loc EXCEPT ![p].first = TRUE, ![p].polls = 0, ![p].live = TRUE, ![p].held = TRUE]
                  /\ pc' = [pc EXCEPT ![p] = IF wake THEN "t_sdrain" ELSE "t_poll"]
                  /\ UNCHANGED <<rl, cl>>
             ELSE IF try
             THEN /\ ent' = [ent EXCEPT ![n] = [e \in DOMAIN s.ent \ take |-> s.ent[e]]]
                  /\ inh' = [inh EXCEPT ![n] = EmptyF]
                  /\ failed' = IF fails THEN [failed EXCEPT ![n] = TRUE] ELSE failed
                  /\ byPar' = IF fails THEN [byPar EXCEPT ![n] = TRUE] ELSE byPar
                  /\ LET w == CloseServed(n, served, WK(p)) IN rl' = [rl EXCEPT ![p] = w.rl] /\ cl' = [cl EXCEPT ![p] = w.cl]
                  /\ pc' = [pc EXCEPT ![p] = "t_fdrain"]
                  /\ UNCHANGED <<run, suppIn, ranAt, wdef, glitch, broken, loc>>
             ELSE /\ pc' = [pc EXCEPT ![p] = IF wake THEN "t_fdrain" ELSE "idle"]
                  /\ UNCHANGED <<ent, inh, run, suppIn, ranAt, failed, byPar, wdef, glitch, broken, rl, cl, loc>>
    /\ UNCHANGED <<clock, version, relAt, epV, fr, plan, sp, hk, cd, sd, ev, spawned, undelivered, hookFree, anomaly>>
    /\ UNCHANGED <<ghost, retired, reflowAt, dueAt>>

\* One poll of the body: it may write or reflow its targets and, when it ends (a generator: when it
\* yields), publish a changed value, which is discarded if the run was superseded by then (run/future.rs
\* publish). A body may panic instead of ending. A node without children never changes: nothing could
\* observe it, so the model saves those states; nor does a node outside Changing.
Outs(n, polls) ==
    (IF n \in Async /\ polls + 1 < MaxPolls
     THEN {"end", "susp"} \cup (IF n \in Generators THEN {"yield"} ELSE {}) ELSE {"end"})
    \cup (IF n \in Panics THEN {"panic"} ELSE {})
T_Poll(p) ==
    /\ pc[p] = "t_poll"
    /\ LET n == loc[p].n  r == run[n] IN
       \E chg \in BOOLEAN, wr \in SUBSET (Targets[n] \ (r.sup \cup ghost)), out \in Outs(n, loc[p].polls) :
         LET pub == chg /\ r.on /\ ~r.cx /\ n \notin ghost
             k == Cardinality(wr) + (IF pub THEN 1 ELSE 0)
             St(t) == clock + Cardinality({u \in wr : u <= t})
             mine == IF pub THEN clock + k ELSE 0
         IN /\ chg => /\ out = IF n \in Generators THEN "yield" ELSE "end"
                      /\ Children(n) # {} /\ n \in Changing
            /\ k = 0 \/ clock + k <= MaxClock
            /\ clock' = clock + k
            /\ version' = [x \in Node |-> IF x \in wr THEN St(x) ELSE IF x = n /\ pub THEN mine ELSE version[x]]
            /\ undelivered' = undelivered \cup {<<t, St(t)>> : t \in wr}
                                \cup (IF pub /\ ~loc[p].first THEN {<<n, mine>>} ELSE {})
            /\ reflowAt' = [x \in Node |-> IF x \in wr /\ x \in Derived THEN St(x) ELSE reflowAt[x]]
            /\ loc' = [loc EXCEPT ![p].ws = {<<t, St(t)>> : t \in wr}, ![p].own = mine,
                                  ![p].out = out, ![p].polls = @ + 1, ![p].held = @ /\ out = "yield"]
            /\ pc' = [pc EXCEPT ![p] = IF out = "panic" THEN "t_fail" ELSE "t_snap"]
    /\ UNCHANGED <<ranAt, relAt, nodeV, epV, workV, spawned, suppIn, hookFree, anomaly, glitch>>
    /\ UNCHANGED <<ghost, retired, dueAt, broken>>

\* after_poll on a panic: only the current run fails its node. A run that fails after it released
\* its epochs has its children told, as with an item that changed nothing.
T_Fail(p) ==
    /\ pc[p] = "t_fail"
    /\ LET n == loc[p].n  cur == run[n].on /\ ~run[n].cx /\ n \notin ghost IN
       /\ failed' = IF cur THEN [failed EXCEPT ![n] = TRUE] ELSE failed
       /\ loc' = [loc EXCEPT ![p].fl = cur /\ ~loc[p].first]
    /\ pc' = [pc EXCEPT ![p] = "t_snap"]
    /\ UNCHANGED <<timeV, ent, run, inh, queued, wdef, byPar, gated, epV, workV, miscV, lifeV>>

\* after_poll's snapshot of the run under its node's lock. A run disposal tore down has nothing to
\* deliver into: its writes go out as a batch of their own, which the model leaves to bodies without targets.
T_Snap(p) ==
    /\ pc[p] = "t_snap"
    /\ LET n == loc[p].n  r == run[n] IN
       IF r.on
       THEN /\ loc' = [loc EXCEPT ![p].snap = {<<e, r.sv[e].wr>> : e \in DOMAIN r.sv},
                                  ![p].rel = {<<e, r.sv[e].wr>> : e \in {x \in DOMAIN r.sv : r.sv[x].rel}},
                                  ![p].cx = r.restart \/ r.cx, ![p].done = FALSE, ![p].prot = FALSE]
            /\ pc' = [pc EXCEPT ![p] = IF loc[p].first THEN "t_rec" ELSE "t_child"]
            /\ UNCHANGED anomaly
       ELSE /\ anomaly' = IF n \in ghost /\ loc[p].ws = {} THEN anomaly ELSE anomaly \cup {"poll without a run"}
            /\ loc' = [loc EXCEPT ![p].live = FALSE]
            /\ pc' = [pc EXCEPT ![p] = "t_end"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, suppIn, undelivered, hookFree, glitch, lifeV>>

\* deliver_into for the first poll: writes and reflows go to the served epochs, routed with the writer added.
T_Rec(p) ==
    /\ pc[p] = "t_rec"
    /\ LET n == loc[p].n  ws == loc[p].ws  snap == loc[p].snap IN
       IF ws = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<wlog, fr, sd, plan, ev, undelivered>>
       ELSE /\ wlog' = [e \in Epoch |-> IF \E y \in snap : y[1] = e THEN wlog[e] \cup {<<n, x[1]>> : x \in ws} ELSE wlog[e]]
            /\ fr' = [fr EXCEPT ![p] = DelivFr(ws, snap)]
            /\ sd' = [sd EXCEPT ![p] = DelivSd(ws, snap, {n})]
            /\ plan' = [plan EXCEPT ![p] = DelivPlan(ws, snap, {n})]
            /\ ev' = [ev EXCEPT ![p] = DelivEv(ws, snap)]
            /\ undelivered' = undelivered \ ws
            /\ pc' = [pc EXCEPT ![p] = "t_frame"]
    /\ UNCHANGED <<timeV, nodeV, epOn, epPar, own, sub, settled, hooks, scope, nextE>>
    /\ UNCHANGED <<rl, sp, hk, cl, cd, rs, spawned, suppIn, hookFree, anomaly, glitch, lifeV, loc>>

\* deliver_children: one child epoch per epoch in `es` (<<epoch, writers>> pairs), carrying the writes
\* and reflows `ws` and, with a stamp or a failure (st), the node's own item.
ChildDel(p, es, ws, stamp, st) ==
    LET n == loc[p].n
        S == SetToSeq(es)
        cnt == Len(S)
        New(x) == x >= nextE /\ x < nextE + cnt
        Of(x) == S[x - nextE + 1]
        kids == [i \in 1..cnt |-> nextE + i - 1]
        tg == {<<kids[i], S[i][2]>> : i \in 1..cnt}
        item == stamp > 0 \/ st
        si == SubIncAll(sub, scope, [i \in 1..cnt |-> S[i][1]], epPar)
    IN IF (ws = {} /\ ~item) \/ cnt = 0
       THEN /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<epV, fr, plan, cd, sd, ev, anomaly>>
       ELSE IF nextE + cnt - 1 > MaxEpochs
       THEN /\ anomaly' = anomaly \cup {"out of epochs or cut-rerun slots"}
            /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<epV, fr, plan, cd, sd, ev>>
       ELSE /\ nextE' = nextE + cnt
            /\ epOn' = epOn \cup {x \in Epoch : New(x)}
            /\ epPar' = [x \in Epoch |-> IF New(x) THEN Of(x)[1] ELSE epPar[x]]
            /\ own' = [x \in Epoch |-> IF New(x) THEN 1 ELSE own[x]]
            /\ sub' = [x \in Epoch |-> IF New(x) THEN 1 ELSE si[1][x]]
            /\ scope' = si[2]
            /\ cd' = [cd EXCEPT ![p] = [x \in Epoch |-> IF New(x) THEN @[x] + 1 ELSE @[x]]]
            /\ wlog' = [x \in Epoch |-> IF New(x) THEN {<<n, y[1]>> : y \in ws} ELSE wlog[x]]
            /\ fr' = [fr EXCEPT ![p] = DelivFr(ws, tg)
                         \cup (IF item THEN {<<n, c, kids[i]>> : c \in Children(n), i \in 1..cnt} ELSE {})]
            /\ sd' = [sd EXCEPT ![p] = DelivSd(ws, tg, {n})]
            /\ plan' = [plan EXCEPT ![p] = DelivPlan(ws, tg, {n})
                         \cup (IF item THEN {[f |-> n, e |-> kids[i], ca |-> stamp, wr |-> S[i][2]] : i \in 1..cnt} ELSE {})]
            /\ ev' = [ev EXCEPT ![p] = DelivEv(ws, tg)]
            /\ pc' = [pc EXCEPT ![p] = "t_frame"]
            /\ UNCHANGED <<settled, hooks, anomaly>>

\* A later poll's batch and item, delivered even if the run was superseded after it published.
T_Child(p) ==
    /\ pc[p] = "t_child"
    /\ undelivered' = undelivered \ (loc[p].ws \cup {<<loc[p].n, loc[p].own>>})
    /\ ChildDel(p, loc[p].snap, loc[p].ws, loc[p].own, loc[p].fl)
    /\ UNCHANGED <<timeV, nodeV, rl, sp, hk, cl, rs, spawned, suppIn, hookFree, glitch, lifeV, loc>>

\* after_poll once the batch is delivered: a first item reaches the epochs released already through
\* child epochs; a yield ends a replacement run's protection, so input that waited for its first item
\* supersedes it; a superseded run hands its item's stamp to the run replacing it; otherwise, under the
\* node's lock, the first poll releases the children (or hands the release to an entry that took it
\* over), and every poll that does not yield closes the run's tokens.
T_Mark(p) ==
    /\ pc[p] = "t_mark"
    /\ LET n == loc[p].n  s == NS(n)  sv == s.run.sv  it == loc[p].own  first == loc[p].first
           yld == loc[p].out = "yield" IN
       IF first /\ it > 0 /\ loc[p].rel # {} /\ ~loc[p].done
       THEN /\ ChildDel(p, loc[p].rel, {}, it, FALSE)
            /\ loc' = [loc EXCEPT ![p].done = TRUE]
            /\ UNCHANGED <<timeV, nodeV, rl, sp, hk, cl, rs, spawned, suppIn, undelivered, hookFree, glitch, lifeV>>
       ELSE IF yld /\ ~loc[p].cx /\ ~loc[p].prot
       THEN LET waiting == \E e \in DOMAIN s.ent : s.ent[e].rn /\ Ready(s.ent[e].w) /\ ~ReflowOnly(s.ent[e], n)
            IN /\ run' = IF s.run.on /\ s.run.replaced
                         THEN [run EXCEPT ![n].replaced = FALSE, ![n].restart = @ \/ waiting, ![n].cx = @ \/ waiting]
                         ELSE run
               /\ loc' = [loc EXCEPT ![p].prot = TRUE]
               /\ UNCHANGED <<timeV, ent, inh, queued, nodeX, epV, workV, miscV, lifeV, pc>>
       ELSE IF loc[p].cx
       THEN /\ run' = [run EXCEPT ![n].sv = [e \in DOMAIN sv |->
                     IF first /\ it > 0 /\ ~sv[e].rel THEN [sv[e] EXCEPT !.ca = Max(@, it)] ELSE sv[e]]]
            /\ pc' = [pc EXCEPT ![p] = "t_fin"]
            /\ UNCHANGED <<timeV, ent, inh, queued, nodeX, epV, workV, miscV, lifeV, loc>>
       ELSE LET Mk(e) == first /\ ~sv[e].rel
                sv2 == [e \in DOMAIN sv |-> [sv[e] EXCEPT !.rel = @ \/ Mk(e), !.ca = IF Mk(e) THEN Max(@, it) ELSE @,
                                                          !.tok = IF yld THEN @ ELSE 0]]
                toEnt == {e \in DOMAIN sv : Mk(e) /\ e \in DOMAIN s.ent}
                toRel == {e \in DOMAIN sv : Mk(e) /\ e \notin DOMAIN s.ent}
            IN /\ ent' = [ent EXCEPT ![n] = [e \in DOMAIN s.ent |->
                              IF e \in toEnt THEN [s.ent[e] EXCEPT !.ca = Max(@, sv2[e].ca), !.wr = @ \cup sv[e].wr]
                              ELSE s.ent[e]]]
               /\ run' = [run EXCEPT ![n].sv = sv2]
               /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(n, e, sv2[e].ca, sv[e].wr) : e \in toRel})]
               /\ cl' = [cl EXCEPT ![p] = [e \in Epoch |-> @[e] + (IF e \in DOMAIN sv /\ ~yld THEN sv[e].tok ELSE 0)]]
               /\ relAt' = IF first /\ s.run.on THEN [relAt EXCEPT ![n] = ranAt[n]] ELSE relAt
               /\ pc' = [pc EXCEPT ![p] = "t_mdrain"]
               /\ UNCHANGED <<clock, version, ranAt, inh, queued, nodeX, epV, fr, plan, sp, hk, cd, sd, ev, rs>>
               /\ UNCHANGED <<miscV, lifeV, loc>>

\* poll_run after after_poll: an ended or failed run finalizes; a suspended or yielded one finalizes if
\* superseded or torn down.
T_AfterMark(p) ==
    /\ pc[p] = "t_aftermark"
    /\ LET r == run[loc[p].n] IN
       pc' = [pc EXCEPT ![p] = IF loc[p].out \in {"end", "panic"} \/ r.cx \/ ~r.on THEN "t_fin" ELSE "t_end"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, lifeV, loc>>

\* finalize: a superseded run hands its epochs over; otherwise it releases what it still owes, closes
\* its tokens, and starts a follow-up run for entries that waited for it. It wakes the nodes it gated.
\* A run disposal tore down finds nothing to finalize.
T_Fin(p) ==
    /\ pc[p] = "t_fin"
    /\ LET n == loc[p].n  s == NS(n)  r == s.run IN
       IF ~r.on
       THEN UNCHANGED <<nodeV, rl, cl, sp, rs>>
       ELSE /\ run' = [run EXCEPT ![n] = NoRun]
            /\ gated' = [gated EXCEPT ![n] = {}]
            /\ rs' = [rs EXCEPT ![p] = @ \cup s.gt]
            /\ IF r.restart
               THEN /\ inh' = [inh EXCEPT ![n] = r.sv]
                    /\ queued' = [queued EXCEPT ![n] = TRUE]
                    /\ sp' = [sp EXCEPT ![p] = IF s.q THEN @ ELSE @ \cup {n}]
                    /\ UNCHANGED <<ent, rl, cl>>
               ELSE LET unrel == {e \in DOMAIN r.sv : ~r.sv[e].rel}
                        ent2 == [e \in DOMAIN s.ent |->
                                   IF e \in unrel THEN [s.ent[e] EXCEPT !.ca = Max(@, r.sv[e].ca), !.wr = @ \cup r.sv[e].wr]
                                   ELSE s.ent[e]]
                        follow == /\ ~WholeWait(n, [s EXCEPT !.ent = ent2]) /\ n \notin ghost
                                  /\ \E e \in DOMAIN ent2 : ent2[e].rn /\ Ready(ent2[e].w)
                    IN /\ ent' = [ent EXCEPT ![n] = ent2]
                       /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(n, e, r.sv[e].ca, r.sv[e].wr) : e \in unrel \ DOMAIN s.ent})]
                       /\ cl' = [cl EXCEPT ![p] = [e \in Epoch |-> @[e] + (IF e \in DOMAIN r.sv THEN r.sv[e].tok ELSE 0)]]
                       /\ queued' = [queued EXCEPT ![n] = s.q \/ follow]
                       /\ sp' = [sp EXCEPT ![p] = IF follow /\ ~s.q THEN @ \cup {n} ELSE @]
                       /\ UNCHANGED inh
            /\ UNCHANGED <<wdef, failed, byPar>>
    /\ loc' = [loc EXCEPT ![p].live = FALSE]
    /\ pc' = [pc EXCEPT ![p] = "t_fdrain"]
    /\ UNCHANGED <<timeV, epV, fr, plan, hk, cd, sd, ev, miscV, lifeV>>

\* A suspended or yielded run is polled again: superseded or torn down, it finalizes instead.
T_Resume(p) ==
    /\ pc[p] = "t_susp"
    /\ LET r == run[loc[p].n] IN pc' = [pc EXCEPT ![p] = IF r.cx \/ ~r.on THEN "t_fin" ELSE "t_poll"]
    /\ loc' = [loc EXCEPT ![p].first = FALSE]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, lifeV>>

-----------------------------------------------------------------------------
(* Cut reruns (protocol/propagate.rs rerun_cut), when their epoch settles. *)

H_Supp(p) ==
    /\ pc[p] = "h_supp"
    /\ suppIn' = IF loc[p].n \in ghost THEN suppIn ELSE [suppIn EXCEPT ![loc[p].n] = @ \cup {loc[p].par}]
    /\ pc' = [pc EXCEPT ![p] = "h_seed"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, undelivered, hookFree, anomaly, glitch, lifeV, loc>>

\* seed as a forced rerun on the chain without the writer itself; a ghost is not rerun.
H_Seed(p) ==
    /\ pc[p] = "h_seed"
    /\ LET n == loc[p].n  e == loc[p].e  wr == loc[p].wr \ {n}  s == NS(n) IN
       IF n \in ghost
       THEN UNCHANGED <<ent, own, sub, scope, fr>>
       ELSE IF e \in DOMAIN s.ent
       THEN /\ ent' = [ent EXCEPT ![n][e].fo = TRUE, ![n][e].wr = @ \cup wr]
            /\ UNCHANGED <<own, sub, scope, fr>>
       ELSE LET owes == RunOwes(s, e) \/ InhOwes(s, e)
                si == SubInc(sub, scope, e, epPar)
            IN /\ ent' = [ent EXCEPT ![n] = Put(@, e, NewEntry(NoWait, wr, TRUE))]
               /\ own' = [own EXCEPT ![e] = @ + 1]
               /\ sub' = si[1] /\ scope' = si[2]
               /\ fr' = [fr EXCEPT ![p] = IF owes THEN {} ELSE {<<n, c, e>> : c \in Children(n)}]
    /\ pc' = [pc EXCEPT ![p] = "h_frame"]
    /\ UNCHANGED <<timeV, run, inh, queued, nodeX, epOn, epPar, settled, wlog, hooks, nextE>>
    /\ UNCHANGED <<plan, rl, sp, hk, cl, cd, sd, ev, rs, miscV, lifeV, loc>>

\* evaluate: decide every ready entry not yet runnable.
H_Eval(p) ==
    /\ pc[p] = "h_eval"
    /\ LET n == loc[p].n
           r == DecideAll(n, SetToSeq(DOMAIN ent[n]), NS(n), WK(p))
       IN SetNS(n, r[1]) /\ SetWK(p, r[2])
    /\ pc' = [pc EXCEPT ![p] = "h_drain"]
    /\ UNCHANGED <<timeV, epV, fr, plan, cd, sd, ev, miscV, lifeV, loc>>

-----------------------------------------------------------------------------
(* Disposal (lifetime/dispose.rs): the node and every descendant with no   *)
(* live parent outside the freed branch become ghosts, each under its      *)
(* write lock, in NodeId order; each is then retired; the survivors with a *)
(* freed parent rerun, unattributed, in a root epoch of their own.         *)

D_Start(p) ==
    /\ pc[p] = "d_start"
    /\ LET r == loc[p].n
           fs == FreeWalk(Desc(r), {r})
           sv == {c \in Desc(r) \ fs : c \notin ghost /\ Parents[c] \cap fs # {}}
       IN IF r \in ghost
          THEN /\ pc' = [pc EXCEPT ![p] = "done"]
               /\ UNCHANGED loc
          ELSE /\ loc' = [loc EXCEPT ![p].todo = fs, ![p].dl = fs, ![p].rr = sv]
               /\ pc' = [pc EXCEPT ![p] = "d_mark"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, lifeV>>

D_Mark(p) ==
    /\ pc[p] = "d_mark"
    /\ IF loc[p].todo = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "d_retire"]
            /\ UNCHANGED <<ghost, loc>>
       ELSE LET x == Least(loc[p].todo) IN
            /\ ghost' = ghost \cup {x}
            /\ loc' = [loc EXCEPT ![p].todo = @ \ {x}]
            /\ UNCHANGED pc
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, retired, reflowAt, dueAt, broken>>

D_Retire(p) ==
    /\ pc[p] = "d_retire"
    /\ IF loc[p].dl = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "d_rerun"]
            /\ UNCHANGED <<nodeV, rl, sp, hk, cl, rs, retired, loc>>
       ELSE LET x == Least(loc[p].dl)
                r == Retire(x, NS(x), WK(p))
            IN /\ SetNS(x, r[1]) /\ SetWK(p, r[2])
               /\ retired' = retired \cup {x}
               /\ loc' = [loc EXCEPT ![p].dl = @ \ {x}]
               /\ pc' = [pc EXCEPT ![p] = "d_rdrain"]
    /\ UNCHANGED <<timeV, epV, fr, plan, cd, sd, ev, miscV, ghost, reflowAt, dueAt, broken>>

\* record_reruns, delivered at once outside any run: each survivor is seeded as a forced rerun.
D_Rerun(p) ==
    /\ pc[p] = "d_rerun"
    /\ LET rr == {c \in loc[p].rr : c \notin ghost}  e == nextE IN
       IF rr = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "done"]
            /\ UNCHANGED <<clock, dueAt, epOn, own, sub, scope, nextE, cd, sd, ev, anomaly>>
       ELSE IF e > MaxEpochs
       THEN /\ anomaly' = anomaly \cup {"out of epochs or cut-rerun slots"}
            /\ UNCHANGED <<clock, dueAt, epOn, own, sub, scope, nextE, cd, sd, ev, pc>>
       ELSE /\ clock' = clock + 1
            /\ dueAt' = [c \in Node |-> IF c \in rr THEN clock + 1 ELSE dueAt[c]]
            /\ nextE' = e + 1
            /\ epOn' = epOn \cup {e}
            /\ own' = [own EXCEPT ![e] = 1]
            /\ sub' = [sub EXCEPT ![e] = 1]
            /\ scope' = scope + 1
            /\ cd' = [cd EXCEPT ![p][e] = 1]
            /\ sd' = [sd EXCEPT ![p] = {[n |-> c, e |-> e, ca |-> 0, wr |-> {}, k |-> "rerun"] : c \in rr}]
            /\ ev' = [ev EXCEPT ![p] = {<<c, e>> : c \in rr}]
            /\ pc' = [pc EXCEPT ![p] = "d_frame"]
            /\ UNCHANGED anomaly
    /\ UNCHANGED <<version, ranAt, relAt, nodeV, epPar, settled, wlog, hooks, fr, plan, rl, sp, hk, cl, rs>>
    /\ UNCHANGED <<spawned, suppIn, undelivered, hookFree, glitch, ghost, retired, reflowAt, broken, loc>>

-----------------------------------------------------------------------------

ProcNext(p) ==
    \/ FrameStep(p) \/ SeedStep(p) \/ FrameEnd(p) \/ ReleaseStep(p) \/ ResumeStep(p) \/ DrainEnd(p)
    \/ CloseStep(p) \/ CloseEnd(p)
    \/ W_Write(p) \/ W_Deliver(p)
    \/ T_Pick(p) \/ T_Start(p) \/ T_Poll(p) \/ T_Fail(p) \/ T_Snap(p) \/ T_Rec(p) \/ T_Child(p) \/ T_Mark(p)
    \/ T_AfterMark(p) \/ T_Fin(p) \/ T_Resume(p)
    \/ H_Supp(p) \/ H_Seed(p) \/ H_Eval(p)
    \/ D_Start(p) \/ D_Mark(p) \/ D_Retire(p) \/ D_Rerun(p)

Init ==
    /\ clock = 0 /\ version = [n \in Node |-> 0] /\ ranAt = [n \in Node |-> 0] /\ relAt = [n \in Node |-> 0]
    /\ ent = [n \in Node |-> EmptyF] /\ run = [n \in Node |-> NoRun] /\ inh = [n \in Node |-> EmptyF]
    /\ queued = [n \in Node |-> FALSE] /\ wdef = [n \in Node |-> 0] /\ failed = [n \in Node |-> FALSE]
    /\ byPar = [n \in Node |-> FALSE] /\ gated = [n \in Node |-> {}]
    /\ spawned = [n \in Node |-> FALSE] /\ suppIn = [n \in Node |-> {}]
    /\ epOn = {} /\ epPar = [e \in Epoch |-> 0] /\ own = Zero /\ sub = Zero
    /\ settled = [e \in Epoch |-> FALSE] /\ wlog = [e \in Epoch |-> {}] /\ hooks = [e \in Epoch |-> {}]
    /\ scope = 0 /\ nextE = 1 /\ undelivered = {}
    /\ pc = [p \in Procs |-> CASE p[1] = "W" -> "w_apply" [] p[1] = "T" -> "idle"
                               [] p[1] = "D" -> "d_start" [] OTHER -> "free"]
    /\ fr = [p \in Procs |-> {}] /\ plan = [p \in Procs |-> {}] /\ rl = [p \in Procs |-> {}]
    /\ sp = [p \in Procs |-> {}] /\ hk = [p \in Procs |-> {}]
    /\ cl = [p \in Procs |-> Zero] /\ cd = [p \in Procs |-> Zero]
    /\ sd = [p \in Procs |-> {}] /\ ev = [p \in Procs |-> {}] /\ rs = [p \in Procs |-> {}]
    /\ loc = [p \in Procs |-> InitLoc(p)]
    /\ hookFree = 1..MaxHooks /\ anomaly = {} /\ glitch = FALSE
    /\ ghost = {} /\ retired = {} /\ reflowAt = [n \in Node |-> 0] /\ dueAt = [n \in Node |-> 0] /\ broken = {}

Next == \E p \in Procs : ProcNext(p)
Spec == Init /\ [][Next]_vars /\ \A p \in Procs : WF_vars(ProcNext(p))

-----------------------------------------------------------------------------
(* Properties.                                                              *)

NoAnomaly == anomaly = {}

\* Every open token is held by exactly one entry, run, handover or pending close.
Tokens(e) ==
    Cardinality({n \in Node : e \in DOMAIN ent[n]})
    + SumP(Node, [n \in Node |-> IF run[n].on /\ e \in DOMAIN run[n].sv THEN run[n].sv[e].tok ELSE 0])
    + SumP(Node, [n \in Node |-> IF e \in DOMAIN inh[n] THEN inh[n][e].tok ELSE 0])
    + SumP(Procs, [p \in Procs |-> cl[p][e] + cd[p][e]])
TokensConserved == \A e \in Epoch : own[e] = Tokens(e)

\* sub is own plus the child epochs with open work; the scope counts root epochs with open work.
TreeCounts ==
    /\ \A e \in epOn : sub[e] = own[e] + Cardinality({c \in epOn : epPar[c] = e /\ sub[c] > 0})
    /\ scope = Cardinality({e \in epOn : epPar[e] = 0 /\ sub[e] > 0})

\* A child waits only on a parent that still owes it a release, and never for more releases than
\* are under way or owed: an obligation still open releases once, each gathered release once.
NoStuckWait == \A c \in Node : \A e \in DOMAIN ent[c] : \A q \in Node : ent[c][e].w[q] > 0 => OwesTo(q, c, e)
Owed(q, c, e) ==
    SumP(Procs, [p \in Procs |-> Cardinality({m \in rl[p] : m.f = q /\ m.t = c /\ m.e = e})
                                  + (IF \E x \in plan[p] : x.f = q /\ x.e = e THEN 1 ELSE 0)])
    + (IF e \in DOMAIN ent[q] \/ RunOwes(NS(q), e) \/ InhOwes(NS(q), e) THEN 1 ELSE 0)
WaitsCounted == \A c \in Node : \A e \in DOMAIN ent[c] : \A q \in Node : ent[c][e].w[q] <= Owed(q, c, e)

\* An unreleased obligation holds a token, so its epoch cannot settle before the release.
ObligationsHeld ==
    \A n \in Node :
        /\ \A e \in DOMAIN run[n].sv : ~run[n].sv[e].rel => run[n].sv[e].tok >= 1
        /\ \A e \in DOMAIN inh[n] : ~inh[n][e].rel => inh[n][e].tok >= 1
        /\ run[n].on => inh[n] = EmptyF
        /\ spawned[n] => queued[n] \/ n \in ghost

\* When the scope's count is zero (flush returns), every node's last run started after its parents'
\* last changes, except changes not yet delivered, ghosts and nodes their parents fail.
ScopeConsistent ==
    scope = 0 => \A c \in Derived \ ghost : ~byPar[c] => \A q \in Parents[c] :
        version[q] <= ranAt[c] \/ <<q, version[q]>> \in undelivered

\* flush in a writer's scope returns once its root epoch has no open work in its subtree; by then
\* every child of a node it wrote or reflowed has released a run started after it. Cut writers rerun
\* later by design, so this holds without writes from bodies; a node its parents fail does not run.
AbsorbedReleased == NoBodyWrites /\ Resilient =>
    \A e \in epOn : epPar[e] = 0 /\ sub[e] = 0 =>
        \A y \in wlog[e] : Len(y) = 3 => \A c \in Derived \ ghost : y[2] \in Parents[c] => relAt[c] >= y[3]

\* Without writes from bodies, a run never starts on an entry while a parent still owes the epoch; a
\* ghost's value no longer changes, and disposal reruns its survivors.
NoGlitch == NoBodyWrites => ~glitch

\* A whole node's run, started below the bound, leaves no entry of it waiting behind.
WholeTogether == "whole" \notin broken

\* A created node's run never starts while its creator may still replace or keep it.
CreatorFirst == "creator" \notin broken

\* A run failed, or a disposal made ghosts, and has not yet opened the epochs that tell the children.
FailUndelivered ==
    \E p \in Procs :
        \/ p[1] = "T" /\ loc[p].out = "panic" /\ pc[p] \in {"t_fail", "t_snap", "t_child"}
        \/ p[1] = "D" /\ pc[p] \in {"d_mark", "d_retire", "d_rdrain", "d_rclose", "d_rerun"}

\* When the scope's count is zero, a node is failed through its parents exactly when its resilience
\* gives way to their failures, except while a failure is not yet delivered.
ResilienceConsistent == scope = 0 /\ ~FailUndelivered => \A c \in Derived \ ghost : byPar[c] <=> GivesWay(c)

\* When the scope's count is zero, every reflowed node has run since its last reflow.
ReflowRan == scope = 0 => \A n \in Derived \ ghost : ~byPar[n] =>
    reflowAt[n] <= ranAt[n] \/ <<n, reflowAt[n]>> \in undelivered

\* A run holds its epochs open until a poll that does not yield: flush waits for a generator's items.
YieldsHeld == \A p \in Procs : p[1] = "T" /\ loc[p].live /\ loc[p].held /\ loc[p].n \notin ghost => scope > 0

\* A retired ghost never has an entry, a handover, a run or a queued run again.
GhostsRetired == \A n \in retired : ent[n] = EmptyF /\ inh[n] = EmptyF /\ ~run[n].on /\ ~queued[n]

\* When the scope's count is zero, every survivor of a disposal has run since.
SurvivorsRerun == scope = 0 => \A c \in Derived \ ghost : ~byPar[c] => dueAt[c] <= ranAt[c]

Quiet ==
    /\ \A p \in Procs : pc[p] \in {"idle", "done", "free"}
    /\ \A n \in Node : ~run[n].on /\ ~queued[n] /\ ent[n] = EmptyF /\ inh[n] = EmptyF
Terminates == <>Quiet
=============================================================================
