\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---------------------------- MODULE Scheduling ----------------------------
(***************************************************************************)
(* lithia's scheduling protocol (design spec 7.2-7.4, 11.4) at the grain   *)
(* of the engine's lock sections: framing, releases, deciding and settling *)
(* entries, runs with supersession, handover and protection, writer sets   *)
(* with cuts and cut reruns, and the epoch tree's accounting.              *)
(*                                                                         *)
(* Processes are outside writers (one batch each), run tasks (Slots per    *)
(* node) and cut reruns. Each step below is one lock section or one atomic *)
(* operation of crates/lithia/src/protocol/, run/ and graph/epoch.rs; a    *)
(* process's pending frames and releases are sets, so they are handled in  *)
(* every order, a superset of the engine's.                                *)
(*                                                                         *)
(* Left out: node creation, lifetimes (generations, support, sweeps,       *)
(* leases), creator order, fragility, reflow, generators, WHOLE_EPOCHS and *)
(* disposal. Values are abstract: a body changes its value or not, and     *)
(* writes any of its targets, chosen freely while the clock allows.        *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    N,          \* nodes are 1..N, numbered in NodeId (topological) order
    Parents,    \* [1..N -> SUBSET 1..N]: every parent is older than its child
    Sources,    \* nodes without a body, written from outside or by bodies
    Async,      \* derived nodes whose runs may suspend and poll again
    Changing,   \* derived nodes whose runs may change their value; the others always compute an equal one
    Targets,    \* [1..N -> SUBSET Sources]: the sources a node's body may write
    Batches,    \* <<B1, B2, ...>>: one outside writer per batch, a set of sources
    MaxClock,   \* bodies change values only while the clock stays within this
    MaxEpochs,  \* epoch ids available; running out is flagged
    MaxPolls,   \* polls per run, at most
    Slots,      \* run tasks per node alive at once
    MaxHooks    \* cut reruns available; running out is flagged

Node == 1..N
Derived == Node \ Sources
Children(n) == {c \in Node : n \in Parents[c]}
Epoch == 1..MaxEpochs
Procs == {<<"W", i>> : i \in 1..Len(Batches)}
            \cup {<<"T", n, k>> : n \in Derived, k \in 1..Slots}
            \cup {<<"H", j>> : j \in 1..MaxHooks}
NoBodyWrites == \A n \in Node : Targets[n] = {}

Max(a, b) == IF a >= b THEN a ELSE b
EmptyF == [x \in {} |-> 0]
Put(f, k, v) == [x \in DOMAIN f \cup {k} |-> IF x = k THEN v ELSE f[x]]
Zero == [e \in Epoch |-> 0]
SetToSeq(S) == CHOOSE f \in [1..Cardinality(S) -> S] : \A i, j \in DOMAIN f : i # j => f[i] # f[j]
RECURSIVE SumP(_, _)
SumP(S, f) == IF S = {} THEN 0 ELSE LET x == CHOOSE y \in S : TRUE IN f[x] + SumP(S \ {x}, f)

\* A run in flight: the epochs it serves (sv), superseded (restart), protected (replaced),
\* its cancel flag (cx) and the nodes a cut rerun leaves alone (sup).
NoRun == [on |-> FALSE, sv |-> EmptyF, restart |-> FALSE, replaced |-> FALSE, cx |-> FALSE, sup |-> {}]
\* An entry: releases each parent still owes it (w), writer set, newest change, carry, forced, cut,
\* runnable. A parent framed again in an epoch before its earlier release arrived owes two.
NoWait == [x \in Node |-> 0]
Ready(w) == \A x \in Node : w[x] = 0
NewEntry(w, wr, fo) == [w |-> w, wr |-> wr, nw |-> 0, ca |-> 0, fo |-> fo, cut |-> FALSE, rn |-> FALSE]
InitLoc(p) == [n |-> IF p[1] = "T" THEN p[2] ELSE 0, todo |-> IF p[1] = "W" THEN Batches[p[2]] ELSE {},
               ws |-> {}, own |-> 0, out |-> "end", polls |-> 0, first |-> TRUE, live |-> FALSE,
               snap |-> {}, rel |-> {}, cx |-> FALSE, done |-> FALSE, e |-> 0, par |-> 0, wr |-> {}]

VARIABLES
    clock, version, ranAt, relAt,
    ent, run, inh, queued, spawned, suppIn,
    epOn, epPar, own, sub, settled, wlog, hooks, scope, nextE,
    undelivered,
    pc, fr, plan, rl, sp, hk, cl, cd, loc,
    hookFree, anomaly, glitch

timeV == <<clock, version, ranAt, relAt>>
nodeV == <<ent, run, inh, queued>>
epV == <<epOn, epPar, own, sub, settled, wlog, hooks, scope, nextE>>
workV == <<fr, plan, rl, sp, hk, cl, cd>>
miscV == <<spawned, suppIn, undelivered, hookFree, anomaly, glitch>>
vars == <<timeV, nodeV, epV, workV, miscV, pc, loc>>

-----------------------------------------------------------------------------
(* Node and work records, so Decide and Settle can be composed in one step. *)

NS(n) == [ent |-> ent[n], run |-> run[n], inh |-> inh[n], q |-> queued[n]]
WK(p) == [rl |-> rl[p], sp |-> sp[p], hk |-> hk[p], cl |-> cl[p]]
SetNS(n, s) ==
    /\ ent' = [ent EXCEPT ![n] = s.ent] /\ run' = [run EXCEPT ![n] = s.run]
    /\ inh' = [inh EXCEPT ![n] = s.inh] /\ queued' = [queued EXCEPT ![n] = s.q]
SetWK(p, w) ==
    /\ rl' = [rl EXCEPT ![p] = w.rl] /\ sp' = [sp EXCEPT ![p] = w.sp]
    /\ hk' = [hk EXCEPT ![p] = w.hk] /\ cl' = [cl EXCEPT ![p] = w.cl]

RelTo(n, e, ca, wr) == {[f |-> n, t |-> c, e |-> e, ca |-> ca, wr |-> wr] : c \in Children(n)}
RunOwes(s, e) == s.run.on /\ e \in DOMAIN s.run.sv /\ ~s.run.sv[e].rel
InhOwes(s, e) == e \in DOMAIN s.inh /\ ~s.inh[e].rel

\* settle (protocol/propagate.rs): close the entry; release the children unless a run still owes them.
Settle(n, e, s, w) ==
    LET en == s.ent[e]
        Merge(r) == [r EXCEPT !.ca = Max(@, en.ca), !.wr = @ \cup en.wr]
        s1 == [s EXCEPT !.ent = [x \in DOMAIN s.ent \ {e} |-> s.ent[x]]]
        s2 == IF RunOwes(s, e) THEN [s1 EXCEPT !.run.sv[e] = Merge(@)]
              ELSE IF InhOwes(s, e) THEN [s1 EXCEPT !.inh[e] = Merge(@)]
              ELSE s1
        w1 == IF RunOwes(s, e) \/ InhOwes(s, e) THEN w
              ELSE [w EXCEPT !.rl = @ \cup RelTo(n, e, en.ca, en.wr)]
    IN <<s2, [w1 EXCEPT !.cl[e] = @ + 1]>>

\* join_run (protocol/propagate.rs): the entry's epoch joins the run in flight, which releases it with its
\* first item or when it ends; the entry's token goes with it.
Join(n, e, s, w) ==
    LET en == s.ent[e]
        s1 == [s EXCEPT !.ent = [x \in DOMAIN s.ent \ {e} |-> s.ent[x]]]
        rec == IF e \in DOMAIN s.run.sv
               THEN [s.run.sv[e] EXCEPT !.tok = @ + 1, !.rel = FALSE, !.wr = @ \cup en.wr, !.ca = Max(@, en.ca)]
               ELSE [tok |-> 1, rel |-> FALSE, wr |-> en.wr, ca |-> en.ca]
    IN <<[s1 EXCEPT !.run.sv = Put(@, e, rec)], w>>

\* A run in flight that has not released yet: its first poll is still to be marked.
Unreleased(s) == s.run.on /\ \E f \in DOMAIN s.run.sv : ~s.run.sv[f].rel

\* decide (protocol/propagate.rs): a ready entry is cut, settles unchanged, starts a run or supersedes one. An
\* unchanged entry that carries a change absorbed by a run started after it joins that run; once the
\* run has released, the entry settles with the node's version, so its children test the change.
Decide(n, e, s, w) ==
    LET en == s.ent[e] IN
    IF en.cut
    THEN Settle(n, e, s, [w EXCEPT !.hk = @ \cup {[n |-> n, e |-> e, wr |-> en.wr]}])
    ELSE IF ~(en.fo \/ en.nw > ranAt[n])
    THEN IF en.nw > 0 /\ Unreleased(s)
         THEN Join(n, e, s, w)
         ELSE IF en.nw > 0
         THEN Settle(n, e, [s EXCEPT !.ent[e].ca = Max(@, version[n])], w)
         ELSE Settle(n, e, s, w)
    ELSE LET s1 == [s EXCEPT !.ent[e].rn = TRUE] IN
         IF ~s1.run.on
         THEN IF s1.q THEN <<s1, w>> ELSE <<[s1 EXCEPT !.q = TRUE], [w EXCEPT !.sp = @ \cup {n}]>>
         ELSE IF ~s1.run.restart /\ ~s1.run.replaced
         THEN <<[s1 EXCEPT !.run.restart = TRUE, !.run.cx = TRUE], w>>
         ELSE <<s1, w>>

RECURSIVE DecideAll(_, _, _, _)
DecideAll(n, es, s, w) ==
    IF es = <<>> THEN <<s, w>>
    ELSE LET e == Head(es)
             r == IF e \in DOMAIN s.ent /\ Ready(s.ent[e].w) /\ ~s.ent[e].rn THEN Decide(n, e, s, w) ELSE <<s, w>>
         IN DecideAll(n, Tail(es), r[1], r[2])

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

-----------------------------------------------------------------------------
(* Generic steps: framing, releasing, the end of a drain, closing tokens.  *)

FramePC == {"w_frame", "t_frame", "h_frame"}
DrainPC == {"w_drain", "t_drain", "t_mdrain", "t_fdrain", "h_drain"}
ClosePC == {"w_close", "t_close", "t_end", "h_close"}

\* frame (protocol/propagate.rs), one child under its lock.
FrameStep(p) ==
    /\ pc[p] \in FramePC
    /\ \E x \in fr[p] :
        LET a == x[1]  c == x[2]  e == x[3] IN
        IF e \in DOMAIN ent[c]
        THEN /\ ent' = [ent EXCEPT ![c][e].w[a] = @ + 1, ![c][e].rn = FALSE]
             /\ fr' = [fr EXCEPT ![p] = @ \ {x}]
             /\ UNCHANGED <<own, sub, scope>>
        ELSE LET owes == RunOwes(NS(c), e) \/ InhOwes(NS(c), e)
                 si == SubInc(sub, scope, e, epPar)
             IN /\ ent' = [ent EXCEPT ![c] = Put(@, e, NewEntry([NoWait EXCEPT ![a] = 1], {}, FALSE))]
                /\ own' = [own EXCEPT ![e] = @ + 1]
                /\ sub' = si[1] /\ scope' = si[2]
                /\ fr' = [fr EXCEPT ![p] = (@ \ {x}) \cup (IF owes THEN {} ELSE {<<c, g, e>> : g \in Children(c)})]
    /\ UNCHANGED <<timeV, run, inh, queued, epOn, epPar, settled, wlog, hooks, nextE>>
    /\ UNCHANGED <<plan, rl, sp, hk, cl, cd, miscV, pc, loc>>

\* The whole region is framed: the delivery's own releases start.
FrameEnd(p) ==
    /\ pc[p] \in FramePC /\ fr[p] = {}
    /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(x.f, x.e, x.ca, x.wr) : x \in plan[p]})]
    /\ plan' = [plan EXCEPT ![p] = {}]
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "w_frame" -> "w_drain"
                                 [] pc[p] = "t_frame" -> "t_drain"
                                 [] OTHER -> "h_eval"]
    /\ UNCHANGED <<timeV, nodeV, epV, fr, sp, hk, cl, cd, miscV, loc>>

\* release_children (protocol/propagate.rs), one child under its lock.
ReleaseStep(p) ==
    /\ pc[p] \in DrainPC
    /\ \E m \in rl[p] :
        LET c == m.t  e == m.e  w0 == [WK(p) EXCEPT !.rl = @ \ {m}] IN
        IF e \in DOMAIN ent[c] /\ ent[c][e].w[m.f] > 0
        THEN LET en == ent[c][e]
                 wr2 == en.wr \cup m.wr
                 en2 == [en EXCEPT !.w[m.f] = @ - 1, !.wr = wr2, !.nw = Max(@, m.ca), !.cut = @ \/ c \in wr2]
                 s2 == [NS(c) EXCEPT !.ent[e] = en2]
                 r == IF Ready(en2.w) THEN Decide(c, e, s2, w0) ELSE <<s2, w0>>
             IN SetNS(c, r[1]) /\ SetWK(p, r[2])
        ELSE /\ rl' = [rl EXCEPT ![p] = @ \ {m}]
             /\ UNCHANGED <<nodeV, sp, hk, cl>>
    /\ UNCHANGED <<timeV, epV, fr, plan, cd, miscV, pc, loc>>

\* The end of drain_deferred: cut reruns wait for their epoch, spawned tasks become runnable.
DrainEnd(p) ==
    /\ pc[p] \in DrainPC /\ rl[p] = {}
    /\ hooks' = [e \in Epoch |-> hooks[e] \cup {[n |-> h.n, wr |-> h.wr] : h \in {h2 \in hk[p] : h2.e = e}}]
    /\ anomaly' = IF \E h \in hk[p] : own[h.e] = 0 THEN anomaly \cup {"cut rerun waits on a settled epoch"} ELSE anomaly
    /\ spawned' = [n \in Node |-> spawned[n] \/ n \in sp[p]]
    /\ hk' = [hk EXCEPT ![p] = {}]
    /\ sp' = [sp EXCEPT ![p] = {}]
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "w_drain" -> "w_close"
                                 [] pc[p] = "t_drain" -> "t_close"
                                 [] pc[p] = "t_mdrain" -> "t_aftermark"
                                 [] pc[p] = "t_fdrain" -> "t_end"
                                 [] OTHER -> "h_close"]
    /\ UNCHANGED <<timeV, nodeV, epOn, epPar, own, sub, settled, wlog, scope, nextE>>
    /\ UNCHANGED <<fr, plan, rl, cl, cd, suppIn, undelivered, hookFree, glitch, loc>>

\* Tokens a close step may close: a writer and a cut rerun close their drain's tokens before
\* their guard; deliver_into closes its drain's at once; a run task closes the rest at its poll's end.
Pending(p, e) ==
    CASE pc[p] \in {"w_close", "h_close"} -> IF cl[p] # Zero THEN cl[p][e] ELSE cd[p][e]
      [] pc[p] = "t_close" -> cl[p][e]
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
            sd == SubDec(sub2, scope, e, k, par2)
            ClearCl(q) == pc[p] = "t_close" \/ pc[p] = "t_end" \/ (pc[p] \in {"w_close", "h_close"} /\ cl[p] # Zero)
            ClearCd(q) == pc[p] = "t_end" \/ (pc[p] \in {"w_close", "h_close"} /\ cl[p] = Zero)
        IN /\ k > 0
           /\ own' = [x \in Epoch |-> IF x = e THEN o ELSE IF New(x) THEN 1 ELSE own[x]]
           /\ sub' = sd[1] /\ scope' = sd[2]
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
    /\ UNCHANGED <<timeV, nodeV, wlog, fr, plan, rl, sp, hk, spawned, suppIn, undelivered, glitch>>

CloseEnd(p) ==
    /\ pc[p] \in ClosePC
    /\ \A e \in Epoch : Pending(p, e) = 0
    /\ pc[p] \in {"w_close", "h_close"} => cd[p] = Zero
    /\ pc' = [pc EXCEPT ![p] = CASE pc[p] = "t_close" -> "t_mark"
                                 [] pc[p] = "t_end" -> IF loc[p].live THEN "t_susp" ELSE "idle"
                                 [] OTHER -> "done"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, loc>>

-----------------------------------------------------------------------------
(* Outside writers: write the batch, then deliver it as a root epoch.      *)

W_Write(p) ==
    /\ pc[p] = "w_apply"
    /\ \E s \in loc[p].todo :
        /\ clock' = clock + 1
        /\ version' = [version EXCEPT ![s] = clock + 1]
        /\ undelivered' = undelivered \cup {<<s, clock + 1>>}
        /\ loc' = [loc EXCEPT ![p].todo = @ \ {s}, ![p].ws = @ \cup {<<s, clock + 1>>}]
    /\ UNCHANGED <<ranAt, relAt, nodeV, epV, workV, spawned, suppIn, hookFree, anomaly, glitch, pc>>

W_Deliver(p) ==
    /\ pc[p] = "w_apply" /\ loc[p].todo = {}
    /\ LET e == nextE IN
       IF e > MaxEpochs
       THEN /\ anomaly' = anomaly \cup {"out of epochs or cut-rerun slots"}
            /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, suppIn, undelivered, hookFree, glitch, pc, loc>>
       ELSE /\ nextE' = e + 1
            /\ epOn' = epOn \cup {e}
            /\ own' = [own EXCEPT ![e] = 1]
            /\ sub' = [sub EXCEPT ![e] = 1]
            /\ scope' = scope + 1
            /\ cd' = [cd EXCEPT ![p][e] = 1]
            /\ wlog' = [wlog EXCEPT ![e] = {<<0, x[1], x[2]>> : x \in loc[p].ws}]
            /\ fr' = [fr EXCEPT ![p] = UNION {{<<x[1], c, e>> : c \in Children(x[1])} : x \in loc[p].ws}]
            /\ plan' = [plan EXCEPT ![p] = {[f |-> x[1], e |-> e, ca |-> x[2], wr |-> {}] : x \in loc[p].ws}]
            /\ undelivered' = undelivered \ loc[p].ws
            /\ pc' = [pc EXCEPT ![p] = "w_frame"]
            /\ UNCHANGED <<timeV, nodeV, epPar, settled, hooks, rl, sp, hk, cl>>
            /\ UNCHANGED <<spawned, suppIn, hookFree, anomaly, glitch, loc>>

-----------------------------------------------------------------------------
(* Run tasks (run/future.rs RunFuture; protocol/lifecycle.rs start_run,    *)
(* after_poll, finalize).                                                  *)

\* q still owes c a release in e: an entry, a run or handover holding e, or one under way.
OwesTo(q, c, e) ==
    \/ e \in DOMAIN ent[q] \/ RunOwes(NS(q), e) \/ InhOwes(NS(q), e)
    \/ \E p \in Procs : (\E m \in rl[p] : m.f = q /\ m.t = c /\ m.e = e) \/ (\E x \in plan[p] : x.f = q /\ x.e = e)

T_Pick(p) ==
    /\ pc[p] = "idle" /\ spawned[loc[p].n]
    /\ spawned' = [spawned EXCEPT ![loc[p].n] = FALSE]
    /\ pc' = [pc EXCEPT ![p] = "t_start"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, suppIn, undelivered, hookFree, anomaly, glitch, loc>>

T_Start(p) ==
    /\ pc[p] = "t_start"
    /\ LET n == loc[p].n
           s == NS(n)
           take == {e \in DOMAIN s.ent : s.ent[e].rn /\ Ready(s.ent[e].w)}
           Rec(e) == IF e \in DOMAIN s.inh /\ e \in take
                     THEN [tok |-> s.inh[e].tok + 1, rel |-> FALSE, wr |-> s.inh[e].wr \cup s.ent[e].wr,
                           ca |-> Max(s.inh[e].ca, s.ent[e].ca)]
                     ELSE IF e \in take THEN [tok |-> 1, rel |-> FALSE, wr |-> s.ent[e].wr, ca |-> s.ent[e].ca]
                     ELSE s.inh[e]
           served == [e \in DOMAIN s.inh \cup take |-> Rec(e)]
       IN /\ queued' = [queued EXCEPT ![n] = FALSE]
          /\ IF ~s.run.on /\ DOMAIN served # {}
             THEN /\ ent' = [ent EXCEPT ![n] = [e \in DOMAIN s.ent \ take |-> s.ent[e]]]
                  /\ inh' = [inh EXCEPT ![n] = EmptyF]
                  /\ run' = [run EXCEPT ![n] = [on |-> TRUE, sv |-> served, restart |-> FALSE,
                                                replaced |-> DOMAIN s.inh # {}, cx |-> FALSE,
                                                sup |-> WrittenBy(suppIn[n], n)]]
                  /\ suppIn' = [suppIn EXCEPT ![n] = {}]
                  /\ ranAt' = [ranAt EXCEPT ![n] = clock]
                  /\ glitch' = (glitch \/ \E e \in take, q \in Parents[n] : OwesTo(q, n, e))
                  /\ loc' = [loc EXCEPT ![p].first = TRUE, ![p].polls = 0, ![p].live = TRUE]
                  /\ pc' = [pc EXCEPT ![p] = "t_poll"]
             ELSE /\ pc' = [pc EXCEPT ![p] = "idle"]
                  /\ UNCHANGED <<ent, inh, run, suppIn, ranAt, glitch, loc>>
    /\ UNCHANGED <<clock, version, relAt, epV, workV, spawned, undelivered, hookFree, anomaly>>

\* One poll of the body: it may write its targets and, when it ends, publish a changed value,
\* which is discarded if the run was superseded by then (run/future.rs publish). A node without
\* children never changes: nothing could observe it, so the model saves those states; nor does a
\* node outside Changing.
T_Poll(p) ==
    /\ pc[p] = "t_poll"
    /\ LET n == loc[p].n  r == run[n] IN
       \E chg \in BOOLEAN, wr \in SUBSET (Targets[n] \ r.sup),
          out \in IF n \in Async /\ loc[p].polls + 1 < MaxPolls THEN {"end", "susp"} ELSE {"end"} :
         LET pub == chg /\ out = "end" /\ ~r.cx
             k == Cardinality(wr) + (IF pub THEN 1 ELSE 0)
             St(t) == clock + Cardinality({u \in wr : u <= t})
             mine == IF pub THEN clock + k ELSE 0
         IN /\ chg => out = "end" /\ Children(n) # {} /\ n \in Changing
            /\ k = 0 \/ clock + k <= MaxClock
            /\ clock' = clock + k
            /\ version' = [x \in Node |-> IF x \in wr THEN St(x) ELSE IF x = n /\ pub THEN mine ELSE version[x]]
            /\ undelivered' = undelivered \cup {<<t, St(t)>> : t \in wr}
                                \cup (IF pub /\ ~loc[p].first THEN {<<n, mine>>} ELSE {})
            /\ loc' = [loc EXCEPT ![p].ws = {<<t, St(t)>> : t \in wr}, ![p].own = mine,
                                  ![p].out = out, ![p].polls = @ + 1]
            /\ pc' = [pc EXCEPT ![p] = "t_snap"]
    /\ UNCHANGED <<ranAt, relAt, nodeV, epV, workV, spawned, suppIn, hookFree, anomaly, glitch>>

\* after_poll's snapshot of the run under its node's lock.
T_Snap(p) ==
    /\ pc[p] = "t_snap"
    /\ LET r == run[loc[p].n] IN
       /\ loc' = [loc EXCEPT ![p].snap = {<<e, r.sv[e].wr>> : e \in DOMAIN r.sv},
                             ![p].rel = {<<e, r.sv[e].wr>> : e \in {x \in DOMAIN r.sv : r.sv[x].rel}},
                             ![p].cx = r.restart \/ r.cx, ![p].done = FALSE]
       /\ anomaly' = IF r.on THEN anomaly ELSE anomaly \cup {"poll without a run"}
    /\ pc' = [pc EXCEPT ![p] = IF loc[p].first THEN "t_rec" ELSE "t_child"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, suppIn, undelivered, hookFree, glitch>>

\* deliver_into for the first poll: writes go to the served epochs, routed with the writer added.
T_Rec(p) ==
    /\ pc[p] = "t_rec"
    /\ LET n == loc[p].n  ws == loc[p].ws  snap == loc[p].snap IN
       IF ws = {}
       THEN /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<wlog, fr, plan, undelivered>>
       ELSE /\ wlog' = [e \in Epoch |-> IF \E y \in snap : y[1] = e THEN wlog[e] \cup {<<n, x[1]>> : x \in ws} ELSE wlog[e]]
            /\ fr' = [fr EXCEPT ![p] = UNION {{<<x[1], c, y[1]>> : c \in Children(x[1])} : x \in ws, y \in snap}]
            /\ plan' = [plan EXCEPT ![p] = {[f |-> x[1], e |-> y[1], ca |-> x[2], wr |-> y[2] \cup {n}] : x \in ws, y \in snap}]
            /\ undelivered' = undelivered \ ws
            /\ pc' = [pc EXCEPT ![p] = "t_frame"]
    /\ UNCHANGED <<timeV, nodeV, epOn, epPar, own, sub, settled, hooks, scope, nextE>>
    /\ UNCHANGED <<rl, sp, hk, cl, cd, spawned, suppIn, hookFree, anomaly, glitch, loc>>

\* deliver_children: one child epoch per epoch in `es` (<<epoch, writers>> pairs), carrying the
\* writes `ws` and, with a stamp, the node's own item.
ChildDel(p, es, ws, stamp) ==
    LET n == loc[p].n
        S == SetToSeq(es)
        cnt == Len(S)
        New(x) == x >= nextE /\ x < nextE + cnt
        Of(x) == S[x - nextE + 1]
        kids == [i \in 1..cnt |-> nextE + i - 1]
        si == SubIncAll(sub, scope, [i \in 1..cnt |-> S[i][1]], epPar)
    IN IF (ws = {} /\ stamp = 0) \/ cnt = 0
       THEN /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<epV, fr, plan, cd, anomaly>>
       ELSE IF nextE + cnt - 1 > MaxEpochs
       THEN /\ anomaly' = anomaly \cup {"out of epochs or cut-rerun slots"}
            /\ pc' = [pc EXCEPT ![p] = "t_mark"]
            /\ UNCHANGED <<epV, fr, plan, cd>>
       ELSE /\ nextE' = nextE + cnt
            /\ epOn' = epOn \cup {x \in Epoch : New(x)}
            /\ epPar' = [x \in Epoch |-> IF New(x) THEN Of(x)[1] ELSE epPar[x]]
            /\ own' = [x \in Epoch |-> IF New(x) THEN 1 ELSE own[x]]
            /\ sub' = [x \in Epoch |-> IF New(x) THEN 1 ELSE si[1][x]]
            /\ scope' = si[2]
            /\ cd' = [cd EXCEPT ![p] = [x \in Epoch |-> IF New(x) THEN @[x] + 1 ELSE @[x]]]
            /\ wlog' = [x \in Epoch |-> IF New(x) THEN {<<n, y[1]>> : y \in ws} ELSE wlog[x]]
            /\ fr' = [fr EXCEPT ![p] =
                  (UNION {{<<y[1], c, kids[i]>> : c \in Children(y[1])} : y \in ws, i \in 1..cnt})
                  \cup (IF stamp > 0 THEN {<<n, c, kids[i]>> : c \in Children(n), i \in 1..cnt} ELSE {})]
            /\ plan' = [plan EXCEPT ![p] =
                  {[f |-> y[1], e |-> kids[i], ca |-> y[2], wr |-> S[i][2] \cup {n}] : y \in ws, i \in 1..cnt}
                  \cup (IF stamp > 0 THEN {[f |-> n, e |-> kids[i], ca |-> stamp, wr |-> S[i][2]] : i \in 1..cnt} ELSE {})]
            /\ pc' = [pc EXCEPT ![p] = "t_frame"]
            /\ UNCHANGED <<settled, hooks, anomaly>>

\* A later poll's batch and item, delivered even if the run was superseded after it published.
T_Child(p) ==
    /\ pc[p] = "t_child"
    /\ undelivered' = undelivered \ (loc[p].ws \cup {<<loc[p].n, loc[p].own>>})
    /\ ChildDel(p, loc[p].snap, loc[p].ws, loc[p].own)
    /\ UNCHANGED <<timeV, nodeV, rl, sp, hk, cl, spawned, suppIn, hookFree, glitch, loc>>

\* after_poll once the batch is delivered: a first item reaches the epochs released already
\* through child epochs; a superseded run hands its item's stamp to the run replacing it;
\* otherwise, under the node's lock, the first poll releases the children (or hands the release to
\* an entry that took it over), and every poll that does not yield closes the run's tokens.
T_Mark(p) ==
    /\ pc[p] = "t_mark"
    /\ LET n == loc[p].n  s == NS(n)  sv == s.run.sv  it == loc[p].own  first == loc[p].first IN
       IF first /\ it > 0 /\ loc[p].rel # {} /\ ~loc[p].done
       THEN /\ ChildDel(p, loc[p].rel, {}, it)
            /\ loc' = [loc EXCEPT ![p].done = TRUE]
            /\ UNCHANGED <<timeV, nodeV, rl, sp, hk, cl, miscV>>
       ELSE IF loc[p].cx
       THEN /\ run' = [run EXCEPT ![n].sv = [e \in DOMAIN sv |->
                     IF first /\ it > 0 /\ ~sv[e].rel THEN [sv[e] EXCEPT !.ca = Max(@, it)] ELSE sv[e]]]
            /\ pc' = [pc EXCEPT ![p] = "t_fin"]
            /\ UNCHANGED <<timeV, ent, inh, queued, epV, workV, miscV, loc>>
       ELSE LET Mk(e) == first /\ ~sv[e].rel
                sv2 == [e \in DOMAIN sv |-> [sv[e] EXCEPT !.rel = @ \/ Mk(e),
                                                          !.ca = IF Mk(e) THEN Max(@, it) ELSE @, !.tok = 0]]
                toEnt == {e \in DOMAIN sv : Mk(e) /\ e \in DOMAIN s.ent}
                toRel == {e \in DOMAIN sv : Mk(e) /\ e \notin DOMAIN s.ent}
            IN /\ ent' = [ent EXCEPT ![n] = [e \in DOMAIN s.ent |->
                              IF e \in toEnt THEN [s.ent[e] EXCEPT !.ca = Max(@, sv2[e].ca), !.wr = @ \cup sv[e].wr]
                              ELSE s.ent[e]]]
               /\ run' = [run EXCEPT ![n].sv = sv2]
               /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(n, e, sv2[e].ca, sv[e].wr) : e \in toRel})]
               /\ cl' = [cl EXCEPT ![p] = [e \in Epoch |-> @[e] + (IF e \in DOMAIN sv THEN sv[e].tok ELSE 0)]]
               /\ relAt' = IF first THEN [relAt EXCEPT ![n] = ranAt[n]] ELSE relAt
               /\ pc' = [pc EXCEPT ![p] = "t_mdrain"]
               /\ UNCHANGED <<clock, version, ranAt, inh, queued, epV, fr, plan, sp, hk, cd, miscV, loc>>

\* poll_run after after_poll: an ended run finalizes; a suspended one finalizes if superseded.
T_AfterMark(p) ==
    /\ pc[p] = "t_aftermark"
    /\ pc' = [pc EXCEPT ![p] = IF loc[p].out = "end" \/ run[loc[p].n].cx THEN "t_fin" ELSE "t_end"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV, loc>>

\* finalize: a superseded run hands its epochs over; otherwise it releases what it still owes,
\* closes its tokens, and starts a follow-up run for entries that waited for it.
T_Fin(p) ==
    /\ pc[p] = "t_fin"
    /\ LET n == loc[p].n  s == NS(n)  r == s.run IN
       /\ run' = [run EXCEPT ![n] = NoRun]
       /\ IF r.restart
          THEN /\ inh' = [inh EXCEPT ![n] = r.sv]
               /\ queued' = [queued EXCEPT ![n] = TRUE]
               /\ sp' = [sp EXCEPT ![p] = IF s.q THEN @ ELSE @ \cup {n}]
               /\ UNCHANGED <<ent, rl, cl>>
          ELSE LET unrel == {e \in DOMAIN r.sv : ~r.sv[e].rel}
                   ent2 == [e \in DOMAIN s.ent |->
                              IF e \in unrel THEN [s.ent[e] EXCEPT !.ca = Max(@, r.sv[e].ca), !.wr = @ \cup r.sv[e].wr]
                              ELSE s.ent[e]]
                   follow == \E e \in DOMAIN ent2 : ent2[e].rn /\ Ready(ent2[e].w)
               IN /\ ent' = [ent EXCEPT ![n] = ent2]
                  /\ rl' = [rl EXCEPT ![p] = @ \cup (UNION {RelTo(n, e, r.sv[e].ca, r.sv[e].wr) : e \in unrel \ DOMAIN s.ent})]
                  /\ cl' = [cl EXCEPT ![p] = [e \in Epoch |-> @[e] + (IF e \in DOMAIN r.sv THEN r.sv[e].tok ELSE 0)]]
                  /\ queued' = [queued EXCEPT ![n] = s.q \/ follow]
                  /\ sp' = [sp EXCEPT ![p] = IF follow /\ ~s.q THEN @ \cup {n} ELSE @]
                  /\ UNCHANGED inh
    /\ loc' = [loc EXCEPT ![p].live = FALSE]
    /\ pc' = [pc EXCEPT ![p] = "t_fdrain"]
    /\ UNCHANGED <<timeV, epV, fr, plan, hk, cd, miscV>>

\* A suspended run is woken: superseded, it finalizes; otherwise its body is polled again.
T_Resume(p) ==
    /\ pc[p] = "t_susp"
    /\ pc' = [pc EXCEPT ![p] = IF run[loc[p].n].cx THEN "t_fin" ELSE "t_poll"]
    /\ loc' = [loc EXCEPT ![p].first = FALSE]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, miscV>>

-----------------------------------------------------------------------------
(* Cut reruns (protocol/propagate.rs rerun_cut), when their epoch settles. *)

H_Supp(p) ==
    /\ pc[p] = "h_supp"
    /\ suppIn' = [suppIn EXCEPT ![loc[p].n] = @ \cup {loc[p].par}]
    /\ pc' = [pc EXCEPT ![p] = "h_seed"]
    /\ UNCHANGED <<timeV, nodeV, epV, workV, spawned, undelivered, hookFree, anomaly, glitch, loc>>

\* seed as a forced rerun on the chain without the writer itself.
H_Seed(p) ==
    /\ pc[p] = "h_seed"
    /\ LET n == loc[p].n  e == loc[p].e  wr == loc[p].wr \ {n}  s == NS(n) IN
       IF e \in DOMAIN s.ent
       THEN /\ ent' = [ent EXCEPT ![n][e].fo = TRUE, ![n][e].wr = @ \cup wr]
            /\ UNCHANGED <<own, sub, scope, fr>>
       ELSE LET owes == RunOwes(s, e) \/ InhOwes(s, e)
                si == SubInc(sub, scope, e, epPar)
            IN /\ ent' = [ent EXCEPT ![n] = Put(@, e, NewEntry(NoWait, wr, TRUE))]
               /\ own' = [own EXCEPT ![e] = @ + 1]
               /\ sub' = si[1] /\ scope' = si[2]
               /\ fr' = [fr EXCEPT ![p] = IF owes THEN {} ELSE {<<n, c, e>> : c \in Children(n)}]
    /\ pc' = [pc EXCEPT ![p] = "h_frame"]
    /\ UNCHANGED <<timeV, run, inh, queued, epOn, epPar, settled, wlog, hooks, nextE>>
    /\ UNCHANGED <<plan, rl, sp, hk, cl, cd, miscV, loc>>

\* evaluate: decide every ready entry not yet runnable.
H_Eval(p) ==
    /\ pc[p] = "h_eval"
    /\ LET n == loc[p].n
           r == DecideAll(n, SetToSeq(DOMAIN ent[n]), NS(n), WK(p))
       IN SetNS(n, r[1]) /\ SetWK(p, r[2])
    /\ pc' = [pc EXCEPT ![p] = "h_drain"]
    /\ UNCHANGED <<timeV, epV, fr, plan, cd, miscV, loc>>

-----------------------------------------------------------------------------

ProcNext(p) ==
    \/ FrameStep(p) \/ FrameEnd(p) \/ ReleaseStep(p) \/ DrainEnd(p) \/ CloseStep(p) \/ CloseEnd(p)
    \/ W_Write(p) \/ W_Deliver(p)
    \/ T_Pick(p) \/ T_Start(p) \/ T_Poll(p) \/ T_Snap(p) \/ T_Rec(p) \/ T_Child(p) \/ T_Mark(p)
    \/ T_AfterMark(p) \/ T_Fin(p) \/ T_Resume(p)
    \/ H_Supp(p) \/ H_Seed(p) \/ H_Eval(p)

Init ==
    /\ clock = 0 /\ version = [n \in Node |-> 0] /\ ranAt = [n \in Node |-> 0] /\ relAt = [n \in Node |-> 0]
    /\ ent = [n \in Node |-> EmptyF] /\ run = [n \in Node |-> NoRun] /\ inh = [n \in Node |-> EmptyF]
    /\ queued = [n \in Node |-> FALSE] /\ spawned = [n \in Node |-> FALSE] /\ suppIn = [n \in Node |-> {}]
    /\ epOn = {} /\ epPar = [e \in Epoch |-> 0] /\ own = Zero /\ sub = Zero
    /\ settled = [e \in Epoch |-> FALSE] /\ wlog = [e \in Epoch |-> {}] /\ hooks = [e \in Epoch |-> {}]
    /\ scope = 0 /\ nextE = 1 /\ undelivered = {}
    /\ pc = [p \in Procs |-> CASE p[1] = "W" -> "w_apply" [] p[1] = "T" -> "idle" [] OTHER -> "free"]
    /\ fr = [p \in Procs |-> {}] /\ plan = [p \in Procs |-> {}] /\ rl = [p \in Procs |-> {}]
    /\ sp = [p \in Procs |-> {}] /\ hk = [p \in Procs |-> {}]
    /\ cl = [p \in Procs |-> Zero] /\ cd = [p \in Procs |-> Zero]
    /\ loc = [p \in Procs |-> InitLoc(p)]
    /\ hookFree = 1..MaxHooks /\ anomaly = {} /\ glitch = FALSE

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
        /\ spawned[n] => queued[n]

\* When the scope's count is zero (flush returns), every node's last run started after its
\* parents' last changes, except changes not yet delivered.
ScopeConsistent ==
    scope = 0 => \A c \in Derived : \A q \in Parents[c] :
        version[q] <= ranAt[c] \/ <<q, version[q]>> \in undelivered

\* flush in a writer's scope returns once its root epoch has no open work in its subtree; by then
\* every child of a source it wrote has released a run started after the write. Cut writers rerun
\* later by design, so this holds without writes from bodies.
AbsorbedReleased == NoBodyWrites =>
    \A e \in epOn : epPar[e] = 0 /\ sub[e] = 0 =>
        \A y \in wlog[e] : Len(y) = 3 => \A c \in Derived : y[2] \in Parents[c] => relAt[c] >= y[3]

\* Without writes from bodies, a run never starts on an entry while a parent still owes the epoch.
NoGlitch == NoBodyWrites => ~glitch

Quiet ==
    /\ \A p \in Procs : pc[p] \in {"idle", "done", "free"}
    /\ \A n \in Node : ~run[n].on /\ ~queued[n] /\ ent[n] = EmptyF /\ inh[n] = EmptyF
Terminates == <>Quiet
=============================================================================
