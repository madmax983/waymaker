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
| `during-completion-write` | During completion write | The torn completion is ignored and no partial result bytes are exposed. The bank has no append point, so the driver refuses it. | Swept |
| `after-completion-barrier` | After completion barrier | The completion is replayed. The activity never runs again. | Swept |
| `during-inactive-bank-erase-or-write` | During inactive-bank erase/write | The old bank stays authoritative and the old run continues. | Owed |
| `after-new-bank-seal-barrier` | After new bank seal barrier | The new bank is authoritative and the old run is never current. | Owed |
| `history-capacity-reached` | History capacity reached | A capacity error with no mutation, or an explicit continue_as_new. | Owed |
| `replay-divergence` | Replay divergence | A deterministic fault. No further execution, and history untouched. | Owed |

## Row 5 does not hold as §14 writes it

§14 says that Waymaker redelivers a torn completion. It cannot. A torn or unsealed tail
leaves no append point. The driver and the rig both refuse the bank, and neither writes past
the damage. A write there is how a NOR bank stops booting for good.

The tests assert the two halves that do hold. Waymaker ignores the torn completion, and no
partial result bytes reach the workflow.

`continue_as_new` would continue the run under a new id. An effect that ran before the crash
would then run again under a different `(RunId, EffectSeq)`. That duplicate is issue
[#95](https://github.com/madmax983/waymaker/issues/95). It is a design and not yet a path:
neither driver here calls `continue_as_new`, so today the run stops.
