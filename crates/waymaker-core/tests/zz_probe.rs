#[test]
fn probe() {
    use waymaker_core::replay::Position;
    use waymaker_core::transition::{EffectRequest, Next, ReplayMachine};
    use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};
    const RUN: RunId = RunId(5);
    let mut m = ReplayMachine::new(RUN);
    println!("0 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    println!("adv RunStarted -> {:?}", m.advance(RecordRef::RunStarted { workflow_kind:1, workflow_version:2, input:b"" }));
    println!("1 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    let sched = RecordRef::EffectScheduled { seq: EffectSeq(0), kind: ActivityKind(7), input_len: 4, input_crc: 0xDEAD_BEEF };
    let bad = EffectRequest { kind: ActivityKind(99), input_len: 4, input_crc: 0xDEAD_BEEF };
    println!("intent(bad, sched0) -> {:?}", m.intent(bad, Next::Record(sched)));
    println!("2 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    let good = EffectRequest { kind: ActivityKind(7), input_len: 4, input_crc: 0xDEAD_BEEF };
    println!("intent(good, EOH) -> {:?}", m.intent(good, Next::EndOfHistory));
    println!("3 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    println!("advance(sched0) -> {:?}", m.advance(sched));
    println!("4 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    println!("outcome(EOH) -> {:?}", m.outcome(Next::EndOfHistory));
    println!("5 {:?} {:?} {:?}", m.position(), m.diverged(), m.pending());
    assert_eq!(m.position(), Position::BeforeRun, "force output");
}
