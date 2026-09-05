//! §10's capacity reserve, driven against media that records what it was asked.
//!
//! Design document §10 "Capacity is explicit": "Waymaker reserves enough tail space for a
//! terminal record or `continue_as_new`. Ordinary effect scheduling fails early with
//! `HistoryNearCapacity`; the runtime never overwrites committed history to make room."
//! Issue [#25](https://github.com/madmax983/waymaker/issues/25) asks for three things to be
//! shown rather than argued, and each has a test named after it below: a journal filled to
//! the boundary refuses a schedule while a terminal record still commits, `continue_as_new`
//! still succeeds from that state, and the refusal moves no byte and asks the device for
//! nothing.
//!
//! The fourth test here is the one that says the reserve is the *right* reserve.
//! [`a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding`] drives the tempting
//! arithmetic — keep room for the terminal record and nothing else — and watches a run reach
//! a state from which it can never write one, because §08's transition table has no edge
//! from an unresolved effect to a terminal record.

use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};
use waymaker_flash::append::{AppendError, Journal, WriteAmplification};
use waymaker_flash::bank::{self, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::capacity::{Bounds, CapacityError, Refusal, Reserve, Reserved, ReservedError};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

// ---------------------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------------------

/// What a writer asked of the device, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Program { offset: u32, len: u32 },
    Erase { offset: u32, len: u32 },
    Barrier,
}

/// NOR-shaped media that records the sequence of mutations it was asked for.
///
/// Erased is `0xFF` and a program only ever clears bits, so "the failure produces no
/// mutation at all" can be checked two independent ways: the image is byte-identical, and
/// the device was never asked for anything.
struct Nor {
    geometry: Geometry,
    media: Vec<u8>,
    ops: Vec<Op>,
}

impl Nor {
    fn new(geometry: Geometry) -> Self {
        let Ok(capacity) = usize::try_from(geometry.capacity()) else {
            unreachable!("a host holds any capacity this file describes")
        };
        Self {
            geometry,
            media: std::vec![ERASED_BYTE; capacity],
            ops: Vec::new(),
        }
    }

    /// The whole image, copied out so a later mutation cannot change it.
    fn snapshot(&self) -> Vec<u8> {
        self.media.clone()
    }

    /// `len` bytes at `at`, copied out for the same reason.
    fn window(&self, at: u32, len: u32) -> Vec<u8> {
        let (Ok(start), Ok(width)) = (usize::try_from(at), usize::try_from(len)) else {
            unreachable!("a host holds any offset this file describes")
        };
        let Some(bytes) = start
            .checked_add(width)
            .and_then(|end| self.media.get(start..end))
        else {
            unreachable!("the windows in this file are inside the device")
        };
        bytes.to_vec()
    }
}

impl StableStorage for Nor {
    type Error = GeometryError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        let end = start
            .checked_add(dst.len())
            .ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(
            self.media
                .get(start..end)
                .ok_or(GeometryError::OutOfBounds)?,
        );
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_program(offset, len)?;
        self.ops.push(Op::Program { offset, len });
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        for (index, wanted) in src.iter().enumerate() {
            let Some(cell) = start
                .checked_add(index)
                .and_then(|at| self.media.get_mut(at))
            else {
                return Err(GeometryError::OutOfBounds);
            };
            // Flash: a program clears bits and never sets them.
            *cell &= *wanted;
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        self.ops.push(Op::Erase { offset, len });
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        let end = start
            .checked_add(usize::try_from(len).map_err(|_| GeometryError::OutOfBounds)?)
            .ok_or(GeometryError::OutOfBounds)?;
        self.media
            .get_mut(start..end)
            .ok_or(GeometryError::OutOfBounds)?
            .fill(ERASED_BYTE);
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.ops.push(Op::Barrier);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------

const PAGE: usize = 512;

/// The run whose history these banks hold.
const RUN: RunId = RunId(0x0123_4567_89AB_CDEF);

/// The activity the workflow schedules.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// The bounds every reserve in this file is computed from.
///
/// Small on purpose. §04's budgets make a real device's bounds small, and a fixture whose
/// reserve is a rounding error against its journal would never reach the boundary the tests
/// below are about.
/// An effect result is deliberately larger than a terminal one, because that is the shape in
/// which the tempting reserve is wrong: with the two equal, keeping room for a terminal
/// record happens to keep room for an outcome as well, and
/// [`a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding`] would have nothing to
/// show. An activity that returns more than a workflow does is also the ordinary case.
const BOUNDS: Bounds = Bounds {
    run_input_bytes: 32,
    effect_result_bytes: 32,
    terminal_bytes: 16,
};

/// A terminal record's result, at exactly [`Bounds::terminal_bytes`].
const TERMINAL_RESULT: [u8; 16] = [0xA5; 16];

/// An effect outcome's result, at exactly [`Bounds::effect_result_bytes`].
const EFFECT_RESULT: [u8; 32] = [0x5A; 32];

/// The next run's input, at exactly [`Bounds::run_input_bytes`].
const NEXT_RUN_INPUT: [u8; 32] = [0x3C; 32];

/// This run's input, which is what the bank header on media carries.
const RUN_INPUT: &[u8] = b"run-input";

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192 is two whole 4096-byte blocks of whole 8-byte units of bytes")
    };
    geometry
}

fn layout() -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("two erase blocks are two banks")
    };
    layout
}

fn align() -> ProgramAlign {
    layout().align()
}

fn reserve() -> Reserve {
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout()) else {
        unreachable!("these bounds fit the banks this file describes")
    };
    reserve
}

fn header() -> BankHeader<'static> {
    BankHeader {
        run: RUN,
        align: align(),
        workflow_kind: 0x0042,
        workflow_version: 3,
        input_schema: 1,
        input: RUN_INPUT,
    }
}

const fn schedule(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(seq),
        kind: DOWNLOAD,
        input_len: 4,
        input_crc: 0x0BAD_F00D,
    }
}

const fn outcome(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectCompleted {
        seq: EffectSeq(seq),
        result: &EFFECT_RESULT,
    }
}

const fn opening() -> RecordRef<'static> {
    RecordRef::RunStarted {
        workflow_kind: 0x0042,
        workflow_version: 3,
        input: &NEXT_RUN_INPUT,
    }
}

const fn terminal() -> RecordRef<'static> {
    RecordRef::RunCompleted {
        result: &TERMINAL_RESULT,
    }
}

/// Bytes `record` occupies on media at this file's granularity, seal included.
fn width(record: &RecordRef<'_>) -> u32 {
    let Ok(bytes) = frame::encoded_len(record, align()) else {
        unreachable!("the records in this file encode")
    };
    let Ok(width) = u32::try_from(bytes) else {
        unreachable!("a frame is shorter than a bank")
    };
    width
}

/// Writes a bank header and its generation seal, and hands back the journal behind them.
///
/// §10's steps 3 to 6, done by hand because the writer that will do them is issue #26's.
fn install(device: &mut Nor, id: BankId, generation: Generation, input: &[u8]) -> JournalRegion {
    install_on(device, layout(), id, generation, input)
}

/// [`install`] on a layout other than this file's default.
fn install_on(
    device: &mut Nor,
    layout: BankLayout,
    id: BankId,
    generation: Generation,
    input: &[u8],
) -> JournalRegion {
    let region = layout.bank(id);
    let mut staging = [0_u8; PAGE];
    let head = BankHeader {
        align: layout.align(),
        input,
        ..header()
    };
    let Ok(header_len) = bank::encode_header(&head, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let Ok(()) = device.program(region.base(), header_frame) else {
        unreachable!("a bank header is a legal program")
    };
    let Ok(()) = device.barrier() else {
        unreachable!("this device's barrier does not fail")
    };
    let Ok(seal) = bank::seal_for(header_frame, generation) else {
        unreachable!("a header frame can be sealed")
    };
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, layout.align(), &mut seal_bytes) else {
        unreachable!("a seal fits its own region")
    };
    let Some(sealed) = seal_bytes.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let Ok(()) = device.program(region.seal_offset(), sealed) else {
        unreachable!("a generation seal is a legal program")
    };
    let Ok(()) = device.barrier() else {
        unreachable!("this device's barrier does not fail")
    };
    let Ok(journal) = JournalRegion::of(layout, id, &head) else {
        unreachable!("this bank has room for a journal")
    };
    journal
}

/// The generation a bank on media reports, read back the way a cold boot has to.
fn sealed_generation(device: &mut Nor, id: BankId) -> Option<Generation> {
    let region = layout().bank(id);
    let mut page = [0_u8; PAGE];
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = usize::try_from(region.seal_bytes()) else {
        unreachable!("a host holds a seal")
    };
    let (Some(head), Some(tail)) = (page.get_mut(..128), seal_bytes.get_mut(..seal_len)) else {
        unreachable!("the buffers in this file are larger than a header and a seal")
    };
    let (Ok(()), Ok(())) = (
        device.read(region.base(), head),
        device.read(region.seal_offset(), tail),
    ) else {
        unreachable!("a bank's header and seal are inside the device")
    };
    bank::sealed_generation(head, tail)
}

/// A writer over `region`, positioned where a recovery of it says it may write.
fn opened(device: &mut Nor, region: JournalRegion) -> Journal {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    while recovery.next(device, &mut page).is_some() {}
    let Some(journal) = Journal::after(recovery) else {
        unreachable!("a journal that ends cleanly has an append point")
    };
    journal
}

/// A reserved writer over `region`.
fn reserved(device: &mut Nor, region: JournalRegion) -> Reserved {
    gated(device, region, reserve())
}

/// A writer over `region`, gated by `reserve`.
fn gated(device: &mut Nor, region: JournalRegion, reserve: Reserve) -> Reserved {
    let Ok(writer) = Reserved::over(opened(device, region), reserve) else {
        unreachable!("this reserve was priced for this journal")
    };
    writer
}

/// Writes `record` through the whole protocol, reserve first.
fn commit(
    writer: &mut Reserved,
    device: &mut Nor,
    record: &RecordRef<'_>,
) -> Result<WriteAmplification, ReservedError<GeometryError>> {
    let mut page = [0_u8; PAGE];
    writer
        .stage(device, record, &mut page)?
        .payload_barrier(device)
        .map_err(ReservedError::Append)?
        .commit(device)
        .map_err(ReservedError::Append)
}

/// Writes `record` through the whole protocol with no reserve at all.
fn commit_unreserved(
    journal: &mut Journal,
    device: &mut Nor,
    record: &RecordRef<'_>,
) -> Result<WriteAmplification, AppendError<GeometryError>> {
    let mut page = [0_u8; PAGE];
    journal
        .stage(device, record, &mut page)?
        .payload_barrier(device)?
        .commit(device)
}

/// Fills `writer` with schedule/outcome pairs until a schedule is refused.
///
/// Returns how many whole effects were committed. Every outcome is required to succeed,
/// which is the reserve's central promise: an admitted schedule is one whose outcome is
/// already paid for.
fn fill_to_the_boundary(writer: &mut Reserved, device: &mut Nor) -> u32 {
    let mut committed = 0_u32;
    loop {
        match commit(writer, device, &schedule(committed)) {
            Ok(_) => {}
            Err(ReservedError::Capacity(Refusal::NearCapacity)) => return committed,
            Err(other) => unreachable!("a schedule was refused for {other:?}"),
        }
        let Ok(_) = commit(writer, device, &outcome(committed)) else {
            unreachable!("an admitted schedule reserves room for its own outcome")
        };
        committed = committed.saturating_add(1);
    }
}

// ---------------------------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------------------------

#[test]
fn a_reserve_is_the_worst_case_outcome_and_the_worst_case_terminal_record() {
    // The one number §10 turns on, computed from the bounds and pinned against the codec
    // rather than against itself: `Reserve` computes frame sizes from a payload length and
    // `frame::encoded_len` computes them from a record, and the two would each be perfectly
    // self-consistent while disagreeing about padding.
    let worst_outcome = RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: &EFFECT_RESULT,
    };
    let worst_terminal = RecordRef::RunFailed {
        error: &TERMINAL_RESULT,
    };

    assert_eq!(
        reserve().tail_bytes(),
        width(&worst_outcome) + width(&worst_terminal),
        "the tail is what an outstanding effect still owes: its outcome, then a terminal record"
    );
    // The numbers themselves, so a change to the frame's overhead or to the padding shows up
    // here as well as in the identity above.
    assert_eq!(width(&worst_outcome), 56);
    assert_eq!(width(&worst_terminal), 40);
    assert_eq!(reserve().tail_bytes(), 96);
}

#[test]
fn a_reserve_carries_what_continue_as_new_costs_the_bank_it_rolls_into() {
    // §10 step 3 writes the new bank header, and the reserve is computed from its worst case
    // for the configured bounds. It is not part of the *tail*: the header goes to the
    // inactive bank, so a journal at its reserve boundary has already paid for it.
    let head = BankHeader {
        input: &NEXT_RUN_INPUT,
        ..header()
    };
    let Some(padded) = head.journal_offset() else {
        unreachable!("a header this size has a journal offset")
    };
    assert_eq!(reserve().rollover_bytes(), 64);
    assert_eq!(
        u32::try_from(padded),
        Ok(reserve().rollover_bytes()),
        "the roll-over figure is the padded header frame a continue_as_new writes"
    );
}

#[test]
fn what_a_record_still_owes_is_a_function_of_its_kind_alone() {
    // The exit cost §14 makes load-bearing. A schedule owes an outcome *and* a terminal
    // record, because §08's transition table has no edge from an unresolved effect to a
    // terminal one; an outcome owes only the terminal record; a terminal record owes
    // nothing.
    let reserve = reserve();
    let outcome_width = width(&outcome(0));
    let terminal_width = width(&terminal());

    assert_eq!(
        reserve.exit_bytes_after(&schedule(0)),
        outcome_width + terminal_width
    );
    assert_eq!(reserve.exit_bytes_after(&outcome(0)), terminal_width);
    assert_eq!(
        reserve.exit_bytes_after(&RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: &EFFECT_RESULT,
        }),
        terminal_width
    );
    assert_eq!(
        reserve.exit_bytes_after(&RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: RUN_INPUT,
        }),
        terminal_width
    );
    assert_eq!(reserve.exit_bytes_after(&terminal()), 0);
    assert_eq!(
        reserve.exit_bytes_after(&RecordRef::RunFailed {
            error: &TERMINAL_RESULT
        }),
        0
    );
}

#[test]
fn a_reserve_refuses_bounds_a_bank_cannot_roll_over_into() {
    // A next-run input longer than a bank header can carry is a run that can never
    // `continue_as_new`, which is half of what §10 promises. Refused where the bounds are
    // declared rather than at the swap, because the swap is the wrong place to learn it.
    let bounds = Bounds {
        run_input_bytes: u16::MAX,
        ..BOUNDS
    };
    assert_eq!(
        Reserve::for_layout(bounds, layout()),
        Err(CapacityError::SwapDoesNotFit)
    );
}

#[test]
fn a_reserve_refuses_bounds_whose_own_tail_a_bank_could_not_hold() {
    // The other half: bounds under which the run this device rolls into would have no room
    // for its own exit. A bank of two 64-byte erase blocks holds a header and almost
    // nothing.
    let Ok(small) = Geometry::new(256, 128, 8, 1) else {
        unreachable!("256 is two whole 128-byte blocks")
    };
    let Ok(small) = BankLayout::new(small) else {
        unreachable!("two erase blocks are two banks")
    };
    let bounds = Bounds {
        run_input_bytes: 48,
        effect_result_bytes: 24,
        terminal_bytes: 24,
    };
    assert_eq!(
        Reserve::for_layout(bounds, small),
        Err(CapacityError::ReserveDoesNotFit)
    );
}

#[test]
fn a_gate_refuses_a_journal_smaller_than_the_one_its_reserve_was_priced_for() {
    // `for_layout` priced a bank's worst-case journal; nothing about a `Reserve` value ties
    // it to that bank afterwards. Review of this change built one on a 4 KiB layout, handed
    // it a journal from a 256 B device of the same granularity, and watched every
    // `RunStarted` be refused for ever with the journal empty. Refused at the gate instead.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let Ok(cramped) = JournalRegion::spanning(geometry(), region.base(), 64, align()) else {
        unreachable!("64 bytes at a bank's journal base is a legal program region")
    };
    let journal = opened(&mut device, cramped);
    assert_eq!(
        Reserved::over(journal, reserve()).map(|_| ()),
        Err(CapacityError::RegionTooSmall)
    );
}

#[test]
fn a_reserve_refuses_a_journal_written_at_another_granularity() {
    // A reserve computed at a byte-programmable granularity under-reserves on a device with
    // an eight-byte program unit: every frame there is padded and every figure is too small.
    let Ok(fine) = Geometry::new(8192, 4096, 1, 1) else {
        unreachable!("8192 is two whole 4096-byte blocks of single bytes")
    };
    let Ok(fine) = BankLayout::new(fine) else {
        unreachable!("two erase blocks are two banks")
    };
    let Ok(fine_reserve) = Reserve::for_layout(BOUNDS, fine) else {
        unreachable!("these bounds fit a 4 KiB bank")
    };

    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let journal = opened(&mut device, region);
    assert_eq!(
        Reserved::over(journal, fine_reserve).map(|_| ()),
        Err(CapacityError::WrongGranularity)
    );
}

#[test]
fn a_reserve_refuses_bounds_whose_run_could_start_and_end_and_never_do_any_work() {
    // The floor is "can be *used*", not "can end". These bounds fit a 128-byte bank's
    // opening record and its exit, and leave no room for the `EffectScheduled` between
    // them — a run that can start, and finish, and never schedule anything. A floor that
    // priced only `max(opening record + terminal, tail)` accepts this.
    let Ok(geometry) = Geometry::new(256, 128, 8, 1) else {
        unreachable!("256 is two whole 128-byte blocks of whole 8-byte units")
    };
    let Ok(small) = BankLayout::new(geometry) else {
        unreachable!("two erase blocks are two banks")
    };
    let bounds = Bounds {
        run_input_bytes: 8,
        effect_result_bytes: 0,
        terminal_bytes: 0,
    };
    assert_eq!(
        Reserve::for_layout(bounds, small),
        Err(CapacityError::ReserveDoesNotFit)
    );
}

#[test]
fn a_bound_no_record_could_carry_is_not_reported_as_a_bank_being_too_small() {
    // §09's `payload_len` is a `u16` and a `RunStarted` spends four of those bytes on the
    // workflow identity, so a run input a *header* can carry is not always one a record can.
    // No bank will fix that, and the refusal has to say so or it sends an operator to the
    // geometry. A 4 MiB device with 1 MiB banks is one whose header really could carry it.
    let Ok(geometry) = Geometry::new(4 * 1024 * 1024, 1024 * 1024, 8, 1) else {
        unreachable!("4 MiB is four whole 1 MiB blocks")
    };
    let Ok(large) = BankLayout::new(geometry) else {
        unreachable!("four erase blocks are two banks of two")
    };
    let bounds = Bounds {
        run_input_bytes: u16::MAX,
        ..BOUNDS
    };
    assert_eq!(
        large.bank(BankId::A).max_run_input_bytes(large.align()),
        usize::from(u16::MAX),
        "a 1 MiB bank's header really can carry the longest input the field describes"
    );
    assert_eq!(
        Reserve::for_layout(bounds, large),
        Err(CapacityError::BoundUnencodable)
    );
}

#[test]
fn every_layout_a_reserve_accepts_can_run_an_effect_end_to_end() {
    // `for_layout`'s postcondition, swept rather than asserted about one fixture: whenever a
    // reserve exists, the worst-case journal behind a worst-case header admits the opening
    // record, a schedule, its outcome and a terminal record, in that order, with the room
    // each leaves behind. A floor that dropped any of its four terms shows up here as a
    // refusal on a layout `for_layout` said was fine.
    let mut accepted = 0_u32;
    for (capacity, erase, unit) in [
        (256_u32, 128_u32, 1_u16),
        (256, 128, 8),
        (512, 256, 4),
        (1024, 512, 8),
        (4096, 1024, 16),
        (8192, 4096, 8),
        (65_536, 4096, 32),
    ] {
        for run_input in [0_u16, 1, 8, 64] {
            for result in [0_u16, 1, 32, 200] {
                let (Ok(geometry), Some(_)) = (
                    Geometry::new(capacity, erase, u32::from(unit), 1),
                    ProgramAlign::new(unit),
                ) else {
                    continue;
                };
                let Ok(layout) = BankLayout::new(geometry) else {
                    continue;
                };
                let bounds = Bounds {
                    run_input_bytes: run_input,
                    effect_result_bytes: result,
                    terminal_bytes: result,
                };
                let Ok(reserve) = Reserve::for_layout(bounds, layout) else {
                    continue;
                };
                accepted = accepted.saturating_add(1);

                // The journal a `continue_as_new` at these bounds would leave behind.
                let region = layout.bank(BankId::A);
                let Some(mut room) = region.payload_bytes().checked_sub(reserve.rollover_bytes())
                else {
                    unreachable!("a reserve that exists was priced inside its bank")
                };

                let input = std::vec![0_u8; usize::from(run_input)];
                let payload = std::vec![0_u8; usize::from(result)];
                let run = layout.align();
                let sequence: [RecordRef<'_>; 4] = [
                    RecordRef::RunStarted {
                        workflow_kind: 1,
                        workflow_version: 1,
                        input: &input,
                    },
                    RecordRef::EffectScheduled {
                        seq: EffectSeq(0),
                        kind: DOWNLOAD,
                        input_len: 0,
                        input_crc: 0,
                    },
                    RecordRef::EffectCompleted {
                        seq: EffectSeq(0),
                        result: &payload,
                    },
                    RecordRef::RunCompleted { result: &payload },
                ];
                for record in &sequence {
                    assert_eq!(
                        reserve.admits(record, room),
                        Ok(()),
                        "a reserve for {bounds:?} on {capacity}/{erase}/{unit} refused \
                         {record:?} with {room} B left"
                    );
                    let Ok(bytes) = frame::encoded_len(record, run) else {
                        unreachable!("a record the reserve admitted encodes")
                    };
                    let (Ok(width), Some(left)) = (
                        u32::try_from(bytes),
                        room.checked_sub(u32::try_from(bytes).unwrap_or(u32::MAX)),
                    ) else {
                        unreachable!(
                            "a record the reserve admitted fits the room it was admitted for"
                        )
                    };
                    let _ = width;
                    room = left;
                }
            }
        }
    }
    assert!(
        accepted > 20,
        "the sweep accepted {accepted} layouts, which is too few to be saying anything"
    );
}

// ---------------------------------------------------------------------------------------
// Issue #25's three "done when" bullets
// ---------------------------------------------------------------------------------------

#[test]
fn scheduling_fails_at_the_reserve_boundary_while_a_terminal_record_still_commits() {
    // Issue #25's first exit criterion. §10: "ordinary effect scheduling fails early with
    // `HistoryNearCapacity`", and "early" is the whole point — a run that discovered the
    // boundary by running out of room mid-frame would be a run with a half-written record at
    // the end of its bank.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);

    let effects = fill_to_the_boundary(&mut writer, &mut device);
    assert!(
        effects > 0,
        "a 4 KiB bank holds more than nothing before its reserve"
    );

    // The state the refusal leaves: less than a schedule plus its exit costs, and at least
    // the tail the reserve kept.
    let room = writer.journal().room();
    assert!(
        room < width(&schedule(effects)) + reserve().exit_bytes_after(&schedule(effects)),
        "a schedule was refused, so it does not fit beside what it would owe"
    );

    // And the exit §10 promises is still open, at the full bound.
    let written = commit(&mut writer, &mut device, &terminal())
        .expect("a terminal record is what the reserve was kept for");
    assert_eq!(written.barriers(), 2);
    assert_eq!(writer.journal().room(), room - width(&terminal()));
}

#[test]
fn continue_as_new_succeeds_from_the_near_capacity_state() {
    // Issue #25's second exit criterion. §10's swap writes to the *inactive* bank, so the
    // active bank being at its reserve boundary is not an obstacle — but "not an obstacle"
    // is a claim, and this is the seven steps run from exactly that state.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);
    let effects = fill_to_the_boundary(&mut writer, &mut device);
    assert!(effects > 0);
    assert_eq!(
        commit(&mut writer, &mut device, &schedule(effects)),
        Err(ReservedError::Capacity(Refusal::NearCapacity)),
        "the run is at its reserve boundary"
    );

    // Step 1 is the caller's: it stops scheduling, which the refusal above already made it
    // do. Step 2: erase the inactive bank.
    let inactive = layout().bank(BankId::B);
    device
        .erase(inactive.base(), inactive.bytes())
        .expect("a whole bank is a whole number of erase blocks");
    // Steps 3 to 6: the new header, a barrier, the higher generation seal, a barrier.
    let next = install(&mut device, BankId::B, Generation(2), &NEXT_RUN_INPUT);

    // Step 7's precondition: the new bank is authoritative and the old one is not.
    assert_eq!(
        sealed_generation(&mut device, BankId::A),
        Some(Generation(1))
    );
    assert_eq!(
        sealed_generation(&mut device, BankId::B),
        Some(Generation(2))
    );

    // And the new run can do everything the reserve promised the *next* run could: the bank
    // it rolled into runs to its own boundary and still ends. That is `for_layout`'s floor —
    // an opening record, one effect scheduled and resolved, and an exit — asserted end to
    // end on media rather than as arithmetic, with the header at exactly the declared bound.
    let mut rolled = reserved(&mut device, next);
    assert_eq!(rolled.journal().offset(), 0);
    commit(&mut rolled, &mut device, &opening())
        .expect("a rolled-over bank has room for its own opening record");
    let effects = fill_to_the_boundary(&mut rolled, &mut device);
    assert!(
        effects > 0,
        "the reserve priced this bank for at least one effect"
    );
    commit(&mut rolled, &mut device, &terminal()).expect("and for the exit after it");
    assert!(
        device
            .window(layout().bank(BankId::B).base(), 4)
            .iter()
            .any(|byte| *byte != ERASED_BYTE),
        "the rolled-into bank really was written"
    );
}

#[test]
fn the_exit_is_still_open_after_a_reset_at_the_boundary() {
    // The reserve's real claim is not about one call: it is that a device which admitted a
    // record can still write its exit *after any reset*. On a reboot the writer is rebuilt
    // from a recovery of media and the reserve is recomputed from the bounds and the
    // geometry, and nothing else in this file exercises that join.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);
    let effects = fill_to_the_boundary(&mut writer, &mut device);
    assert!(effects > 0);
    let offset = writer.journal().offset();

    // The power goes here. Everything below is what the next boot does: a fresh recovery of
    // the same media, a fresh reserve from the same bounds, and a fresh gate over the two.
    // The writer above is not used again.
    let _ = writer;
    let mut rebooted = reserved(&mut device, region);
    assert_eq!(
        rebooted.journal().offset(),
        offset,
        "recovery found the same append point the writer left"
    );
    assert_eq!(
        commit(&mut rebooted, &mut device, &schedule(effects)),
        Err(ReservedError::Capacity(Refusal::NearCapacity)),
        "and the same boundary"
    );
    commit(&mut rebooted, &mut device, &terminal())
        .expect("the exit the reserve was kept for survives a reset");
}

#[test]
fn the_reserve_holds_at_every_program_granularity() {
    // Every media-driving test above runs at one granularity, and every figure in a reserve
    // is padded to the program unit. `tests/append.rs` sweeps the seal's width the same way
    // and for the same reason: a reserve correct only at eight-byte units is a reserve
    // nobody outside this fixture can use.
    for unit in [1_u32, 2, 4, 8, 16, 32] {
        let (Ok(geometry), Some(_)) =
            (Geometry::new(8192, 4096, unit, 1), u16::try_from(unit).ok())
        else {
            unreachable!("a legal geometry")
        };
        let Ok(layout) = BankLayout::new(geometry) else {
            unreachable!("two erase blocks are two banks")
        };
        let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
            unreachable!("these bounds fit a 4 KiB bank at any granularity")
        };

        let mut device = Nor::new(geometry);
        let region = install_on(&mut device, layout, BankId::A, Generation(1), RUN_INPUT);
        let mut writer = gated(&mut device, region, reserve);
        let effects = fill_to_the_boundary(&mut writer, &mut device);
        assert!(
            effects > 0,
            "a 4 KiB bank at a {unit}-byte program unit holds at least one effect"
        );
        assert!(
            commit(&mut writer, &mut device, &terminal()).is_ok(),
            "the exit is still open at a {unit}-byte program unit"
        );
    }
}

#[test]
fn a_refused_schedule_moves_no_byte_and_asks_the_device_for_nothing() {
    // Issue #25's third exit criterion, checked two independent ways because either alone
    // could pass while the other failed: a program that cleared no bit changes no image, and
    // an image compared without the op log would not notice one.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);
    let effects = fill_to_the_boundary(&mut writer, &mut device);

    let before = device.snapshot();
    let offset = writer.journal().offset();
    let amplification = writer.journal().amplification();
    device.ops.clear();

    assert_eq!(
        commit(&mut writer, &mut device, &schedule(effects)),
        Err(ReservedError::Capacity(Refusal::NearCapacity))
    );

    assert_eq!(device.ops, Vec::new(), "the device was never asked");
    assert_eq!(device.snapshot(), before, "no byte moved");
    assert_eq!(
        writer.journal().offset(),
        offset,
        "the journal did not advance"
    );
    assert_eq!(
        writer.journal().amplification(),
        amplification,
        "a refusal costs the media nothing, so it is not wear"
    );

    // And the writer is not poisoned: the exit the reserve was kept for still works.
    commit(&mut writer, &mut device, &terminal()).expect("a refusal is not a broken journal");
}

// ---------------------------------------------------------------------------------------
// Teeth: the reserve everybody writes first, and what it costs
// ---------------------------------------------------------------------------------------

#[test]
fn a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding() {
    // The tempting arithmetic. §10 says "enough tail space for a terminal record", so the
    // obvious reserve is one terminal record — and it is wrong, because §08's transition
    // table has no edge from an unresolved effect to a terminal one. A schedule admitted
    // under it leaves room for `RunCompleted` and no room for the outcome that has to come
    // first, and the run can never write either.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut journal = opened(&mut device, region);
    let terminal_only = width(&terminal());

    // Drive the wrong policy directly against the unreserved writer.
    let mut seq = 0_u32;
    loop {
        if width(&schedule(seq)) + terminal_only > journal.room() {
            break;
        }
        let Ok(_) = commit_unreserved(&mut journal, &mut device, &schedule(seq)) else {
            unreachable!("the wrong policy admitted this schedule, so it fits")
        };
        if width(&outcome(seq)) > journal.room() {
            // Stranded. The effect is unresolved, its outcome does not fit, and §08 refuses
            // a terminal record while it is outstanding.
            assert!(
                journal.room() < width(&outcome(seq)),
                "the outcome this schedule owes has nowhere to go"
            );
            // The real reserve would have refused that schedule.
            let room_before = journal.room() + width(&schedule(seq));
            assert_eq!(
                reserve().admits(&schedule(seq), room_before),
                Err(Refusal::NearCapacity),
                "§10's reserve refuses the schedule the terminal-only rule admitted"
            );
            return;
        }
        let Ok(_) = commit_unreserved(&mut journal, &mut device, &outcome(seq)) else {
            unreachable!("the outcome fits, so it commits")
        };
        seq = seq.saturating_add(1);
    }
    unreachable!("a terminal-only reserve strands this run before the journal is full")
}

// ---------------------------------------------------------------------------------------
// Admission, as a predicate
// ---------------------------------------------------------------------------------------

#[test]
fn an_admitted_record_always_fits_the_journal_it_was_admitted_for() {
    // The predicate and the writer have to agree: a record `admits` accepts must never be
    // the one `Journal::stage` refuses with `NoRoom`, or the reserve would be advice.
    let reserve = reserve();
    for room in 0..256_u32 {
        for record in [schedule(0), outcome(0), terminal()] {
            if reserve.admits(&record, room).is_ok() {
                assert!(
                    width(&record) <= room,
                    "an admitted record must fit the room it was admitted for"
                );
                assert!(
                    width(&record) + reserve.exit_bytes_after(&record) <= room,
                    "and must leave what it still owes"
                );
            }
        }
    }
}

#[test]
fn a_record_that_exactly_fits_beside_what_it_owes_is_admitted() {
    // The completeness half. `an_admitted_record_always_fits_the_journal_it_was_admitted_for`
    // is one-directional — it is satisfied by an `admits` that refuses everything — so the
    // exact boundary is pinned here, one byte either side of it, for a record that owes
    // nothing and for one that owes the whole tail.
    let reserve = reserve();
    for record in [terminal(), schedule(0), outcome(0)] {
        let needed = width(&record) + reserve.exit_bytes_after(&record);
        assert_eq!(
            reserve.admits(&record, needed),
            Ok(()),
            "a record that exactly fits beside what it owes is admitted"
        );
        assert_eq!(
            reserve.admits(&record, needed - 1),
            Err(Refusal::NearCapacity),
            "and one byte less is not"
        );
    }
}

#[test]
fn a_run_started_at_its_declared_bound_is_admitted_and_priced_from_the_codec() {
    // `start_bytes` is the one ceiling no other test exercises: `RunStarted` appears
    // elsewhere only in the over-length refusal. A four-byte error in the workflow-identity
    // prefix makes a run unable to write its own first record, for ever.
    let reserve = reserve();
    let at_the_bound = RecordRef::RunStarted {
        workflow_kind: 0x0042,
        workflow_version: 3,
        input: &NEXT_RUN_INPUT,
    };
    assert_eq!(reserve.admits(&at_the_bound, 4096), Ok(()));
    // And priced at exactly what the codec charges, the way the outcome and the terminal
    // record are: a ceiling derived from a constant that drifted from `frame::body` would
    // refuse a legal record rather than admit an oversized one, which is silent.
    let needed = width(&at_the_bound) + reserve.exit_bytes_after(&at_the_bound);
    assert_eq!(reserve.admits(&at_the_bound, needed), Ok(()));
    assert_eq!(
        reserve.admits(&at_the_bound, needed - 1),
        Err(Refusal::NearCapacity),
        "an opening record priced above the codec's figure would refuse here for the wrong \
         reason"
    );
}

#[test]
fn a_record_longer_than_the_bounds_the_reserve_was_computed_from_is_refused() {
    // A reserve is a promise about records of a declared size. A terminal record longer than
    // its bound is a record the reserve never budgeted for, and admitting it would make the
    // promise false for the run that comes after it — so it is refused with the kernel's own
    // word for a length that does not fit.
    let oversized = [0_u8; 64];
    assert_eq!(
        reserve().admits(&RecordRef::RunCompleted { result: &oversized }, 4096),
        Err(Refusal::OverDeclaredBound)
    );
    assert_eq!(
        reserve().admits(
            &RecordRef::EffectCompleted {
                seq: EffectSeq(0),
                result: &oversized,
            },
            4096
        ),
        Err(Refusal::OverDeclaredBound)
    );
    assert_eq!(
        reserve().admits(
            &RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: &[0_u8; 64],
            },
            4096
        ),
        Err(Refusal::OverDeclaredBound)
    );
    // And one at exactly the bound is not.
    assert_eq!(reserve().admits(&terminal(), 4096), Ok(()));
}

#[test]
fn a_reserved_writer_reports_the_same_amplification_the_plain_one_does() {
    // The reserve is a gate and not a second writer: what reaches media, and what it cost,
    // is `append`'s to say.
    let mut plain = Nor::new(geometry());
    let plain_region = install(&mut plain, BankId::A, Generation(1), RUN_INPUT);
    let mut plain_journal = opened(&mut plain, plain_region);
    let plain_written =
        commit_unreserved(&mut plain_journal, &mut plain, &schedule(0)).expect("a legal append");

    let mut gated = Nor::new(geometry());
    let gated_region = install(&mut gated, BankId::A, Generation(1), RUN_INPUT);
    let mut gated_writer = reserved(&mut gated, gated_region);
    let gated_written =
        commit(&mut gated_writer, &mut gated, &schedule(0)).expect("a legal append");

    assert_eq!(plain_written, gated_written);
    assert_eq!(plain.snapshot(), gated.snapshot());
    assert_eq!(
        plain_journal.offset(),
        gated_writer.journal().offset(),
        "the same record leaves the two writers in the same place"
    );
}

#[test]
fn a_full_journal_refuses_even_a_terminal_record() {
    // The reserve is not a guarantee that a terminal record fits *whatever* happened: it is
    // a guarantee about journals only this gate has written. A writer that ran to the end
    // through some other path meets `NoRoom` from `append`, which is the honest answer.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut journal = opened(&mut device, region);
    while commit_unreserved(&mut journal, &mut device, &schedule(0)).is_ok() {}
    assert!(journal.room() < width(&schedule(0)));

    let Ok(writer) = Reserved::over(journal, reserve()) else {
        unreachable!("the reserve fits this layout")
    };
    let mut writer = writer;
    assert!(matches!(
        commit(&mut writer, &mut device, &terminal()),
        Err(ReservedError::Capacity(Refusal::NearCapacity))
    ));
    // And the media is untouched by that refusal, which is what "no mutation at all" means
    // on the path where even the exit does not fit.
    let before = device.snapshot();
    let _ = commit(&mut writer, &mut device, &terminal());
    assert_eq!(device.snapshot(), before);
}

#[test]
fn a_journal_driven_past_its_capacity_refuses_rather_than_reusing_its_own_bytes() {
    // §10's third sentence: "the runtime never overwrites committed history to make room" —
    // no circular buffer, no oldest-record eviction. A journal is driven to its boundary,
    // then past it in every way this gate allows, and the first record it ever committed is
    // still the first record it reads back, at the same offset, byte for byte.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);

    let first = width(&schedule(0));
    commit(&mut writer, &mut device, &schedule(0)).expect("an empty journal takes a schedule");
    let opening = device.window(region.base(), first);
    assert!(opening.iter().any(|byte| *byte != ERASED_BYTE));
    commit(&mut writer, &mut device, &outcome(0)).expect("and the outcome it owes");

    let effects = fill_to_the_boundary(&mut writer, &mut device);
    let offset = writer.journal().offset();
    let image = device.snapshot();

    // Every refusal there is, repeatedly. None of them may move the offset backwards, and
    // none of them may put a byte anywhere.
    for _ in 0..4 {
        assert_eq!(
            commit(&mut writer, &mut device, &schedule(effects + 1)),
            Err(ReservedError::Capacity(Refusal::NearCapacity))
        );
        assert_eq!(
            writer.journal().offset(),
            offset,
            "an offset that moved backwards is a journal reusing its own bytes"
        );
    }
    assert_eq!(device.snapshot(), image, "no byte was reused");
    assert_eq!(
        device.window(region.base(), first),
        opening,
        "the first record ever committed is untouched"
    );

    // And the exit is the only thing that still moves it, forwards.
    commit(&mut writer, &mut device, &terminal()).expect("the exit the reserve was kept for");
    assert!(writer.journal().offset() > offset);
}

#[test]
fn a_bank_at_the_boundary_still_reads_back_as_the_history_it_committed() {
    // The reserve must not corrupt the thing it protects. Every record committed up to the
    // boundary is recovered, in order, by the reader that walks media.
    let mut device = Nor::new(geometry());
    let region = install(&mut device, BankId::A, Generation(1), RUN_INPUT);
    let mut writer = reserved(&mut device, region);
    let effects = fill_to_the_boundary(&mut writer, &mut device);
    let offset = writer.journal().offset();

    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    let mut seen = 0_u32;
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            unreachable!("every frame this writer committed is sound")
        };
        match record {
            RecordRef::EffectScheduled { seq, .. } => assert_eq!(seq.0, seen),
            RecordRef::EffectCompleted { seq, .. } => {
                assert_eq!(seq.0, seen);
                seen = seen.saturating_add(1);
            }
            other => unreachable!("this journal holds no {other:?}"),
        }
    }
    assert_eq!(seen, effects);
    assert_eq!(recovery.ending(), Some(Ending::Clean { append_at: offset }));
    assert_eq!(recovery.append_offset(), Some(offset));
}
