---- MODULE AgentKernel ----
(***************************************************************************)
(* Safety-floor state model for the Mework agent kernel.                  *)
(*                                                                         *)
(* This spec is written from required safety properties, not from the      *)
(* implementation. Any divergence is an implementation defect. The CSP-M  *)
(* protocol in formal/csp/AgentKernel.csp is authoritative for interaction *)
(* structure; ProB validates both layers and Rust-exported traces.         *)
(*                                                                         *)
(* Safety properties:                                                      *)
(*   S1 Approval precedes execution.                                      *)
(*   S2 Each authorization is consumed by one execution.                  *)
(*   S3 Policy tightening invalidates unconsumed authorizations.          *)
(*   S4 Cancellation prevents new requests, grants, executions, task      *)
(*      creation, waits, deliveries, and rounds; in-flight work may close. *)
(*   S5 Idle has no unsettled calls. RoundEnd requires settled calls and   *)
(*      no pending waits; TurnEnd occurs in prep.                          *)
(*   S6 Running tasks do not exceed MaxLiveAgents.                         *)
(*   S7 Only ToolExecEnd moves a call from executing to done.              *)
(*   S8 task_wait is the only model wait mechanism. A wait is initiated,   *)
(*      delivered after task settlement, or withdrawn on timeout while the *)
(*      task continues. Pending waits prevent round closure. Context edits *)
(*      are allowed only while idle.                                       *)
(*   S9 Model actions occur in rounds, host boundary actions in prep.      *)
(*      roundDue records deliverables that must start a later round.       *)
(*      Every folded terminal result triggers a round.                     *)
(*   S10 Asynchronous tasks carry a per-slot task identity. Every result, *)
(*      wait, and fold must use the current identity.                      *)
(*   S11 Tasks survive turn boundaries. Done tasks in idle wait for a      *)
(*      host-initiated TurnStart.                                          *)
(*   S12 An executing call may hand its running work to one free task slot *)
(*      at most once. The handoff mints a fresh identity under the same    *)
(*      capacity limit as a spawn, and the call still settles normally.    *)
(*                                                                         *)
(* Guard-audit variables duplicate action guards over pre-state values.    *)
(* They remain true in the unmodified model and expose weakened guards.    *)
(*                                                                         *)
(* Environment assumptions:                                                *)
(*   A1 Tool classifications come from the trusted host classifier.        *)
(*   A2 The trace exporter serializes per-slot events in causal order and  *)
(*      submits the task identity that corresponds to task parameters.     *)
(*   A3 The host retains one immutable call object from request to settle. *)
(*                                                                         *)
(* Calls and Agents are finite reusable slots. The exporter maps live      *)
(* entities to free slots; traces exceeding the configured concurrency     *)
(* cannot be replayed. TaskIds model adjacent-generation freshness.        *)
(*                                                                         *)
(* This layer verifies safety only. Liveness is covered structurally by    *)
(* NoAdversary deadlock checks in the CSP layer.                            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

Calls == 1..2
Agents == 1..2
MaxLiveAgents == 1
TaskIds == 1..2

CallPhases  == {"unused", "requested", "allowed", "denied", "executing", "execbg", "done"}
AgentPhases == {"none", "running", "done"}
TurnPhases  == {"idle", "running", "cancelling"}
RoundPhases == {"prep", "inround"}
GrantKinds  == {"none", "auto", "user"}

VARIABLES
  turn,         \* Current turn phase: idle / running / cancelling
  round,        \* Round window: prep (between rounds) / inround
  roundDue,     \* S9 deliverable obligation for a subsequent model round
  steerPending, \* Non-empty steer mailbox; pending steer survives turns
  callPhase,    \* Phase of each call slot
  callDanger,   \* Trusted classification result; dangerous calls require approval
  callFresh,    \* Unconsumed grant not invalidated by policy tightening
  callGrant,    \* Grant-source audit: none / auto (ToolAllow) / user (ToolApprove)
  turnLegal,    \* Guard audit for turn and round-boundary actions
  callLegal,    \* Guard audit for call actions in each slot
  agentPhase,   \* Phase of each task slot
  taskRid,      \* S10 current or previous task identity; AgentSpawn rotates it
  agentWaited,  \* S8 pending task wait; initiated by TaskWait and cleared by delivery or cancellation
  wakeFoldPending, \* S11 tasks already done at TurnStart, owed before its first round
  agentLegal    \* Guard audit for task actions in each slot

vars == <<turn, round, roundDue, steerPending, callPhase, callDanger, callFresh,
          callGrant, turnLegal, callLegal, agentPhase, taskRid, agentWaited,
          agentLegal, wakeFoldPending>>

RunningAgents == {a \in Agents : agentPhase[a] = "running"}

TypeOK ==
  /\ turn \in TurnPhases
  /\ round \in RoundPhases
  /\ roundDue \in BOOLEAN
  /\ steerPending \in BOOLEAN
  /\ callPhase \in [Calls -> CallPhases]
  /\ callDanger \in [Calls -> BOOLEAN]
  /\ callFresh \in [Calls -> BOOLEAN]
  /\ callGrant \in [Calls -> GrantKinds]
  /\ turnLegal \in BOOLEAN
  /\ callLegal \in [Calls -> BOOLEAN]
  /\ agentPhase \in [Agents -> AgentPhases]
  /\ taskRid \in [Agents -> TaskIds]
  /\ agentWaited \in [Agents -> BOOLEAN]
  /\ wakeFoldPending \subseteq Agents
  /\ agentLegal \in [Agents -> BOOLEAN]

Init ==
  /\ turn = "idle"
  /\ round = "prep"
  /\ roundDue = FALSE
  /\ steerPending = FALSE
  /\ callPhase = [c \in Calls |-> "unused"]
  /\ callDanger = [c \in Calls |-> FALSE]
  /\ callFresh = [c \in Calls |-> FALSE]
  /\ callGrant = [c \in Calls |-> "none"]
  /\ turnLegal = TRUE
  /\ callLegal = [c \in Calls |-> TRUE]
  /\ agentPhase = [a \in Agents |-> "none"]
  /\ taskRid = [a \in Agents |-> 1]
  /\ agentWaited = [a \in Agents |-> FALSE]
  /\ wakeFoldPending = {}
  /\ agentLegal = [a \in Agents |-> TRUE]

(***************************************************************************)
(* Turn lifecycle                                                          *)
(***************************************************************************)

(* S9: TurnStart creates a delivery obligation for the first round. Wake   *)
(* turns that deliver completed idle tasks also use TurnStart; after prep  *)
(* folds them, the first round receives the result. The snapshot is fixed  *)
(* at TurnStart: later completions do not add a first-round obligation.    *)
TurnStart ==
  /\ turn = "idle"
  /\ turn' = "running"
  /\ roundDue' = TRUE
  /\ turnLegal' = (turnLegal /\ turn = "idle")
  /\ wakeFoldPending' = {a \in Agents : agentPhase[a] = "done"}
  /\ UNCHANGED <<round, steerPending, callPhase, callDanger, callFresh, callGrant,
                 callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* Cancellation does not clear roundDue. TurnEnd discards the obligation  *)
(* on the cancellation path, while RoundStart's turn guard independently  *)
(* prevents new rounds. Cancellation clears every pending wait but does not *)
(* interrupt tasks; running tasks survive closure and done tasks remain for *)
(* the next turn.                                                         *)
TurnCancel ==
  /\ turn = "running"
  /\ turn' = "cancelling"
  /\ agentWaited' = [a \in Agents |-> FALSE]
  /\ turnLegal' = (turnLegal /\ turn = "running")
  /\ UNCHANGED <<wakeFoldPending, round, roundDue, steerPending, callPhase, callDanger, callFresh,
                 callGrant, callLegal, agentPhase, taskRid, agentLegal>>

(* S5+S9+S11: TurnEnd requires prep, whose RoundEnd guarantee clears calls. *)
(* Running tasks survive idle. Done tasks must be folded or delivered except *)
(* on cancellation, when they survive for the next turn. Normal closure also *)
(* requires no delivery obligation or pending steer. Cancellation discards   *)
(* delivery and retains pending steer.                                       *)
TurnEnd ==
  /\ turn \in {"running", "cancelling"}
  /\ round = "prep"
  /\ \A a \in Agents : \/ agentPhase[a] \in {"none", "running"}
                       \/ (turn = "cancelling" /\ agentPhase[a] = "done")
  /\ (turn = "cancelling" \/ (~roundDue /\ ~steerPending))
  /\ turn' = "idle"
  /\ roundDue' = FALSE
  /\ turnLegal' = (turnLegal
                   /\ turn \in {"running", "cancelling"}
                   /\ round = "prep"
                   /\ (\A a \in Agents : \/ agentPhase[a] \in {"none", "running"}
                                         \/ (turn = "cancelling" /\ agentPhase[a] = "done"))
                   /\ (turn = "cancelling" \/ (~roundDue /\ ~steerPending)))
  /\ UNCHANGED <<wakeFoldPending, round, steerPending, callPhase, callDanger, callFresh, callGrant,
                 callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S8: Context edits are allowed only between turns. Persisted data is     *)
(* authoritative and a running turn reads its initial projection. Edit     *)
(* content is outside this protocol's state because its format is irrelevant. *)
ContextEdit ==
  /\ turn = "idle"
  /\ turnLegal' = (turnLegal /\ turn = "idle")
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callPhase, callDanger,
                 callFresh, callGrant, callLegal, agentPhase, taskRid,
                 agentWaited, agentLegal>>

(***************************************************************************)
(* Round lifecycle and steer (S9)                                         *)
(***************************************************************************)

(* S4+S9: Only a running turn may start a round from prep. A delivery      *)
(* obligation is required to prevent empty rounds, and pending steer must  *)
(* join first. Starting the round consumes the obligation.                 *)
RoundStart ==
  /\ turn = "running"
  /\ wakeFoldPending = {}
  /\ round = "prep"
  /\ roundDue
  /\ ~steerPending
  /\ round' = "inround"
  /\ roundDue' = FALSE
  /\ turnLegal' = (turnLegal /\ turn = "running" /\ wakeFoldPending = {}
                   /\ round = "prep" /\ roundDue /\ ~steerPending)
  /\ UNCHANGED <<wakeFoldPending, turn, steerPending, callPhase, callDanger, callFresh, callGrant,
                 callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S5+S8: A round closes only with settled calls and no pending waits.     *)
(* This also closes an in-flight round during cancellation, whose wait     *)
(* invalidation satisfies the same condition.                              *)
RoundEnd ==
  /\ round = "inround"
  /\ \A c \in Calls : callPhase[c] = "unused"
  /\ \A a \in Agents : ~agentWaited[a]
  /\ round' = "prep"
  /\ turnLegal' = (turnLegal /\ round = "inround"
                   /\ (\A c \in Calls : callPhase[c] = "unused")
                   /\ (\A a \in Agents : ~agentWaited[a]))
  /\ UNCHANGED <<wakeFoldPending, turn, roundDue, steerPending, callPhase, callDanger, callFresh,
                 callGrant, callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S9: Steer is enqueued only during an active turn. Idle user input uses  *)
(* the ordinary message path.                                              *)
SteerEnqueue ==
  /\ turn \in {"running", "cancelling"}
  /\ steerPending' = TRUE
  /\ turnLegal' = (turnLegal /\ turn \in {"running", "cancelling"})
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, callPhase, callDanger, callFresh,
                 callGrant, callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S9: Steer joins only in running prep. Joining creates a delivery       *)
(* obligation because the content must reach the model.                    *)
SteerJoin ==
  /\ turn = "running"
  /\ round = "prep"
  /\ steerPending
  /\ steerPending' = FALSE
  /\ roundDue' = TRUE
  /\ turnLegal' = (turnLegal /\ turn = "running" /\ round = "prep" /\ steerPending)
  /\ UNCHANGED <<wakeFoldPending, turn, round, callPhase, callDanger, callFresh, callGrant,
                 callLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(***************************************************************************)
(* Tool-call lifecycle (S10: synchronous calls have no task identity)     *)
(***************************************************************************)

(* S4+S9: No requests are accepted while cancelling. Requests are model    *)
(* actions and occur only in rounds. d is the trusted classification.      *)
ToolRequest(c, d) ==
  /\ turn = "running"
  /\ round = "inround"
  /\ callPhase[c] = "unused"
  /\ callPhase' = [callPhase EXCEPT ![c] = "requested"]
  /\ callDanger' = [callDanger EXCEPT ![c] = d]
  /\ callFresh' = [callFresh EXCEPT ![c] = FALSE]
  /\ callGrant' = [callGrant EXCEPT ![c] = "none"]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ round = "inround"
       /\ callPhase[c] = "unused"]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, turnLegal, agentPhase,
                 taskRid, agentWaited, agentLegal>>

(* S1: Non-dangerous calls receive automatic authorization. An allowed call *)
(* may be reauthorized after policy tightening. InvCallsInRound guarantees *)
(* that requested and allowed calls exist only in a round.                  *)
ToolAllow(c) ==
  /\ turn = "running"
  /\ callPhase[c] \in {"requested", "allowed"}
  /\ ~callDanger[c]
  /\ callPhase' = [callPhase EXCEPT ![c] = "allowed"]
  /\ callFresh' = [callFresh EXCEPT ![c] = TRUE]
  /\ callGrant' = [callGrant EXCEPT ![c] = "auto"]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ callPhase[c] \in {"requested", "allowed"}
       /\ ~callDanger[c]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, turnLegal,
                 agentPhase, taskRid, agentWaited, agentLegal>>

(* S1: Dangerous calls require user approval and may be reapproved from    *)
(* allowed. A3 ensures that approved parameters are the executed ones.     *)
ToolApprove(c) ==
  /\ turn = "running"
  /\ callPhase[c] \in {"requested", "allowed"}
  /\ callDanger[c]
  /\ callPhase' = [callPhase EXCEPT ![c] = "allowed"]
  /\ callFresh' = [callFresh EXCEPT ![c] = TRUE]
  /\ callGrant' = [callGrant EXCEPT ![c] = "user"]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ callPhase[c] \in {"requested", "allowed"}
       /\ callDanger[c]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, turnLegal,
                 agentPhase, taskRid, agentWaited, agentLegal>>

ToolDeny(c) ==
  /\ turn = "running"
  /\ callPhase[c] = "requested"
  /\ callPhase' = [callPhase EXCEPT ![c] = "denied"]
  /\ callFresh' = [callFresh EXCEPT ![c] = FALSE]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ callPhase[c] = "requested"]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, callGrant,
                 turnLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S3: Policy tightening invalidates every unconsumed grant or approval.   *)
(* It is unguarded and may occur in every phase.                           *)
PolicyTighten ==
  /\ callFresh' = [c \in Calls |-> FALSE]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callPhase, callDanger,
                 callGrant, turnLegal, callLegal, agentPhase, taskRid,
                 agentWaited, agentLegal>>

(* S1+S2+S4: Starting execution consumes a fresh grant and requires a      *)
(* running turn. InvFreshOnlyAllowed structurally confines freshness to    *)
(* allowed calls, so no redundant phase guard is added.                    *)
ToolExecStart(c) ==
  /\ turn = "running"
  /\ callFresh[c]
  /\ callPhase' = [callPhase EXCEPT ![c] = "executing"]
  /\ callFresh' = [callFresh EXCEPT ![c] = FALSE]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ callFresh[c]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, callGrant,
                 turnLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S7+S12: Only this action moves a call out of execution. In-flight        *)
(* execution may finish while cancelling. A call that handed its work to a  *)
(* task slot is still executing for receipt purposes, so it leaves through  *)
(* the same door rather than a second one.                                  *)
ToolExecEnd(c) ==
  /\ callPhase[c] \in {"executing", "execbg"}
  /\ callPhase' = [callPhase EXCEPT ![c] = "done"]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ callPhase[c] \in {"executing", "execbg"}]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, callFresh,
                 callGrant, turnLegal, agentPhase, taskRid, agentWaited, agentLegal>>

(* S6+S9+S10+S12: A deadline does not abandon running work. An executing    *)
(* call hands its process to an empty task slot, which mints a fresh        *)
(* identity under the same capacity limit a spawn obeys. The call moves to  *)
(* execbg, which is executing minus the right to hand off again, so one     *)
(* call can create at most one task. No delivery obligation is charged here *)
(* because the call's own ToolSettle charges one for the receipt that names *)
(* the task. InvCallsInRound structurally confines an executing call to a   *)
(* round, so no redundant round guard is added; the turn guard is real      *)
(* because a call may still be executing while cancelling.                  *)
ToolBackground(c, a, r) ==
  /\ turn = "running"
  /\ callPhase[c] = "executing"
  /\ Cardinality(RunningAgents) < MaxLiveAgents
  /\ agentPhase[a] = "none"
  /\ r # taskRid[a]
  /\ callPhase' = [callPhase EXCEPT ![c] = "execbg"]
  /\ agentPhase' = [agentPhase EXCEPT ![a] = "running"]
  /\ taskRid' = [taskRid EXCEPT ![a] = r]
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ turn = "running" /\ callPhase[c] = "executing"]
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ Cardinality(RunningAgents) < MaxLiveAgents
       /\ agentPhase[a] = "none" /\ r # taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callDanger, callFresh,
                 callGrant, turnLegal, agentWaited>>

(* S5: Reclaim settled calls at any time. Unsettled calls may be discarded *)
(* only while cancelling. S9: settlement creates a delivery obligation;   *)
(* this is harmless on the cancellation path because TurnEnd ignores it.  *)
ToolSettle(c) ==
  /\ \/ callPhase[c] \in {"done", "denied"}
     \/ /\ callPhase[c] \in {"requested", "allowed"}
        /\ turn = "cancelling"
  /\ callPhase' = [callPhase EXCEPT ![c] = "unused"]
  /\ callFresh' = [callFresh EXCEPT ![c] = FALSE]
  /\ callGrant' = [callGrant EXCEPT ![c] = "none"]
  /\ roundDue' = TRUE
  /\ callLegal' = [callLegal EXCEPT ![c] =
       callLegal[c] /\ (callPhase[c] \in {"done", "denied"}
                        \/ (callPhase[c] \in {"requested", "allowed"} /\ turn = "cancelling"))]
  /\ UNCHANGED <<wakeFoldPending, turn, round, steerPending, callDanger, turnLegal, agentPhase,
                 taskRid, agentWaited, agentLegal>>

(***************************************************************************)
(* Task lifecycle (S10: asynchronous tasks carry task identities)         *)
(***************************************************************************)

(* S4+S6+S9+S10: Tasks spawn only in a running round within capacity. The  *)
(* new identity differs from the preceding generation in this slot, and    *)
(* spawn confirmation creates a delivery obligation.                        *)
AgentSpawn(a, r) ==
  /\ turn = "running"
  /\ round = "inround"
  /\ agentPhase[a] = "none"
  /\ Cardinality(RunningAgents) < MaxLiveAgents
  /\ r # taskRid[a]
  /\ agentPhase' = [agentPhase EXCEPT ![a] = "running"]
  /\ taskRid' = [taskRid EXCEPT ![a] = r]
  /\ roundDue' = TRUE
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ turn = "running" /\ round = "inround"
       /\ agentPhase[a] = "none"
       /\ Cardinality(RunningAgents) < MaxLiveAgents
       /\ r # taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, steerPending, callPhase, callDanger, callFresh,
                 callGrant, turnLegal, callLegal, agentWaited>>

(* S10+S11: Completion must use the current identity, preventing a late    *)
(* previous generation from completing a replacement. A task may complete  *)
(* while idle because it survives turn boundaries. Completion is the only  *)
(* exit from running, whatever ended the task: the model finishing, a host *)
(* failure, or the user closing it. The outcome word is host result data,  *)
(* not a kernel phase, so no terminal result can bypass delivery.          *)
AgentComplete(a, r) ==
  /\ agentPhase[a] = "running"
  /\ r = taskRid[a]
  /\ agentPhase' = [agentPhase EXCEPT ![a] = "done"]
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ agentPhase[a] = "running" /\ r = taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callPhase, callDanger,
                 callFresh, callGrant, turnLegal, callLegal, taskRid, agentWaited>>

(* S9+S10: Fold pushes an unwaited done result to the model in prep and    *)
(* creates a delivery obligation for the next round. The host transport is *)
(* outside this model and does not affect its guards. Fold applies only to *)
(* done tasks and is forbidden while idle; idle results wait for TurnStart. *)
(* Pending waits cannot enter prep by InvWaitPending.                       *)
AgentFold(a, r) ==
  /\ turn = "running"
  /\ round = "prep"
  /\ agentPhase[a] = "done"
  /\ r = taskRid[a]
  /\ agentPhase' = [agentPhase EXCEPT ![a] = "none"]
  /\ roundDue' = TRUE
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ turn = "running" /\ round = "prep"
       /\ agentPhase[a] = "done" /\ r = taskRid[a]]
  /\ wakeFoldPending' = wakeFoldPending \ {a}
  /\ UNCHANGED <<turn, round, steerPending, callPhase, callDanger, callFresh,
                 callGrant, turnLegal, callLegal, taskRid, agentWaited>>

(* S8+S10: A model initiates a wait only in a running round for its current *)
(* nonempty task identity, with at most one pending wait per task. Waiting *)
(* for running tasks blocks until settlement; settled tasks may deliver     *)
(* immediately. Initiation creates no delivery obligation.                 *)
TaskWait(a, r) ==
  /\ turn = "running"
  /\ round = "inround"
  /\ agentPhase[a] # "none"
  /\ ~agentWaited[a]
  /\ r = taskRid[a]
  /\ agentWaited' = [agentWaited EXCEPT ![a] = TRUE]
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ turn = "running" /\ round = "inround"
       /\ agentPhase[a] # "none" /\ ~agentWaited[a] /\ r = taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, roundDue, steerPending, callPhase, callDanger,
                 callFresh, callGrant, turnLegal, callLegal, agentPhase, taskRid>>

(* S8+S10: Timeout withdraws a pending wait for a still-running task. A    *)
(* settled task wait must deliver rather than time out. One task_wait waits *)
(* for all named tasks or its deadline; another task's delivery does not    *)
(* withdraw this wait. Timeout creates a delivery obligation.               *)
TaskWaitTimeout(a, r) ==
  /\ agentWaited[a]
  /\ agentPhase[a] = "running"
  /\ r = taskRid[a]
  /\ roundDue' = TRUE
  /\ agentWaited' = [agentWaited EXCEPT ![a] = FALSE]
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ agentWaited[a] /\ agentPhase[a] = "running"
       /\ r = taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, steerPending, callPhase, callDanger, callFresh,
                 callGrant, turnLegal, callLegal, agentPhase, taskRid>>

(* S8+S10: Delivery requires a pending wait, a settled task, and its       *)
(* current identity. Delivery creates a round obligation. InvWaitPending   *)
(* structurally provides the otherwise redundant turn and round guards.    *)
TaskWaitDeliver(a, r) ==
  /\ agentWaited[a]
  /\ agentPhase[a] = "done"
  /\ r = taskRid[a]
  /\ agentPhase' = [agentPhase EXCEPT ![a] = "none"]
  /\ agentWaited' = [agentWaited EXCEPT ![a] = FALSE]
  /\ roundDue' = TRUE
  /\ agentLegal' = [agentLegal EXCEPT ![a] =
       agentLegal[a] /\ agentWaited[a]
       /\ agentPhase[a] = "done" /\ r = taskRid[a]]
  /\ UNCHANGED <<wakeFoldPending, turn, round, steerPending, callPhase, callDanger, callFresh,
                 callGrant, turnLegal, callLegal, taskRid>>

(***************************************************************************)
(* Transition relation                                                     *)
(***************************************************************************)

Next ==
  \/ TurnStart
  \/ TurnCancel
  \/ TurnEnd
  \/ ContextEdit
  \/ RoundStart
  \/ RoundEnd
  \/ SteerEnqueue
  \/ SteerJoin
  \/ PolicyTighten
  \/ \E c \in Calls, d \in BOOLEAN : ToolRequest(c, d)
  \/ \E c \in Calls : ToolAllow(c)
  \/ \E c \in Calls : ToolApprove(c)
  \/ \E c \in Calls : ToolDeny(c)
  \/ \E c \in Calls : ToolExecStart(c)
  \/ \E c \in Calls : ToolExecEnd(c)
  \/ \E c \in Calls : ToolSettle(c)
  \/ \E c \in Calls, a \in Agents, r \in TaskIds : ToolBackground(c, a, r)
  \/ \E a \in Agents, r \in TaskIds : AgentSpawn(a, r)
  \/ \E a \in Agents, r \in TaskIds : AgentComplete(a, r)
  \/ \E a \in Agents, r \in TaskIds : AgentFold(a, r)
  \/ \E a \in Agents, r \in TaskIds : TaskWait(a, r)
  \/ \E a \in Agents, r \in TaskIds : TaskWaitTimeout(a, r)
  \/ \E a \in Agents, r \in TaskIds : TaskWaitDeliver(a, r)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Invariants                                                              *)
(***************************************************************************)

(* Guard audits remain true. Weakening any action guard causes the first   *)
(* illegal action to violate its corresponding audit invariant.            *)
InvTurnAudit  == turnLegal
InvCallAudit  == \A c \in Calls : callLegal[c]
InvAgentAudit == \A a \in Agents : agentLegal[a]

(* S5+S9+S11: Idle has no active calls, a prep round window, and no        *)
(* delivery obligation. Pending steer may survive turns; tasks may survive *)
(* idle and are therefore excluded.                                        *)
InvQuiescentIdle ==
  turn = "idle" =>
    /\ \A c \in Calls : callPhase[c] = "unused"
    /\ round = "prep"
    /\ ~roundDue

(* S9 structural projection: the complete call pipeline lives in a round; *)
(* call slots are empty during prep.                                       *)
InvCallsInRound ==
  \A c \in Calls : callPhase[c] # "unused" => round = "inround"

(* S8 structural projection: pending waits exist only in a running round    *)
(* for an existing task. RoundEnd blocks them, TurnCancel clears them, and *)
(* delivery clears them, supporting TaskWaitDeliver's lack of redundant    *)
(* turn and round guards.                                                   *)
InvWaitPending ==
  \A a \in Agents :
    agentWaited[a] => /\ turn = "running"
                      /\ round = "inround"
                      /\ agentPhase[a] # "none"

(* S6: Concurrency limit. *)
InvCapacity == Cardinality(RunningAgents) <= MaxLiveAgents

(* S1 audit projection: executing dangerous calls hold user approval, and  *)
(* executing non-dangerous calls hold automatic authorization. A handed-off *)
(* call is still executing work it was authorized for, so it is included.  *)
InvGrantMatchesDanger ==
  \A c \in Calls :
    callPhase[c] \in {"executing", "execbg"} =>
      /\ callDanger[c] => callGrant[c] = "user"
      /\ ~callDanger[c] => callGrant[c] = "auto"

(* S2 structural projection: freshness exists only in allowed calls;       *)
(* execution and policy tightening clear it immediately.                   *)
InvFreshOnlyAllowed == \A c \in Calls : callFresh[c] => callPhase[c] = "allowed"

(* S4/S5 readable consequence of InvQuiescentIdle: in-flight calls exist  *)
(* only during active turns. Tasks may survive idle and are excluded.      *)
InvWorkInTurn ==
  \A c \in Calls : callPhase[c] \in {"executing", "execbg"} => turn # "idle"

====
