---------------------------- MODULE Admission ----------------------------
(***************************************************************************)
(* The admission path of the coordinator: CoordinatorService::before_tool  *)
(* in crates/coordinator/src/adapter.rs, which runs core::step and appends *)
(* the recorded event to the run's trace.                                  *)
(*                                                                         *)
(* One TLA+ action is one indivisible step of that function. The mutex     *)
(* `self.inner` is held from `lock()` until the guard drops when the       *)
(* function returns, so every step between lock and unlock is atomic with  *)
(* respect to the other call.                                              *)
(*                                                                         *)
(*   Lock          let mut inner = self.inner.lock()                       *)
(*   Step          core::step(inner.state.clone(), BeforeTool(call), now)  *)
(*   AppendOk      inner.trace.append(&event)  returned Ok                 *)
(*   AppendFails   inner.trace.append(&event)? returned Err                *)
(*   NoRecord      the core emitted no Record (unknown slot)               *)
(*   Commit        inner.state = next                                      *)
(*   Unlock        the guard drops as before_tool returns Ok(admission)    *)
(*   Reply         Ok(Response::new(to_wire(admission))) reaches the shim  *)
(*                                                                         *)
(* See the coverage table in .agents/skills/formal-verify-admission/       *)
(* SKILL.md for file:function for every action.                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Calls,       \* the calls in flight (two): each one a call of before_tool
    Slots,       \* the slots of the run
    WithGhost,   \* TRUE: a call may name a slot that is not in the run
    Mutation     \* "none", or the name of a deliberately broken variant

Ghost == "ghost-slot"
AllSlots == IF WithGhost THEN Slots \cup {Ghost} ELSE Slots

VARIABLES
    slotOf,    \* [Calls -> AllSlots]: the slot each call names (never changes)
    pc,        \* [Calls -> string]: where each call is inside before_tool
    holder,    \* SUBSET Calls: who holds the lock on `inner` ({} = free)
    nextId,    \* inner.state.next_event_id: the committed core state
    clk,       \* [Slots -> Nat]: each slot's own clock entry in inner.state
    out,       \* [Calls -> record]: what core::step returned for the call
    trace,     \* Seq(record): the rows of trace.db, in commit order
    reply,     \* [Calls -> string]: what the shim got back
    injected   \* SUBSET Calls: calls whose append the environment made fail

vars == <<slotOf, pc, holder, nextId, clk, out, trace, reply, injected>>

NoOut == [has |-> FALSE, id |-> 0, slot |-> "-", stamp |-> 0,
          nid |-> 0, nclk |-> [s \in Slots |-> 0], verdict |-> "-"]

TraceIds == {trace[i].id : i \in DOMAIN trace}

(* A call is inside the critical section from the first line after lock()  *)
(* up to and including `inner.state = next`. Outside it the committed      *)
(* state and the trace must agree.                                         *)
InCritical(k) == pc[k] \in {"locked", "stepped", "early_commit",
                            "to_commit", "committed"}
Quiet == \A k \in Calls : ~InCritical(k)

-----------------------------------------------------------------------------
Init ==
    /\ slotOf \in [Calls -> AllSlots]
    /\ pc = [k \in Calls |-> "new"]
    /\ holder = {}
    /\ nextId = 1                        \* State::new: event ids start at 1
    /\ clk = [s \in Slots |-> 0]         \* every slot's clock starts at zero
    /\ out = [k \in Calls |-> NoOut]
    /\ trace = <<>>
    /\ reply = [k \in Calls |-> "-"]
    /\ injected = {}

(* `self.inner.lock()`: blocks while the other call holds it. *)
Lock(k) ==
    /\ pc[k] = "new"
    /\ (Mutation = "no_mutex" \/ holder = {})
    /\ holder' = holder \cup {k}
    /\ pc' = [pc EXCEPT ![k] = "locked"]
    /\ UNCHANGED <<slotOf, nextId, clk, out, trace, reply, injected>>

(* `core::step(inner.state.clone(), ...)`: works on a copy. A known slot    *)
(* gets the next event id and its clock ticked (clock rule 1); an unknown   *)
(* slot gets a Deny, no Record and the state unchanged.                     *)
(* Broken variants: "no_tick" skips the tick; "record_unknown_slot" records *)
(* an event for the unknown slot as well.                                   *)
Step(k) ==
    /\ pc[k] = "locked"
    /\ LET s == slotOf[k]
           known == s \in Slots
           stampOf == IF ~known THEN 0
                      ELSE IF Mutation = "no_tick" THEN clk[s] ELSE clk[s] + 1
       IN out' = [out EXCEPT ![k] =
              IF known \/ Mutation = "record_unknown_slot"
              THEN [has |-> TRUE, id |-> nextId, slot |-> s, stamp |-> stampOf,
                    nid |-> nextId + 1,
                    nclk |-> IF known THEN [clk EXCEPT ![s] = stampOf] ELSE clk,
                    verdict |-> IF known THEN "Allow" ELSE "Deny"]
              ELSE [has |-> FALSE, id |-> 0, slot |-> s, stamp |-> 0,
                    nid |-> nextId, nclk |-> clk, verdict |-> "Deny"]]
    /\ pc' = [pc EXCEPT ![k] =
                IF Mutation = "commit_before_append" THEN "early_commit"
                ELSE "stepped"]
    /\ UNCHANGED <<slotOf, holder, nextId, clk, trace, reply, injected>>

(* Only in the broken variant "commit_before_append": `inner.state = next`  *)
(* moved above the append loop.                                             *)
EarlyCommit(k) ==
    /\ pc[k] = "early_commit"
    /\ Mutation = "commit_before_append"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ pc' = [pc EXCEPT ![k] = "stepped"]
    /\ UNCHANGED <<slotOf, holder, out, trace, reply, injected>>

AfterAppend == IF Mutation = "commit_before_append" THEN "committed"
               ELSE "to_commit"

(* `inner.trace.append(&event)` returned Ok: one row, durable. The id is    *)
(* the primary key, so a row with that id already there makes it fail.      *)
AppendOk(k) ==
    /\ pc[k] = "stepped"
    /\ out[k].has
    /\ out[k].id \notin TraceIds
    /\ trace' = Append(trace, [id |-> out[k].id, slot |-> out[k].slot,
                               stamp |-> out[k].stamp, call |-> k])
    /\ pc' = [pc EXCEPT ![k] = AfterAppend]
    /\ UNCHANGED <<slotOf, holder, nextId, clk, out, reply, injected>>

(* The append fails for a reason outside the coordinator (the test plants   *)
(* a conflicting row; a disk error would do the same). Nothing is written.  *)
(* Baseline: `?` returns Err and the guard drops, the state stays.          *)
(* Broken variant "ignore_append_error": the error is dropped and the call  *)
(* carries on to commit and Allow.                                          *)
AppendFails(k) ==
    /\ pc[k] = "stepped"
    /\ out[k].has
    /\ injected' = injected \cup {k}
    /\ IF Mutation = "ignore_append_error"
       THEN /\ pc' = [pc EXCEPT ![k] = AfterAppend]
            /\ UNCHANGED holder
       ELSE /\ pc' = [pc EXCEPT ![k] = "failed"]
            /\ holder' = holder \ {k}
    /\ UNCHANGED <<slotOf, nextId, clk, out, trace, reply>>

(* The id is already a row: only possible when the two calls are not        *)
(* serialized (Mutation = "no_mutex"). The primary key refuses the row.     *)
AppendIdClash(k) ==
    /\ pc[k] = "stepped"
    /\ out[k].has
    /\ out[k].id \in TraceIds
    /\ pc' = [pc EXCEPT ![k] = "failed"]
    /\ holder' = holder \ {k}
    /\ UNCHANGED <<slotOf, nextId, clk, out, trace, reply, injected>>

(* The core emitted only an Admit: nothing to append. *)
NoRecord(k) ==
    /\ pc[k] = "stepped"
    /\ ~out[k].has
    /\ pc' = [pc EXCEPT ![k] = AfterAppend]
    /\ UNCHANGED <<slotOf, holder, nextId, clk, out, trace, reply, injected>>

(* `inner.state = next`, after every output was performed. *)
Commit(k) ==
    /\ pc[k] = "to_commit"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ pc' = [pc EXCEPT ![k] = "committed"]
    /\ UNCHANGED <<slotOf, holder, out, trace, reply, injected>>

(* The function returns Ok(admission): the guard drops, the lock is free. *)
Unlock(k) ==
    /\ pc[k] = "committed"
    /\ holder' = holder \ {k}
    /\ pc' = [pc EXCEPT ![k] = "returned"]
    /\ UNCHANGED <<slotOf, nextId, clk, out, trace, reply, injected>>

(* The tonic handler answers the shim; an Err becomes Status::internal.     *)
Reply(k) ==
    /\ pc[k] \in {"returned", "failed"}
    /\ reply' = [reply EXCEPT ![k] =
                   IF pc[k] = "failed" THEN "Err" ELSE out[k].verdict]
    /\ pc' = [pc EXCEPT ![k] = "done"]
    /\ UNCHANGED <<slotOf, holder, nextId, clk, out, trace, injected>>

(* End state: both calls answered. Nothing changes; named so TLC does not   *)
(* report the end as a deadlock.                                            *)
Done ==
    /\ \A k \in Calls : pc[k] = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E k \in Calls : Lock(k)
    \/ \E k \in Calls : Step(k)
    \/ \E k \in Calls : EarlyCommit(k)
    \/ \E k \in Calls : AppendOk(k)
    \/ \E k \in Calls : AppendFails(k)
    \/ \E k \in Calls : AppendIdClash(k)
    \/ \E k \in Calls : NoRecord(k)
    \/ \E k \in Calls : Commit(k)
    \/ \E k \in Calls : Unlock(k)
    \/ \E k \in Calls : Reply(k)
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* The rules. *)

TypeOK ==
    /\ slotOf \in [Calls -> AllSlots]
    /\ holder \subseteq Calls
    /\ nextId \in Nat
    /\ clk \in [Slots -> Nat]
    /\ reply \in [Calls -> {"-", "Allow", "Deny", "Err"}]
    /\ injected \subseteq Calls

(* (a) Every Allow the shim got has its BeforeTool event in the trace.      *)
AllowIsDurable ==
    \A k \in Calls :
        reply[k] = "Allow" => \E i \in DOMAIN trace : trace[i].call = k

(* A Deny of an unknown slot records nothing, and nothing in the trace      *)
(* names a slot outside the run.                                            *)
DenyRecordsNothing ==
    \A k \in Calls :
        reply[k] = "Deny" => \A i \in DOMAIN trace : trace[i].call # k

(* (b) The ids in the trace, in commit order, are 1, 2, 3, ...: unique,     *)
(* strictly increasing, none skipped.                                       *)
IdsConsecutive ==
    \A i \in DOMAIN trace : trace[i].id = i

(* (c) Outside the critical section the committed core state is exactly     *)
(* what the trace holds: a failed append left next_event_id and the slot's  *)
(* clock where they were, so a retry reuses the same id and stamp.          *)
CoreMatchesTrace ==
    Quiet =>
        /\ nextId = Len(trace) + 1
        /\ \A s \in Slots :
             clk[s] = Cardinality({i \in DOMAIN trace : trace[i].slot = s})

(* (d) Each slot's stamps strictly increase in commit order. *)
StampsIncrease ==
    \A i, j \in DOMAIN trace :
        (i < j /\ trace[i].slot = trace[j].slot) => trace[i].stamp < trace[j].stamp

(* (e) What the mutex buys: two calls never make each other fail. A call    *)
(* gets an Err only when its own append failed for an outside reason.       *)
CallsDoNotCollide ==
    \A k \in Calls : reply[k] = "Err" => k \in injected

-----------------------------------------------------------------------------
(* A witness, expected to be VIOLATED: the retry after a failed append      *)
(* reuses the failed call's id and stamp. Its violation shows the model     *)
(* reaches the case rule (c) is about, so a pass is not vacuous.            *)
RetryReusesIdAndStamp ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ reply[k1] = "Err"
        /\ reply[k2] = "Allow"
        /\ \E i \in DOMAIN trace :
             /\ trace[i].call = k2
             /\ trace[i].id = out[k1].id
             /\ trace[i].stamp = out[k1].stamp
WitnessNotReached == ~RetryReusesIdAndStamp

=============================================================================
