# Failure semantics

Design document §14 states ten failure points. Each one is a named test. The `matrix` CI
stage runs the tests on the in-memory model of NOR and on the power-cut rig.

Both halves run on a host. The rig drives `waymaker-fault` rather than a part, and no board
has run either half. See [the hardware compatibility matrix](hardware-matrix.md).

The **rig** column says whether the rig reaches that row. The rig still owes four rows,
because it has no bank-swap workload, no capacity refusal and no divergent replay. Issue
[#96](https://github.com/madmax983/waymaker/issues/96) is where they arrive.

| Id | Failure point | What happens | On the rig |
| --- | --- | --- | --- |
| `during-schedule-frame-write` | During schedule frame write | The frame is ignored. The activity was not yet dispatchable. | Swept |
| `after-schedule-barrier-before-dispatch` | After schedule barrier, before dispatch | The stable effect id is redelivered. | Swept |
| `during-physical-activity` | During physical activity | The effect is redelivered. The activity must tolerate the duplicate attempt. | Swept |
| `after-activity-before-completion-barrier` | After physical activity, before completion barrier | The same id is redelivered. | Swept |
| `during-completion-write` | During completion write | The torn completion is ignored and no partial result bytes are exposed. If the frame's reserved slot is otherwise erased, the run redelivers the effect under its own identity; if a byte of the slot is neither erased nor a real seal, the bank has no append point and is refused. | Swept |
| `after-completion-barrier` | After completion barrier | The completion is replayed. The activity never runs again. | Swept |
| `during-inactive-bank-erase-or-write` | During inactive-bank erase/write | The old bank stays authoritative and the old run continues. | Owed |
| `after-new-bank-seal-barrier` | After new bank seal barrier | The new bank is authoritative and the old run is never current. | Owed |
| `history-capacity-reached` | History capacity reached | A capacity error with no mutation, or an explicit continue_as_new. | Owed |
| `replay-divergence` | Replay divergence | A deterministic fault. No further execution, and history untouched. | Owed |

## Row 5 holds, except where recovery cannot tell an interrupted append from damage

§14 says that Waymaker redelivers a torn completion. Issue
[#95](https://github.com/madmax983/waymaker/issues/95) makes that true, for most torn
completions.

No writer starts a record before the one ahead of it has sealed. So an interrupted write
never touches more than its own reserved slot — the padded frame body, plus one commit-seal
program unit. Recovery checks only that slot's own tail: the bytes from the frame's own
unpadded length to the end of the slot. If every one of those bytes is erased, recovery
ignores the frame and keeps scanning. The run keeps going, and it redelivers the effect
under the identity its schedule record already committed — no `continue_as_new`, and no new
`(RunId, EffectSeq)` for an effect that had already run.

A tear inside the commit seal itself is different. Those bytes are neither erased nor a real
seal, and recovery still cannot tell an interrupted append from damage there. The bank has
no append point and is refused, exactly as before.

The tests sweep both outcomes. See
[ADR 0048](https://github.com/madmax983/waymaker/blob/main/docs/adr/0048-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md)
for the full decision.
