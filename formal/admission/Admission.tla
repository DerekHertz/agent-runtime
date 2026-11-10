---------------------------- MODULE Admission ----------------------------
(***************************************************************************)
(* The recording path of the coordinator: CoordinatorService::            *)
(* step_and_record in crates/coordinator/src/adapter.rs, which every RPC   *)
(* uses. It runs core::step (a BeforeTool admission, an AfterTool          *)
(* completion report, or a Stop or SessionEnd lifecycle report) and        *)
(* appends the recorded event to the run's trace.                          *)
(*                                                                         *)
(* One TLA+ action is one indivisible step of that function. The mutex     *)
(* `self.inner` is held from `lock()` until the guard drops when the       *)
(* function returns, so every step between lock and unlock is atomic with  *)
(* respect to the other call.                                              *)
(*                                                                         *)
(*   Lock          let mut inner = self.inner.lock()                       *)
(*   Step          core::step(inner.state.clone(), input, now): for a      *)
(*                 BeforeTool Identities::record_before, for an AfterTool  *)
(*                 (a success or a failure) completion::after_tool and     *)
(*                 Identities::record_after, for a lifecycle report        *)
(*                 lifecycle::lifecycle and Identities::record_lifecycle   *)
(*   BlobOk        inside TraceWriter::append: one self.blobs.put returned *)
(*                 Ok (the event's payload, or an AfterTool's input)       *)
(*   BlobFails     that put returned Err: no row is inserted               *)
(*   AppendOk      the INSERT inside TraceWriter::append returned Ok       *)
(*   AppendFails   the INSERT returned Err (stored blobs stay as orphans)  *)
(*   NoRecord      the core emitted no Record (a refusal)                  *)
(*   Commit        inner.state = next                                      *)
(*   Unlock        the guard drops as step_and_record returns Ok(answer)   *)
(*   Reply         the tonic handler answers the shim                      *)
(*                                                                         *)
(* See the coverage table in .agents/skills/formal-verify-admission/       *)
(* SKILL.md for file:function for every action.                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Calls,       \* the calls in flight (two): each a BeforeTool or an AfterTool report
    Payloads,    \* the raw tool inputs a call may carry (calls may share one)
    Slots,       \* the slots of the run
    Sessions,    \* the session_ids a call may carry
    UseIds,      \* the tool_use_ids a call may carry
    Tools,       \* the tool names a call may carry
    Kinds,       \* the kinds a call may be: subset of {"B", "A", "F", "L"} (BeforeTool, an
                 \* AfterTool of a success (PostToolUse), an AfterTool of a failure
                 \* (PostToolUseFailure), a lifecycle report (Stop or SessionEnd: one
                 \* kind, because both take the same transition and differ only in the
                 \* event kind written, which the real tests check))
    WithGhost,   \* TRUE: a call may name a slot that is not in the run
    WithSeed,    \* TRUE: the run may start with one BeforeTool already recorded
    Mutation     \* "none", or the name of a deliberately broken variant

Ghost == "ghost-slot"
AllSlots == IF WithGhost THEN Slots \cup {Ghost} ELSE Slots
NoSlot == "unbound"
NoTool == "-"
NoBlob == "-"
\* The tool_use_id of a lifecycle row: it names no call, so its rows are no call's rows.
NoUse == "no-use"
Keys == Sessions \X UseIds
\* What the core remembers about one call: its BeforeTool's event id (0: none), its tool
\* (NoTool: the call is unknown), and its first AfterTool's event id (0: not completed).
NoCall == [before |-> 0, tool |-> NoTool, done |-> 0]

\* An AfterTool's response blob, and a failed AfterTool's error blob. Each call carries its own.
Resp(k) == "resp-" \o k
ErrBlob(k) == "err-" \o k
\* A lifecycle report's fields blob. Each call carries its own, like a response.
LifeBlob(k) == "life-" \o k

VARIABLES
    slotOf,    \* [Calls -> AllSlots]: the slot each call names (never changes)
    payloadOf, \* [Calls -> Payloads]: the raw tool input of each call (never changes)
    sessionOf, \* [Calls -> Sessions]: the session_id of each call (never changes)
    useIdOf,   \* [Calls -> UseIds]: the tool_use_id of each call (never changes)
    kindOf,    \* [Calls -> Kinds]: whether the call is a BeforeTool or an AfterTool
    toolOf,    \* [Calls -> Tools]: the tool each call names (never changes)
    blobs,     \* the blobs stored and flushed in runs/<id>/blobs/
    pc,        \* [Calls -> string]: where each call is inside step_and_record
    wr,        \* [Calls -> Nat]: the next write of a call in "writing" (see Writes)
    holder,    \* SUBSET Calls: who holds the lock on `inner` ({} = free)
    nextId,    \* inner.state.next_event_id: the committed core state
    clk,       \* [Slots -> Nat]: each slot's own clock entry in inner.state
    bound,     \* [Sessions -> Slots \cup {NoSlot}]: session bindings in inner.state
    calls,     \* [Keys -> call record]: the recorded calls (Identities.calls) in inner.state
    out,       \* [Calls -> record]: what core::step returned for the call
    trace,     \* Seq(record): the rows of trace.db, in commit order
    reply,     \* [Calls -> string]: what the shim got back
    injected   \* SUBSET Calls: calls whose blob or row write the environment made fail

vars == <<slotOf, payloadOf, sessionOf, useIdOf, kindOf, toolOf, blobs, pc, wr, holder, nextId,
          clk, bound, calls, out, trace, reply, injected>>
\* The per-call constants.
given == <<slotOf, payloadOf, sessionOf, useIdOf, kindOf, toolOf>>
\* The committed core state.
core == <<nextId, clk, bound, calls>>

NoOut == [has |-> FALSE, id |-> 0, slot |-> "-", stamp |-> 0, before |-> 0,
          nid |-> 0, nclk |-> [s \in Slots |-> 0],
          nbound |-> [x \in Sessions |-> NoSlot], ncalls |-> [c \in Keys |-> NoCall],
          verdict |-> "-"]

TraceIds == {trace[i].id : i \in DOMAIN trace}
KeyOf(r) == <<r.session, r.use>>

\* Whether a call is an AfterTool report, of either outcome. Both are recorded as AfterTool rows
\* and go through the same pairing and completion code.
IsAfter(k) == kindOf[k] \in {"A", "F"}

\* Whether a call is a lifecycle report (Stop or SessionEnd).
IsLife(k) == kindOf[k] = "L"

\* The outcome a call's AfterTool row must carry: its report's ("-" for a BeforeTool). The
\* interruption flag is part of a failure's outcome in the code; it is opaque data here, like the
\* payload bytes, and its tri-state is checked by the real tests.
ReportOutcome(k) == CASE kindOf[k] \in {"B", "L"} -> "-"
                      [] kindOf[k] = "A" -> "success"
                      [] kindOf[k] = "F" -> "failure"

(* The blob a call's row names as its payload: a BeforeTool's input, a     *)
(* successful AfterTool's response, a failed AfterTool's raw error, a       *)
(* lifecycle report's fields.                                               *)
PrimaryBlob(k) == CASE kindOf[k] = "B" -> payloadOf[k]
                    [] kindOf[k] = "A" -> Resp(k)
                    [] kindOf[k] = "F" -> ErrBlob(k)
                    [] kindOf[k] = "L" -> LifeBlob(k)

(* The outcome the row gets. Broken variant "failure_as_success": a failed  *)
(* report is recorded as a success (the wire or core mapping drops the      *)
(* failure).                                                                *)
RowOutcome(k) == IF kindOf[k] = "F" /\ Mutation = "failure_as_success"
                 THEN "success" ELSE ReportOutcome(k)

(* The writes TraceWriter::append makes for a recorded event, in order:    *)
(* "blob" puts the payload (an input, a response, an error or lifecycle    *)
(* fields), "input" puts an AfterTool's input, "row" is the INSERT. A      *)
(* BeforeTool and a lifecycle report have one blob. Broken variants: "row_before_blob" inserts first;           *)
(* "input_blob_after_row" puts an AfterTool's input after its row.         *)
RowFirst == Mutation = "row_before_blob"
Writes(k) ==
    IF ~IsAfter(k)
    THEN IF RowFirst THEN <<"row", "blob">> ELSE <<"blob", "row">>
    ELSE IF RowFirst THEN <<"row", "blob", "input">>
    ELSE IF Mutation = "input_blob_after_row" THEN <<"blob", "row", "input">>
    ELSE <<"blob", "input", "row">>
NextWrite(k) == Writes(k)[wr[k]]

(* A call is inside the critical section from the first line after lock()  *)
(* up to and including `inner.state = next`. Outside it the committed      *)
(* state and the trace must agree.                                         *)
InCritical(k) == pc[k] \in {"locked", "stepped", "early_commit", "writing", "to_commit",
                            "committed"}
Quiet == \A k \in Calls : ~InCritical(k)

(* The optional row already in the trace: a BeforeTool of (x1, u1) on the  *)
(* first slot, recorded before either call. It lets two calls show a       *)
(* completion, its failure and retry, or a repeated completion.            *)
SeedSlot == CHOOSE s \in Slots : TRUE
SeedSession == CHOOSE x \in Sessions : TRUE
SeedUse == CHOOSE u \in UseIds : TRUE
SeedTool == CHOOSE t \in Tools : TRUE
SeedPayload == CHOOSE p \in Payloads : TRUE
SeedRow == [id |-> 1, slot |-> SeedSlot, stamp |-> 1, call |-> "seed", kind |-> "B",
            blob |-> SeedPayload, inBlob |-> NoBlob, session |-> SeedSession, use |-> SeedUse,
            tool |-> SeedTool, before |-> 0, outcome |-> "-"]

-----------------------------------------------------------------------------
Init ==
    /\ slotOf \in [Calls -> AllSlots]
    /\ payloadOf \in [Calls -> Payloads]
    /\ sessionOf \in [Calls -> Sessions]
    /\ useIdOf \in [Calls -> UseIds]
    /\ kindOf \in [Calls -> Kinds]
    /\ toolOf \in [Calls -> Tools]
    \* A lifecycle report carries no tool, tool_use_id or tool input: fix those unused
    \* constants to one value, so TLC does not explore copies that differ only there.
    /\ \A k \in Calls : kindOf[k] = "L" =>
         /\ useIdOf[k] = SeedUse /\ toolOf[k] = SeedTool /\ payloadOf[k] = SeedPayload
    /\ pc = [k \in Calls |-> "new"]
    /\ wr = [k \in Calls |-> 0]
    /\ holder = {}
    /\ out = [k \in Calls |-> NoOut]
    /\ reply = [k \in Calls |-> "-"]
    /\ injected = {}
    /\ \E seeded \in IF WithSeed THEN BOOLEAN ELSE {FALSE} :
         IF seeded
         THEN /\ trace = <<SeedRow>>
              /\ blobs = {SeedPayload}
              /\ nextId = 2
              /\ clk = [s \in Slots |-> IF s = SeedSlot THEN 1 ELSE 0]
              /\ bound = [x \in Sessions |-> IF x = SeedSession THEN SeedSlot ELSE NoSlot]
              /\ calls = [c \in Keys |-> IF c = <<SeedSession, SeedUse>>
                                         THEN [before |-> 1, tool |-> SeedTool, done |-> 0]
                                         ELSE NoCall]
         ELSE /\ trace = <<>>                 \* a new run: empty trace and blobs/
              /\ blobs = {}
              /\ nextId = 1                   \* State::new: event ids start at 1
              /\ clk = [s \in Slots |-> 0]    \* every slot's clock starts at zero
              /\ bound = [x \in Sessions |-> NoSlot]
              /\ calls = [c \in Keys |-> NoCall]

(* `self.inner.lock()`: blocks while the other call holds it. *)
Lock(k) ==
    /\ pc[k] = "new"
    /\ (Mutation = "no_mutex" \/ holder = {})
    /\ holder' = holder \cup {k}
    /\ pc' = [pc EXCEPT ![k] = "locked"]
    /\ UNCHANGED <<given, blobs, wr, core, out, trace, reply, injected>>

(* `core::step(inner.state.clone(), ...)`: works on a copy. An unknown slot *)
(* is refused. A known slot then has its call's identity checked: a        *)
(* session bound to another slot is refused; a BeforeTool whose            *)
(* (session, tool_use_id) is already recorded is refused                   *)
(* (Identities::record_before); an AfterTool of a recorded call that names *)
(* another tool is refused (Identities::record_after). A refusal has no    *)
(* Record and the state unchanged. Otherwise the call gets the next event  *)
(* id, its slot's clock ticked (clock rule 1), its session bound, and in   *)
(* the copy: a BeforeTool records the call; an AfterTool pairs with the    *)
(* call's BeforeTool by identity alone (0 for an orphan, which is then     *)
(* recorded with no BeforeTool) and becomes the call's completion unless   *)
(* it already has one, whether it reports a success or a failure. Empty    *)
(* ids, an empty response and an empty error are refused by the same or    *)
(* earlier checks and are                                                  *)
(* not modeled separately. An empty tool is refused before any identity    *)
(* check (core::before_tool and completion::after_tool), so no call is     *)
(* recorded with a tool outside Tools, which never holds NoTool.           *)
(* Broken variants: "no_tick" skips the tick; "record_unknown_slot"        *)
(* records an event for the unknown slot as well; "no_binding_check",      *)
(* "no_repeat_check" and "no_tool_check" skip one identity check;          *)
(* "guess_before" pairs an orphan with the latest event; "last_completion" *)
(* lets a repeated report replace the completion; "success_only_completion"*)
(* lets only a successful report become the completion, so a failure       *)
(* leaves the call open; "identity_before_append"                           *)
(* writes the new binding and calls into inner.state at once, before the   *)
(* append; "completion_before_append" does so for an AfterTool's calls     *)
(* only.                                                                   *)
(* A lifecycle report (kind "L") names no call: only an unknown slot or a  *)
(* session bound to another slot refuses it (Identities::record_lifecycle).*)
(* Admitted, it gets the next id and a tick, binds its session if new, and *)
(* leaves every recorded call, completion and other binding as it was.     *)
(* Its empty session or empty fields are refused by the same checks and   *)
(* are not modeled separately.                                             *)
Step(k) ==
    /\ pc[k] = "locked"
    /\ LET s == slotOf[k]
           x == sessionOf[k]
           c == <<x, useIdOf[k]>>
           isAfter == IsAfter(k)
           isLife == IsLife(k)
           known == s \in Slots
           recorded == calls[c].tool # NoTool
           sameSlot == bound[x] \in {NoSlot, s} \/ Mutation = "no_binding_check"
           fits == IF isLife THEN TRUE
                   ELSE IF isAfter
                   THEN ~recorded \/ calls[c].tool = toolOf[k] \/ Mutation = "no_tool_check"
                   ELSE ~recorded \/ Mutation = "no_repeat_check"
           admitted == known /\ sameSlot /\ fits
           stampOf == IF ~known THEN 0
                      ELSE IF Mutation = "no_tick" THEN clk[s] ELSE clk[s] + 1
           pairs == IF recorded THEN calls[c].before
                    ELSE IF Mutation = "guess_before" THEN nextId - 1 ELSE 0
           doneOf == IF Mutation = "success_only_completion" /\ kindOf[k] = "F"
                     THEN calls[c].done
                     ELSE IF calls[c].done = 0 \/ Mutation = "last_completion"
                     THEN nextId ELSE calls[c].done
           newCall == IF isAfter
                      THEN [before |-> pairs, tool |-> toolOf[k], done |-> doneOf]
                      ELSE [before |-> nextId, tool |-> toolOf[k], done |-> 0]
           \* A lifecycle report binds its session on its first event and changes no
           \* call. Broken variants: "lifecycle_unbinds" unbinds the session (a SessionEnd
           \* that frees it for another slot); "lifecycle_forgets_calls" drops the
           \* session's recorded calls (so a late completion would be an orphan).
           nbound == IF ~admitted THEN bound
                     ELSE IF isLife /\ Mutation = "lifecycle_unbinds"
                     THEN [bound EXCEPT ![x] = NoSlot]
                     ELSE [bound EXCEPT ![x] = s]
           ncalls == IF ~admitted THEN calls
                     ELSE IF isLife
                     THEN IF Mutation = "lifecycle_forgets_calls"
                          THEN [d \in Keys |-> IF d[1] = x THEN NoCall ELSE calls[d]]
                          ELSE calls
                     ELSE [calls EXCEPT ![c] = newCall]
           yes == IF isAfter \/ isLife THEN "Ack" ELSE "Allow"
           no == IF isAfter \/ isLife THEN "Refused" ELSE "Deny"
       IN /\ out' = [out EXCEPT ![k] =
                 IF admitted \/ (~known /\ Mutation = "record_unknown_slot")
                 THEN [has |-> TRUE, id |-> nextId, slot |-> s, stamp |-> stampOf,
                       before |-> IF isAfter THEN pairs ELSE 0,
                       nid |-> nextId + 1,
                       nclk |-> IF known THEN [clk EXCEPT ![s] = stampOf] ELSE clk,
                       nbound |-> nbound, ncalls |-> ncalls,
                       verdict |-> IF known THEN yes ELSE no]
                 ELSE [has |-> FALSE, id |-> 0, slot |-> s, stamp |-> 0, before |-> 0,
                       nid |-> nextId, nclk |-> clk, nbound |-> bound, ncalls |-> calls,
                       verdict |-> no]]
          /\ IF Mutation = "identity_before_append"
                \/ (Mutation = "completion_before_append" /\ isAfter)
             THEN /\ calls' = ncalls
                  /\ bound' = IF Mutation = "identity_before_append" THEN nbound ELSE bound
             ELSE UNCHANGED <<bound, calls>>
    /\ pc' = [pc EXCEPT ![k] =
                IF Mutation = "commit_before_append" THEN "early_commit"
                ELSE "stepped"]
    /\ UNCHANGED <<given, blobs, wr, holder, nextId, clk, trace, reply, injected>>

(* Only in the broken variant "commit_before_append": `inner.state = next`  *)
(* moved above the append.                                                  *)
EarlyCommit(k) ==
    /\ pc[k] = "early_commit"
    /\ Mutation = "commit_before_append"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ bound' = out[k].nbound
    /\ calls' = out[k].ncalls
    /\ pc' = [pc EXCEPT ![k] = "stepped"]
    /\ UNCHANGED <<given, blobs, wr, holder, out, trace, reply, injected>>

AfterAppend == IF Mutation = "commit_before_append" THEN "committed"
               ELSE "to_commit"

(* `inner.trace.append(&event)` begins: the event's writes, in order. *)
StartWrites(k) ==
    /\ pc[k] = "stepped"
    /\ out[k].has
    /\ pc' = [pc EXCEPT ![k] = "writing"]
    /\ wr' = [wr EXCEPT ![k] = 1]
    /\ UNCHANGED <<given, blobs, holder, core, out, trace, reply, injected>>

(* Move to the next write, or leave append once the last one is done. *)
Advance(k) ==
    IF wr[k] = Len(Writes(k))
    THEN /\ pc' = [pc EXCEPT ![k] = AfterAppend]
         /\ UNCHANGED wr
    ELSE /\ wr' = [wr EXCEPT ![k] = wr[k] + 1]
         /\ UNCHANGED pc

(* A write failed for a reason outside the coordinator. Baseline: `?`      *)
(* returns Err, the guard drops, the state stays. Broken variant           *)
(* "ignore_append_error": the error is dropped and the call carries on.    *)
Fail(k) ==
    /\ injected' = injected \cup {k}
    /\ IF Mutation = "ignore_append_error"
       THEN /\ Advance(k)
            /\ UNCHANGED holder
       ELSE /\ pc' = [pc EXCEPT ![k] = "failed"]
            /\ holder' = holder \ {k}
            /\ UNCHANGED wr

(* One `self.blobs.put` returned Ok: the blob is in blobs/ and flushed.     *)
(* Equal contents share one blob (a set).                                   *)
BlobOk(k) ==
    /\ pc[k] = "writing"
    /\ NextWrite(k) \in {"blob", "input"}
    /\ blobs' = blobs \cup {IF NextWrite(k) = "blob" THEN PrimaryBlob(k) ELSE payloadOf[k]}
    /\ Advance(k)
    /\ UNCHANGED <<given, holder, core, out, trace, reply, injected>>

(* The put failed (a full disk, a read-only blobs/; the tests make blobs/   *)
(* read-only). No blob counts as stored and no row is inserted.             *)
BlobFails(k) ==
    /\ pc[k] = "writing"
    /\ NextWrite(k) \in {"blob", "input"}
    /\ Fail(k)
    /\ UNCHANGED <<given, blobs, core, out, trace, reply>>

(* The INSERT returned Ok: one row, durable, naming the call's blobs. The   *)
(* id is the primary key, so a row with that id already there fails it.     *)
AppendOk(k) ==
    /\ pc[k] = "writing"
    /\ NextWrite(k) = "row"
    /\ out[k].id \notin TraceIds
    /\ trace' = Append(trace, [id |-> out[k].id, slot |-> out[k].slot,
                               stamp |-> out[k].stamp, call |-> k,
                               kind |-> IF IsAfter(k) THEN "A"
                                        ELSE IF IsLife(k) THEN "L" ELSE "B",
                               blob |-> PrimaryBlob(k),
                               inBlob |-> IF IsAfter(k) THEN payloadOf[k] ELSE NoBlob,
                               session |-> sessionOf[k],
                               use |-> IF IsLife(k) THEN NoUse ELSE useIdOf[k],
                               tool |-> IF IsLife(k) THEN NoTool ELSE toolOf[k],
                               before |-> out[k].before,
                               outcome |-> RowOutcome(k)])
    /\ Advance(k)
    /\ UNCHANGED <<given, blobs, holder, core, out, reply, injected>>

(* The INSERT fails for a reason outside the coordinator (the tests plant a *)
(* conflicting row). No row is written; blobs already stored stay as        *)
(* orphans that no row names.                                               *)
AppendFails(k) ==
    /\ pc[k] = "writing"
    /\ NextWrite(k) = "row"
    /\ Fail(k)
    /\ UNCHANGED <<given, blobs, core, out, trace, reply>>

(* The id is already a row: only possible when the two calls are not        *)
(* serialized (Mutation = "no_mutex"). The primary key refuses the row.     *)
AppendIdClash(k) ==
    /\ pc[k] = "writing"
    /\ NextWrite(k) = "row"
    /\ out[k].id \in TraceIds
    /\ pc' = [pc EXCEPT ![k] = "failed"]
    /\ holder' = holder \ {k}
    /\ UNCHANGED <<given, blobs, wr, core, out, trace, reply, injected>>

(* The core emitted only an answer: nothing to append. *)
NoRecord(k) ==
    /\ pc[k] = "stepped"
    /\ ~out[k].has
    /\ pc' = [pc EXCEPT ![k] = AfterAppend]
    /\ UNCHANGED <<given, blobs, wr, holder, core, out, trace, reply, injected>>

(* `inner.state = next`, after the append. *)
Commit(k) ==
    /\ pc[k] = "to_commit"
    /\ nextId' = out[k].nid
    /\ clk' = out[k].nclk
    /\ bound' = out[k].nbound
    /\ calls' = out[k].ncalls
    /\ pc' = [pc EXCEPT ![k] = "committed"]
    /\ UNCHANGED <<given, blobs, wr, holder, out, trace, reply, injected>>

(* The function returns Ok(answer): the guard drops, the lock is free. *)
Unlock(k) ==
    /\ pc[k] = "committed"
    /\ holder' = holder \ {k}
    /\ pc' = [pc EXCEPT ![k] = "returned"]
    /\ UNCHANGED <<given, blobs, wr, core, out, trace, reply, injected>>

(* The tonic handler answers the shim; an Err becomes Status::internal, a   *)
(* refused report Status::failed_precondition.                              *)
Reply(k) ==
    /\ pc[k] \in {"returned", "failed"}
    /\ reply' = [reply EXCEPT ![k] =
                   IF pc[k] = "failed" THEN "Err" ELSE out[k].verdict]
    /\ pc' = [pc EXCEPT ![k] = "done"]
    /\ UNCHANGED <<given, blobs, wr, holder, core, out, trace, injected>>

(* End state: both calls answered. Nothing changes; named so TLC does not   *)
(* report the end as a deadlock.                                            *)
Done ==
    /\ \A k \in Calls : pc[k] = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E k \in Calls : Lock(k)
    \/ \E k \in Calls : Step(k)
    \/ \E k \in Calls : EarlyCommit(k)
    \/ \E k \in Calls : StartWrites(k)
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
    /\ kindOf \in [Calls -> Kinds]
    /\ toolOf \in [Calls -> Tools]
    /\ holder \subseteq Calls
    /\ nextId \in Nat
    /\ clk \in [Slots -> Nat]
    /\ sessionOf \in [Calls -> Sessions]
    /\ useIdOf \in [Calls -> UseIds]
    /\ bound \in [Sessions -> Slots \cup {NoSlot}]
    /\ calls \in [Keys -> [before : Nat, tool : Tools \cup {NoTool}, done : Nat]]
    /\ reply \in [Calls -> {"-", "Allow", "Deny", "Ack", "Refused", "Err"}]
    /\ injected \subseteq Calls

(* (a) Every Allow or Ack the shim got has its event in the trace.          *)
AllowIsDurable ==
    \A k \in Calls :
        reply[k] \in {"Allow", "Ack"} => \E i \in DOMAIN trace : trace[i].call = k

(* A Deny or a refused report records nothing.                              *)
DenyRecordsNothing ==
    \A k \in Calls :
        reply[k] \in {"Deny", "Refused"} => \A i \in DOMAIN trace : trace[i].call # k

(* (b) The ids in the trace, in commit order, are 1, 2, 3, ...: unique,     *)
(* strictly increasing, none skipped.                                       *)
IdsConsecutive ==
    \A i \in DOMAIN trace : trace[i].id = i

(* What the trace says the core must remember about the call c: the id of  *)
(* its BeforeTool row, the tool of its first row, the id of its first      *)
(* AfterTool row (0 for none), or NoCall when no row names it.             *)
RowsOf(c) == {i \in DOMAIN trace : KeyOf(trace[i]) = c}
FirstOf(S) == CHOOSE i \in S : \A j \in S : i <= j
CallFromTrace(c) ==
    LET rows == RowsOf(c)
        befores == {i \in rows : trace[i].kind = "B"}
        afters == {i \in rows : trace[i].kind = "A"}
    IN IF rows = {} THEN NoCall
       ELSE [before |-> IF befores = {} THEN 0 ELSE trace[FirstOf(befores)].id,
             tool |-> trace[FirstOf(rows)].tool,
             done |-> IF afters = {} THEN 0 ELSE trace[FirstOf(afters)].id]

(* (c) Outside the critical section the committed core state is exactly     *)
(* what the trace holds: a failed write left next_event_id, the slot's      *)
(* clock, the session bindings, the recorded calls and their completions    *)
(* where they were, so a retry reuses the same id and stamp, still pairs,   *)
(* and is not refused as a repeat of itself; and the first completion of a  *)
(* call stays its completion.                                               *)
CoreMatchesTrace ==
    Quiet =>
        /\ nextId = Len(trace) + 1
        /\ \A s \in Slots :
             clk[s] = Cardinality({i \in DOMAIN trace : trace[i].slot = s})
        /\ \A c \in Keys : calls[c] = CallFromTrace(c)
        /\ \A x \in Sessions :
             IF \E i \in DOMAIN trace : trace[i].session = x
             THEN \E i \in DOMAIN trace : trace[i].session = x /\ bound[x] = trace[i].slot
             ELSE bound[x] = NoSlot

(* (d) Each slot's stamps strictly increase in commit order. *)
StampsIncrease ==
    \A i, j \in DOMAIN trace :
        (i < j /\ trace[i].slot = trace[j].slot) => trace[i].stamp < trace[j].stamp

(* (f) Every row names blobs that are stored and flushed: its payload, and  *)
(* an AfterTool's input. Blobs are put before their row, so no failure      *)
(* leaves a row without its blobs. Orphan blobs are allowed.                *)
RowsNameStoredBlobs ==
    \A i \in DOMAIN trace :
        /\ trace[i].blob \in blobs
        /\ trace[i].kind = "A" => trace[i].inBlob \in blobs

(* (e) What the mutex buys: two calls never make each other fail. A call    *)
(* gets an Err only when one of its own writes failed for an outside reason.*)
CallsDoNotCollide ==
    \A k \in Calls : reply[k] = "Err" => k \in injected

(* (g) A session stays on one slot: every row of a session names the same   *)
(* slot.                                                                    *)
SessionStaysOnSlot ==
    \A i, j \in DOMAIN trace :
        trace[i].session = trace[j].session => trace[i].slot = trace[j].slot

(* (g) A call has at most one BeforeTool row, and none after a row of its   *)
(* own (a late BeforeTool for an orphan completion is a repeat).            *)
CallRecordedOnce ==
    \A i, j \in DOMAIN trace :
        (i < j /\ KeyOf(trace[i]) = KeyOf(trace[j])) => trace[j].kind # "B"

(* (h) Every row of a call names the call's tool.                           *)
CallKeepsItsTool ==
    \A i, j \in DOMAIN trace :
        KeyOf(trace[i]) = KeyOf(trace[j]) => trace[i].tool = trace[j].tool

(* (i) Every row carries its report's outcome: a failure is never recorded  *)
(* as a success, nor a success as a failure. (The seed row is a BeforeTool.)*)
RowKeepsItsOutcome ==
    \A i \in DOMAIN trace :
        trace[i].call # "seed" => trace[i].outcome = ReportOutcome(trace[i].call)

(* (h) An AfterTool row names its own call's BeforeTool row when there is   *)
(* one, by identity alone, and no row (0) when there is none: never a guess.*)
AfterPairsWithItsBefore ==
    \A i \in DOMAIN trace :
        trace[i].kind = "A" =>
            LET bs == {j \in DOMAIN trace : trace[j].kind = "B"
                                          /\ KeyOf(trace[j]) = KeyOf(trace[i])}
            IN IF bs = {} THEN trace[i].before = 0
               ELSE \E j \in bs : j < i /\ trace[i].before = trace[j].id

-----------------------------------------------------------------------------
(* Witnesses, each expected to be VIOLATED: their violation shows the       *)
(* model reaches the case a rule is about, so a pass is not vacuous.        *)

(* The retry after a failed write reuses the failed call's id and stamp.    *)
RetryReusesIdAndStamp ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ reply[k1] = "Err"
        /\ reply[k2] \in {"Allow", "Ack"}
        /\ \E i \in DOMAIN trace :
             /\ trace[i].call = k2
             /\ trace[i].id = out[k1].id
             /\ trace[i].stamp = out[k1].stamp
WitnessNotReached == ~RetryReusesIdAndStamp

(* A call stored its payload blob, a later write failed, and no row names   *)
(* that blob, which rule (f) allows.                                        *)
OrphanBlobReached ==
    \E k \in Calls :
        /\ reply[k] = "Err"
        /\ PrimaryBlob(k) \in blobs
        /\ \A i \in DOMAIN trace : trace[i].blob # PrimaryBlob(k)
OrphanNotReached == ~OrphanBlobReached

(* A known slot's call refused for its identity or its tool.                *)
IdentityRefusalReached ==
    \E k \in Calls : reply[k] \in {"Deny", "Refused"} /\ slotOf[k] \in Slots
IdentityRefusalNotReached == ~IdentityRefusalReached

(* A call whose write failed, retried with the same session and             *)
(* tool_use_id and recorded.                                                *)
FailedIdentityRetried ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ reply[k1] = "Err"
        /\ reply[k2] \in {"Allow", "Ack"}
        /\ <<sessionOf[k1], useIdOf[k1]>> = <<sessionOf[k2], useIdOf[k2]>>
FailedIdentityNotRetried == ~FailedIdentityRetried

(* A completion report failed, and its retry was recorded with the failed   *)
(* report's id and still paired with the call's BeforeTool.                 *)
FailedCompletionRetriedAndPaired ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ kindOf[k1] = "A" /\ kindOf[k2] = "A"
        /\ reply[k1] = "Err" /\ reply[k2] = "Ack"
        /\ <<sessionOf[k1], useIdOf[k1]>> = <<sessionOf[k2], useIdOf[k2]>>
        /\ \E i \in DOMAIN trace :
             trace[i].call = k2 /\ trace[i].id = out[k1].id /\ trace[i].before # 0
CompletionRetryNotReached == ~FailedCompletionRetriedAndPaired

(* Two AfterTool rows of one call name the same BeforeTool.                 *)
RepeatedCompletionReached ==
    \E i, j \in DOMAIN trace :
        /\ i < j
        /\ trace[i].kind = "A" /\ trace[j].kind = "A"
        /\ KeyOf(trace[i]) = KeyOf(trace[j])
        /\ trace[i].before # 0 /\ trace[i].before = trace[j].before
RepeatedCompletionNotReached == ~RepeatedCompletionReached

(* An AfterTool row with no BeforeTool (an orphan), recorded after another  *)
(* call's event, so a guess would have had something to guess.              *)
OrphanCompletionReached ==
    \E i \in DOMAIN trace : trace[i].kind = "A" /\ trace[i].before = 0 /\ i > 1
OrphanCompletionNotReached == ~OrphanCompletionReached

(* An AfterTool pairs with a BeforeTool that is not the latest event before *)
(* it: overlapping calls completed out of order.                            *)
OutOfOrderPairingReached ==
    \E i, j \in DOMAIN trace :
        /\ j < i
        /\ trace[i].kind = "A" /\ trace[j].kind = "B"
        /\ trace[i].before # 0 /\ trace[j].id > trace[i].before
OutOfOrderPairingNotReached == ~OutOfOrderPairingReached

(* A failure report failed to persist, and its retry was recorded with the  *)
(* failed report's id, as a failure, still paired with the call's           *)
(* BeforeTool.                                                              *)
FailedFailureRetriedAndPaired ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ kindOf[k1] = "F" /\ kindOf[k2] = "F"
        /\ reply[k1] = "Err" /\ reply[k2] = "Ack"
        /\ <<sessionOf[k1], useIdOf[k1]>> = <<sessionOf[k2], useIdOf[k2]>>
        /\ \E i \in DOMAIN trace :
             /\ trace[i].call = k2 /\ trace[i].id = out[k1].id /\ trace[i].before # 0
             /\ trace[i].outcome = "failure"
FailureRetryNotReached == ~FailedFailureRetriedAndPaired

(* Two AfterTool rows of one call with different outcomes (a success after  *)
(* a failure, or a failure after a success), both naming the same           *)
(* BeforeTool, with every call answered: rule (c) then requires the first   *)
(* to stay the call's completion.                                           *)
MixedOutcomesReached ==
    \E i, j \in DOMAIN trace :
        /\ i < j
        /\ trace[i].kind = "A" /\ trace[j].kind = "A"
        /\ KeyOf(trace[i]) = KeyOf(trace[j])
        /\ trace[i].outcome # trace[j].outcome
        /\ trace[i].before # 0 /\ trace[i].before = trace[j].before
        /\ \A k \in Calls : pc[k] = "done"
MixedOutcomesNotReached == ~MixedOutcomesReached

(* A lifecycle report of a session, then an AfterTool of that session that  *)
(* pairs with a BeforeTool recorded before the lifecycle report: a late     *)
(* completion after Stop or SessionEnd still pairs.                         *)
LateCompletionAfterLifecycle ==
    \E b, i, j \in DOMAIN trace :
        /\ b < i /\ i < j
        /\ trace[b].kind = "B" /\ trace[i].kind = "L" /\ trace[j].kind = "A"
        /\ trace[i].session = trace[j].session
        /\ KeyOf(trace[b]) = KeyOf(trace[j])
        /\ trace[j].before = trace[b].id
LateCompletionNotReached == ~LateCompletionAfterLifecycle

(* A lifecycle report failed to persist, and the same session's lifecycle   *)
(* report was then recorded with the failed one's id and stamp.             *)
FailedLifecycleRetried ==
    \E k1, k2 \in Calls :
        /\ k1 # k2
        /\ kindOf[k1] = "L" /\ kindOf[k2] = "L"
        /\ reply[k1] = "Err" /\ reply[k2] = "Ack"
        /\ sessionOf[k1] = sessionOf[k2]
        /\ \E i \in DOMAIN trace :
             /\ trace[i].call = k2 /\ trace[i].id = out[k1].id
             /\ trace[i].stamp = out[k1].stamp
LifecycleRetryNotReached == ~FailedLifecycleRetried

(* After a lifecycle report, another session's event on the same slot takes *)
(* the slot's next stamp: a new session continues its slot's clock.         *)
NewSessionContinuesClock ==
    \E i, j \in DOMAIN trace :
        /\ i < j
        /\ trace[i].kind = "L"
        /\ trace[j].session # trace[i].session
        /\ trace[j].slot = trace[i].slot
        /\ trace[j].stamp = trace[i].stamp + 1
NewSessionNotReached == ~NewSessionContinuesClock

=============================================================================
