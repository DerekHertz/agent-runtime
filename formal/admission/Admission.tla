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
(*                 with Identities::record_before (core/identity.rs)       *)
(*   BlobOk        inside TraceWriter::append: self.blobs.put(tool_input)  *)
(*                 returned Ok (the blob is flushed)                       *)
(*   BlobFails     the same put returned Err: no row is inserted           *)
(*   AppendOk      the INSERT inside TraceWriter::append returned Ok       *)
(*   AppendFails   the INSERT returned Err (the blob stays, an orphan)     *)
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
    Payloads,    \* the raw tool inputs a call may carry (calls may share one)
    Slots,       \* the slots of the run
    Sessions,    \* the session_ids a call may carry
    UseIds,      \* the tool_use_ids a call may carry
    WithGhost,   \* TRUE: a call may name a slot that is not in the run
    Mutation     \* "none", or the name of a deliberately broken variant

Ghost == "ghost-slot"
AllSlots == IF WithGhost THEN Slots \cup {Ghost} ELSE Slots
NoSlot == "unbound"

VARIABLES
    slotOf,    \* [Calls -> AllSlots]: the slot each call names (never changes)
    payloadOf, \* [Calls -> Payloads]: the raw tool input of each call (never changes)
    sessionOf, \* [Calls -> Sessions]: the session_id of each call (never changes)
    useIdOf,   \* [Calls -> UseIds]: the tool_use_id of each call (never changes)
    blobs,     \* SUBSET Payloads: the blobs stored and flushed in runs/<id>/blobs/
    pc,        \* [Calls -> string]: where each call is inside before_tool
    holder,    \* SUBSET Calls: who holds the lock on `inner` ({} = free)
    nextId,    \* inner.state.next_event_id: the committed core state
    clk,       \* [Slots -> Nat]: each slot's own clock entry in inner.state
    bound,     \* [Sessions -> Slots \cup {NoSlot}]: session bindings in inner.state
    seen,      \* SUBSET (Sessions \X UseIds): call identities in inner.state
    out,       \* [Calls -> record]: what core::step returned for the call
    trace,     \* Seq(record): the rows of trace.db, in commit order
    reply,     \* [Calls -> string]: what the shim got back
    injected   \* SUBSET Calls: calls whose append the environment made fail

vars == <<slotOf, payloadOf, sessionOf, useIdOf, blobs, pc, holder, nextId, clk, bound, seen,
          out, trace, reply, injected>>
\* The parts of the state the identity actions never change, and the per-call constants.
ids == <<sessionOf, useIdOf>>

NoOut == [has |-> FALSE, id |-> 0, slot |-> "-", stamp |-> 0,
          nid |-> 0, nclk |-> [s \in Slots |-> 0],
          nbound |-> [x \in Sessions |-> NoSlot], nseen |-> {}, verdict |-> "-"]

TraceIds == {trace[i].id : i \in DOMAIN trace}

(* A call is inside the critical section from the first line after lock()  *)
(* up to and including `inner.state = next`. Outside it the committed      *)
(* state and the trace must agree.                                         *)
InCritical(k) == pc[k] \in {"locked", "stepped", "early_commit", "blob_stored",
                            "row_written", "to_commit", "committed"}
Quiet == \A k \in Calls : ~InCritical(k)

-----------------------------------------------------------------------------
Init ==
    /\ slotOf \in [Calls -> AllSlots]
    /\ payloadOf \in [Calls -> Payloads]
    /\ sessionOf \in [Calls -> Sessions]
    /\ useIdOf \in [Calls -> UseIds]
    /\ blobs = {}                        \* a new run's blobs/ is empty
    /\ pc = [k \in Calls |-> "new"]
    /\ holder = {}
    /\ nextId = 1                        \* State::new: event ids start at 1
    /\ clk = [s \in Slots |-> 0]         \* every slot's clock starts at zero
    /\ bound = [x \in Sessions |-> NoSlot] \* no session is bound yet
    /\ seen = {}                         \* no call is recorded yet
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
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, nextId, clk, bound, seen, out, trace, reply, injected>>

(* `core::step(inner.state.clone(), ...)`: works on a copy. An unknown slot *)
(* gets a Deny. A known slot then has its call's identity checked by        *)
(* Identities::record_before: a session bound to another slot, or a         *)
(* (session, tool_use_id) already recorded, gets a Deny too. Every Deny has *)
(* no Record and the state unchanged. Otherwise the call gets the next      *)
(* event id, its slot's clock ticked (clock rule 1), its session bound and  *)
(* its identity recorded, all in the copy. Empty ids are refused by the     *)
(* same check and are not modeled separately.                               *)
(* Broken variants: "no_tick" skips the tick; "record_unknown_slot" records *)
(* an event for the unknown slot as well; "no_binding_check" and            *)
(* "no_repeat_check" skip one identity check; "identity_before_append"      *)
(* writes the binding and the identity into inner.state at once, before    *)
(* the append.                                                              *)
Step(k) ==
    /\ pc[k] = "locked"
    /\ LET s == slotOf[k]
           x == sessionOf[k]
           c == <<x, useIdOf[k]>>
           known == s \in Slots
           sameSlot == bound[x] \in {NoSlot, s} \/ Mutation = "no_binding_check"
           fresh == c \notin seen \/ Mutation = "no_repeat_check"
           admitted == known /\ sameSlot /\ fresh
           stampOf == IF ~known THEN 0
                      ELSE IF Mutation = "no_tick" THEN clk[s] ELSE clk[s] + 1
           nbound == IF admitted THEN [bound EXCEPT ![x] = s] ELSE bound
           nseen == IF admitted THEN seen \cup {c} ELSE seen
       IN /\ out' = [out EXCEPT ![k] =
                 IF admitted \/ (~known /\ Mutation = "record_unknown_slot")
                 THEN [has |-> TRUE, id |-> nextId, slot |-> s, stamp |-> stampOf,
                       nid |-> nextId + 1,
                       nclk |-> IF known THEN [clk EXCEPT ![s] = stampOf] ELSE clk,
                       nbound |-> nbound, nseen |-> nseen,
                       verdict |-> IF known THEN "Allow" ELSE "Deny"]
                 ELSE [has |-> FALSE, id |-> 0, slot |-> s, stamp |-> 0,
                       nid |-> nextId, nclk |-> clk, nbound |-> bound, nseen |-> seen,
                       verdict |-> "Deny"]]
          /\ IF Mutation = "identity_before_append"
             THEN bound' = nbound /\ seen' = nseen
             ELSE UNCHANGED <<bound, seen>>
    /\ pc' = [pc EXCEPT ![k] =
                IF Mutation = "commit_before_append" THEN "early_commit"
                ELSE "stepped"]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, nextId, clk, trace, reply, injected>>

(* Only in the broken variant "commit_before_append": `inner.state = next`  *)
(* moved above the append loop.                                             *)
EarlyCommit(k) ==
    /\ pc[k] = "early_commit"
    /\ Mutation = "commit_before_append"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ bound' = out[k].nbound
    /\ seen' = out[k].nseen
    /\ pc' = [pc EXCEPT ![k] = "stepped"]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, out, trace, reply, injected>>

AfterAppend == IF Mutation = "commit_before_append" THEN "committed"
               ELSE "to_commit"

(* TraceWriter::append stores the payload blob, then inserts the row. The   *)
(* broken variant "row_before_blob" swaps the two.                          *)
RowFirst == Mutation = "row_before_blob"
BlobFrom == IF RowFirst THEN "row_written" ELSE "stepped"
AfterBlob == IF RowFirst THEN AfterAppend ELSE "blob_stored"
RowFrom == IF RowFirst THEN "stepped" ELSE "blob_stored"
AfterRow == IF RowFirst THEN "row_written" ELSE AfterAppend

(* `self.blobs.put(tool_input)` returned Ok: the blob is in blobs/ and      *)
(* flushed. Equal payloads share one blob (a set).                          *)
BlobOk(k) ==
    /\ pc[k] = BlobFrom
    /\ out[k].has
    /\ blobs' = blobs \cup {payloadOf[k]}
    /\ pc' = [pc EXCEPT ![k] = AfterBlob]
    /\ UNCHANGED <<slotOf, payloadOf, ids, holder, nextId, clk, bound, seen, out, trace, reply, injected>>

(* The put failed (a full disk, a read-only blobs/; the test makes blobs/   *)
(* read-only). No blob counts as stored and no row is inserted; `?` returns *)
(* Err, the guard drops, the state stays. Under "ignore_append_error" the   *)
(* error is dropped like any other append error.                            *)
BlobFails(k) ==
    /\ pc[k] = BlobFrom
    /\ out[k].has
    /\ injected' = injected \cup {k}
    /\ IF Mutation = "ignore_append_error"
       THEN /\ pc' = [pc EXCEPT ![k] = AfterBlob]
            /\ UNCHANGED holder
       ELSE /\ pc' = [pc EXCEPT ![k] = "failed"]
            /\ holder' = holder \ {k}
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, nextId, clk, bound, seen, out, trace, reply>>

(* The INSERT returned Ok: one row, durable, naming the call's blob. The id *)
(* is the primary key, so a row with that id already there makes it fail.   *)
AppendOk(k) ==
    /\ pc[k] = RowFrom
    /\ out[k].has
    /\ out[k].id \notin TraceIds
    /\ trace' = Append(trace, [id |-> out[k].id, slot |-> out[k].slot,
                               stamp |-> out[k].stamp, call |-> k,
                               blob |-> payloadOf[k],
                               session |-> sessionOf[k], use |-> useIdOf[k]])
    /\ pc' = [pc EXCEPT ![k] = AfterRow]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, nextId, clk, bound, seen, out, reply, injected>>

(* The INSERT fails for a reason outside the coordinator (the test plants a *)
(* conflicting row; a disk error would do the same). No row is written; a   *)
(* blob already stored stays as an orphan that no row names.                *)
(* Baseline: `?` returns Err and the guard drops, the state stays.          *)
(* Broken variant "ignore_append_error": the error is dropped and the call  *)
(* carries on to commit and Allow.                                          *)
AppendFails(k) ==
    /\ pc[k] = RowFrom
    /\ out[k].has
    /\ injected' = injected \cup {k}
    /\ IF Mutation = "ignore_append_error"
       THEN /\ pc' = [pc EXCEPT ![k] = AfterRow]
            /\ UNCHANGED holder
       ELSE /\ pc' = [pc EXCEPT ![k] = "failed"]
            /\ holder' = holder \ {k}
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, nextId, clk, bound, seen, out, trace, reply>>

(* The id is already a row: only possible when the two calls are not        *)
(* serialized (Mutation = "no_mutex"). The primary key refuses the row.     *)
AppendIdClash(k) ==
    /\ pc[k] = RowFrom
    /\ out[k].has
    /\ out[k].id \in TraceIds
    /\ pc' = [pc EXCEPT ![k] = "failed"]
    /\ holder' = holder \ {k}
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, nextId, clk, bound, seen, out, trace, reply, injected>>

(* The core emitted only an Admit: nothing to append. *)
NoRecord(k) ==
    /\ pc[k] = "stepped"
    /\ ~out[k].has
    /\ pc' = [pc EXCEPT ![k] = AfterAppend]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, nextId, clk, bound, seen, out, trace, reply, injected>>

(* `inner.state = next`, after every output was performed. *)
Commit(k) ==
    /\ pc[k] = "to_commit"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ bound' = out[k].nbound
    /\ seen' = out[k].nseen
    /\ pc' = [pc EXCEPT ![k] = "committed"]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, out, trace, reply, injected>>

(* The function returns Ok(admission): the guard drops, the lock is free. *)
Unlock(k) ==
    /\ pc[k] = "committed"
    /\ holder' = holder \ {k}
    /\ pc' = [pc EXCEPT ![k] = "returned"]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, nextId, clk, bound, seen, out, trace, reply, injected>>

(* The tonic handler answers the shim; an Err becomes Status::internal.     *)
Reply(k) ==
    /\ pc[k] \in {"returned", "failed"}
    /\ reply' = [reply EXCEPT ![k] =
                   IF pc[k] = "failed" THEN "Err" ELSE out[k].verdict]
    /\ pc' = [pc EXCEPT ![k] = "done"]
    /\ UNCHANGED <<slotOf, payloadOf, ids, blobs, holder, nextId, clk, bound, seen, out, trace, injected>>

(* End state: both calls answered. Nothing changes; named so TLC does not   *)
(* report the end as a deadlock.                                            *)
Done ==
    /\ \A k \in Calls : pc[k] = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E k \in Calls : Lock(k)
    \/ \E k \in Calls : Step(k)
    \/ \E k \in Calls : EarlyCommit(k)
    \/ \E k \in Calls : BlobOk(k)
    \/ \E k \in Calls : BlobFails(k)
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
    /\ payloadOf \in [Calls -> Payloads]
    /\ blobs \subseteq Payloads
    /\ holder \subseteq Calls
    /\ nextId \in Nat
    /\ clk \in [Slots -> Nat]
    /\ sessionOf \in [Calls -> Sessions]
    /\ useIdOf \in [Calls -> UseIds]
    /\ bound \in [Sessions -> Slots \cup {NoSlot}]
    /\ seen \subseteq Sessions \X UseIds
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
(* what the trace holds: a failed append left next_event_id, the slot's     *)
(* clock, the session bindings and the recorded call identities where they  *)
(* were, so a retry reuses the same id and stamp and is not refused as a    *)
(* repeat of itself.                                                        *)
CoreMatchesTrace ==
    Quiet =>
        /\ nextId = Len(trace) + 1
        /\ \A s \in Slots :
             clk[s] = Cardinality({i \in DOMAIN trace : trace[i].slot = s})
        /\ seen = {<<trace[i].session, trace[i].use>> : i \in DOMAIN trace}
        /\ \A x \in Sessions :
             IF \E i \in DOMAIN trace : trace[i].session = x
             THEN \E i \in DOMAIN trace : trace[i].session = x /\ bound[x] = trace[i].slot
             ELSE bound[x] = NoSlot

(* (d) Each slot's stamps strictly increase in commit order. *)
StampsIncrease ==
    \A i, j \in DOMAIN trace :
        (i < j /\ trace[i].slot = trace[j].slot) => trace[i].stamp < trace[j].stamp

(* (f) Every row names a blob that is stored and flushed: the blob is put   *)
(* before its row, so no failure leaves a row without its blob. A blob that *)
(* no row names (an orphan after a failed INSERT) is allowed.               *)
RowsNameStoredBlobs ==
    \A i \in DOMAIN trace : trace[i].blob \in blobs

(* (e) What the mutex buys: two calls never make each other fail. A call    *)
(* gets an Err only when its own append failed for an outside reason.       *)
CallsDoNotCollide ==
    \A k \in Calls : reply[k] = "Err" => k \in injected

(* (g) A session stays on one slot: every row of a session names the same   *)
(* slot.                                                                    *)
SessionStaysOnSlot ==
    \A i, j \in DOMAIN trace :
        trace[i].session = trace[j].session => trace[i].slot = trace[j].slot

(* (g) A call identity (session, tool_use_id) is recorded at most once.     *)
CallRecordedOnce ==
    \A i, j \in DOMAIN trace :
        (i # j) => <<trace[i].session, trace[i].use>> # <<trace[j].session, trace[j].use>>

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

(* A second witness, expected to be VIOLATED: a call stored its blob, its   *)
(* INSERT failed, and no row names that blob, which rule (f) allows. Its    *)
(* violation shows the model reaches the orphan case, so (f) is not proved  *)
(* only because blobs never outlive a failure.                              *)
OrphanBlobReached ==
    \E k \in Calls :
        /\ reply[k] = "Err"
        /\ payloadOf[k] \in blobs
        /\ \A i \in DOMAIN trace : trace[i].blob # payloadOf[k]
OrphanNotReached == ~OrphanBlobReached

(* Two identity witnesses, each expected to be VIOLATED. The first: a call  *)
(* of a known slot was refused for its identity (a repeat, or its session   *)
(* bound to the other slot), so rule (g) is not vacuous. The second: a call *)
(* whose write failed was retried with the same session and tool_use_id    *)
(* and admitted, so the identity part of (c) is reached.                    *)
IdentityRefusalReached == \E k \in Calls : reply[k] = "Deny" /\ slotOf[k] \in Slots
IdentityRefusalNotReached == ~IdentityRefusalReached
FailedIdentityRetried ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ reply[k1] = "Err"
        /\ reply[k2] = "Allow"
        /\ <<sessionOf[k1], useIdOf[k1]>> = <<sessionOf[k2], useIdOf[k2]>>
FailedIdentityNotRetried == ~FailedIdentityRetried

=============================================================================
