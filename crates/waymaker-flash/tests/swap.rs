//! §10's seven-step bank swap, driven against media that records what it was asked.
//!
//! Design document §10 Two-bank lifecycle, and issue
//! [#26](https://github.com/madmax983/waymaker/issues/26), which states the protocol as
//! seven steps:
//!
//! 1. stop accepting new effects for the current run;
//! 2. erase the inactive bank;
//! 3. write the new bank header, new `RunId`, workflow version and next-run input;
//! 4. **barrier** — the new bank's payload becomes durable;
//! 5. write the higher-generation bank seal;
//! 6. **barrier** — the new bank becomes authoritative;
//! 7. return success and lazily erase the old bank.
//!
//! Every test here drives `waymaker_flash::swap` against a NOR-shaped device that keeps the
//! sequence of mutations it was asked for, so "the barrier is between the payload and the
//! seal" is read off the recorded sequence rather than argued from the source.
//!
//! # What is *not* here
//!
//! The crash sweep. "Every crash point across all seven steps" is
//! `crates/waymaker-fault/tests/swap.rs`, because the injector is a crate a layer may not
//! depend on in any dependency kind — the same division
//! [ADR 0013](https://github.com/madmax983/waymaker/blob/main/docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md)
//! makes for every other writer in this workspace.

use waymaker_core::{EffectId, EffectIdAllocator, EffectSeq, RecordRef, RunId};
use waymaker_flash::append::Journal;
use waymaker_flash::bank::{self, Authority, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::frame::{ERASED_BYTE, ProgramAlign};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery, RegionError};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};
use waymaker_flash::swap::{Installed, Retired, Swap, SwapError, SwapFailure};

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
/// Erased is `0xFF` and a program only ever clears bits, so a swap that programmed a seal
/// over an unerased bank would show up as bytes rather than as a passing test.
struct Nor {
    geometry: Geometry,
    media: Vec<u8>,
    ops: Vec<Op>,
    /// How many mutations to accept before refusing every one that follows.
    ///
    /// [`usize::MAX`] is a device that never fails, which is what most of this file wants.
    /// A smaller number is how the tests below reach [`SwapFailure::Storage`] at a chosen
    /// step without an injector: §12 says `program` and `erase` may fail, and a swap that
    /// carried on past one would be a swap sealing a bank it never wrote.
    accepts: usize,
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
            accepts: usize::MAX,
        }
    }

    /// The same device, refusing every mutation after the first `accepts`.
    const fn failing_after(mut self, accepts: usize) -> Self {
        self.accepts = accepts;
        self
    }

    /// Whether this call is one the device still accepts, counting it either way.
    fn accept(&self) -> Result<(), GeometryError> {
        if self.ops.len() >= self.accepts {
            return Err(GeometryError::OutOfBounds);
        }
        Ok(())
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
        self.accept()?;
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
        self.accept()?;
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
        self.accept()?;
        self.ops.push(Op::Barrier);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------

const PAGE: usize = 512;

/// The run the device is on when a swap starts.
const RUN: RunId = RunId(0x0123_4567_89AB_CDEF);

/// The run a swap installs. Distinct from [`RUN`] in every bit that matters.
const NEXT_RUN: RunId = RunId(0xFEDC_BA98_7654_3210);

/// The generation the current bank carries.
const CURRENT: Generation = Generation(4);

/// The generation a swap installs, minted the way the writer mints it.
const NEXT: Generation = match CURRENT.successor() {
    Some(next) => next,
    None => unreachable!(),
};

/// This run's input, which the bank on media carries.
const RUN_INPUT: &[u8] = b"the-run-in-progress";

/// The bounded input the workflow supplies for its next run.
const NEXT_INPUT: &[u8] = b"what-the-next-run-starts-from";

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

/// The header the bank in use carries.
fn current_header() -> BankHeader<'static> {
    BankHeader {
        run: RUN,
        align: align(),
        workflow_kind: 0x0042,
        workflow_version: 3,
        input_schema: 1,
        input: RUN_INPUT,
    }
}

/// The header a swap installs: a new run, a later workflow version, a new input.
fn next_header() -> BankHeader<'static> {
    BankHeader {
        run: NEXT_RUN,
        align: align(),
        workflow_kind: 0x0042,
        workflow_version: 4,
        input_schema: 1,
        input: NEXT_INPUT,
    }
}

/// What `select` says about the device as it stands.
fn authority(device: &mut Nor) -> Authority {
    bank::select(BankId::ALL.map(|id| sealed_generation(device, id)))
}

/// The generation a bank on media reports, read back the way a cold boot has to.
fn sealed_generation(device: &mut Nor, id: BankId) -> Option<Generation> {
    let region = layout().bank(id);
    let mut page = [0_u8; PAGE];
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = usize::try_from(region.seal_bytes()) else {
        unreachable!("a host holds a seal")
    };
    let (Some(head), Some(tail)) = (page.get_mut(..PAGE), seal_bytes.get_mut(..seal_len)) else {
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

/// The header a bank on media carries, decoded the way a cold boot has to.
fn header_on(device: &mut Nor, id: BankId) -> Option<(RunId, u16, Vec<u8>)> {
    let region = layout().bank(id);
    let mut page = [0_u8; PAGE];
    let Ok(()) = device.read(region.base(), &mut page) else {
        unreachable!("a bank's header is inside the device")
    };
    let decoded = bank::decode_header(&page).ok()?;
    Some((
        decoded.run,
        decoded.workflow_version,
        decoded.input.to_vec(),
    ))
}

/// Installs a bank the way a previous life left it: header, barrier, seal, barrier.
///
/// Not the swap under test — this is the device's history, and a test that built its
/// starting state with the writer it is testing would be a test of nothing.
fn install(device: &mut Nor, id: BankId, generation: Generation, header: &BankHeader<'_>) {
    let region = layout().bank(id);
    let mut staging = [0_u8; PAGE];
    let Ok(header_len) = bank::encode_header(header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.base(), header_frame),
        device.barrier(),
    ) else {
        unreachable!("a bank header is a legal program")
    };
    let Ok(seal) = bank::seal_for(header_frame, generation) else {
        unreachable!("a header frame can be sealed")
    };
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, layout().align(), &mut seal_bytes) else {
        unreachable!("a seal fits its own region")
    };
    let Some(sealed) = seal_bytes.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.seal_offset(), sealed),
        device.barrier(),
    ) else {
        unreachable!("a generation seal is a legal program")
    };
}

/// A device booted from bank A at [`CURRENT`], with bank B holding an older run.
///
/// Bank B is written rather than left erased on purpose: the swap has to *recycle* a bank
/// that has something in it, and a sweep over an already-erased spare would never program
/// over a stale seal.
fn booted() -> Nor {
    let mut device = Nor::new(geometry());
    let stale = BankHeader {
        run: RunId(0x1111_1111_1111_1111),
        input: b"a-run-two-generations-ago",
        ..current_header()
    };
    install(&mut device, BankId::B, Generation(3), &stale);
    install(&mut device, BankId::A, CURRENT, &current_header());
    device.ops.clear();
    device
}

/// The journal region of the bank in use.
fn current_region() -> JournalRegion {
    let Ok(region) = JournalRegion::of(layout(), BankId::A, &current_header()) else {
        unreachable!("bank A holds a journal behind its header")
    };
    region
}

/// A writer over the bank in use, positioned where a recovery of it says it may write.
fn current_journal(device: &mut Nor) -> Journal {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(current_region());
    while recovery.next(device, &mut page).is_some() {}
    let Some(journal) = Journal::after(recovery) else {
        unreachable!("an erased journal has an append point")
    };
    journal
}

/// The swap this file is about, planned but not yet begun.
fn planned(device: &mut Nor) -> Swap<'static> {
    let retired = Retired::Journal(current_journal(device));
    let Ok(swap) = Swap::beginning(
        layout(),
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        retired,
        next_header(),
    ) else {
        unreachable!("this device is on a run that can roll over")
    };
    swap
}

/// The whole protocol, steps 2 to 6, against `device`.
fn perform(device: &mut Nor) -> Installed {
    let swap = planned(device);
    let mut page = [0_u8; PAGE];
    let Ok(installed) = swap
        .prepare(device)
        .and_then(|prepared| prepared.stage(device, &mut page))
        .and_then(|staged| staged.payload_barrier(device))
        .and_then(|sealable| sealable.commit(device))
    else {
        unreachable!("a swap on a device that accepts every mutation succeeds")
    };
    installed
}

// ---------------------------------------------------------------------------------------
// The protocol
// ---------------------------------------------------------------------------------------

#[test]
fn a_swap_installs_the_next_run_in_the_other_bank_and_makes_it_authoritative() {
    let mut device = booted();
    let installed = perform(&mut device);

    assert_eq!(
        authority(&mut device),
        Authority::Bank {
            id: BankId::B,
            generation: NEXT
        },
        "\u{a7}10: the bank with the highest valid generation seal is authoritative"
    );
    assert_eq!(
        installed.authority(),
        Authority::Bank {
            id: BankId::B,
            generation: NEXT
        },
        "a completed swap reports the authority the next swap begins from"
    );
    assert_eq!(
        header_on(&mut device, BankId::B),
        Some((NEXT_RUN, 4, NEXT_INPUT.to_vec())),
        "\u{a7}10 step 3: the new run id, workflow version and next-run input"
    );
}

#[test]
fn the_seven_steps_reach_the_device_in_the_order_the_protocol_states() {
    // The whole of §10 as a sequence of calls. A barrier missing between the payload and
    // the seal is a seal that can reach media first, and no assertion about the bytes on a
    // device whose barrier is a no-op could see it — which is why this is read off the
    // recorded sequence.
    let mut device = booted();
    let spare = layout().bank(BankId::B);
    let retiring = layout().bank(BankId::A);

    let installed = perform(&mut device);
    let through_six = device.ops.clone();
    let Ok(()) = installed.reclaim(&mut device) else {
        unreachable!("a device that accepts every mutation accepts an erase")
    };

    assert_eq!(
        through_six,
        vec![
            // Step 2, and the barrier that orders the header after it. Without it §12
            // permits the header to become durable before the erase that would take it.
            Op::Erase {
                offset: spare.base(),
                len: spare.bytes()
            },
            Op::Barrier,
            // Step 3, then step 4.
            Op::Program {
                offset: spare.base(),
                len: header_bytes()
            },
            Op::Barrier,
            // Step 5, then step 6.
            Op::Program {
                offset: spare.seal_offset(),
                len: spare.seal_bytes()
            },
            Op::Barrier,
        ]
    );
    assert_eq!(
        device.ops.get(through_six.len()..),
        Some(
            [
                // Step 7, and it is the *retiring* bank.
                Op::Erase {
                    offset: retiring.base(),
                    len: retiring.bytes()
                },
                Op::Barrier,
            ]
            .as_slice()
        )
    );
}

/// Bytes the next run's header occupies on media, padded to the program unit.
fn header_bytes() -> u32 {
    let Some(offset) = next_header().journal_offset() else {
        unreachable!("this header has a journal behind it")
    };
    let Ok(bytes) = u32::try_from(offset) else {
        unreachable!("a header is shorter than a bank")
    };
    bytes
}

#[test]
fn the_old_run_survives_until_the_lazy_erase_and_is_never_authoritative_after_it() {
    // §10 step 7 is *lazy*: the old bank is still on media, still sealed, and still lower
    // than the new one, right up to the erase — and after it the device has one bank.
    let mut device = booted();
    let installed = perform(&mut device);

    assert_eq!(
        sealed_generation(&mut device, BankId::A),
        Some(CURRENT),
        "the retired bank is intact until it is reclaimed"
    );
    let Ok(()) = installed.reclaim(&mut device) else {
        unreachable!("a device that accepts every mutation accepts an erase")
    };

    assert_eq!(
        sealed_generation(&mut device, BankId::A),
        None,
        "a reclaimed bank is not a candidate at any generation"
    );
    assert_eq!(
        authority(&mut device),
        Authority::Bank {
            id: BankId::B,
            generation: NEXT
        }
    );
    assert!(
        device
            .window(
                layout().bank(BankId::A).base(),
                layout().bank(BankId::A).bytes()
            )
            .iter()
            .all(|byte| *byte == ERASED_BYTE),
        "a reclaimed bank is erased media"
    );
}

#[test]
fn the_installed_journal_is_erased_and_takes_the_new_runs_opening_record() {
    // §10 step 3 writes a header and nothing else, so the journal behind it is erased
    // media — which is the one ending after which appending is safe.
    let mut device = booted();
    let installed = perform(&mut device);
    let region = installed.region();

    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    while recovery.next(&mut device, &mut page).is_some() {}
    assert_eq!(recovery.ending(), Some(Ending::Clean { append_at: 0 }));

    let Some(mut journal) = Journal::after(recovery) else {
        unreachable!("an erased journal has an append point")
    };
    let opening = RecordRef::RunStarted {
        workflow_kind: 0x0042,
        workflow_version: 4,
        input: NEXT_INPUT,
    };
    let mut staging = [0_u8; PAGE];
    let Ok(_written) = journal
        .stage(&mut device, &opening, &mut staging)
        .and_then(|staged| staged.payload_barrier(&mut device))
        .and_then(|sealable| sealable.commit(&mut device))
    else {
        unreachable!("an erased journal takes its run's opening record")
    };
    assert!(journal.offset() > 0);
}

#[test]
fn recovery_never_combines_the_footprints_of_the_two_runs() {
    // Issue #26: "Recovery never combines their footprints." The old run's records are in
    // bank A and the new run's in bank B, and a cold boot reads exactly one of them —
    // whichever the generation seal names — with the run id under it.
    let mut device = booted();
    let mut page = [0_u8; PAGE];

    // Something in the old run's journal, so there is a footprint to combine.
    let mut old = current_journal(&mut device);
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: b"the-old-run",
    };
    let Ok(_written) = old
        .stage(&mut device, &record, &mut page)
        .and_then(|staged| staged.payload_barrier(&mut device))
        .and_then(|sealable| sealable.commit(&mut device))
    else {
        unreachable!("an erased journal takes a record")
    };

    // And the swap retires *that* writer, rather than a second one over the same region:
    // §10 step 1 is the run giving up the value it was appending with.
    let Ok(swap) = Swap::beginning(
        layout(),
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        Retired::Journal(old),
        next_header(),
    ) else {
        unreachable!("this device is on a run that can roll over")
    };
    let Ok(installed) = swap
        .prepare(&mut device)
        .and_then(|prepared| prepared.stage(&mut device, &mut page))
        .and_then(|staged| staged.payload_barrier(&mut device))
        .and_then(|sealable| sealable.commit(&mut device))
    else {
        unreachable!("a swap on a device that accepts every mutation succeeds")
    };

    let Authority::Bank { id, generation } = authority(&mut device) else {
        unreachable!("a swapped device has exactly one authoritative bank")
    };
    assert_eq!((id, generation), (BankId::B, NEXT));
    assert_eq!(
        header_on(&mut device, id).map(|(run, _, _)| run),
        Some(NEXT_RUN),
        "the run a reader boots is the one the authoritative bank's header names"
    );

    // And the journal behind that header is the new run's, which is empty: the old run's
    // record is in the bank the reader did not boot.
    let mut recovery = Recovery::new(installed.region());
    while recovery.next(&mut device, &mut page).is_some() {}
    assert_eq!(recovery.ending(), Some(Ending::Clean { append_at: 0 }));
}

// ---------------------------------------------------------------------------------------
// Effect identity across the boundary
// ---------------------------------------------------------------------------------------

#[test]
fn the_new_run_starts_its_effect_sequence_at_the_first_one() {
    // Issue #26's third "done when". A run that continued the old sequence would resolve
    // its first effect against a result belonging to the run before it.
    let mut device = booted();
    let installed = perform(&mut device);

    let mut allocator = installed.allocator();
    assert_eq!(allocator.run(), NEXT_RUN);
    assert_eq!(allocator.peek(), Some(EffectSeq::FIRST));
    assert_eq!(
        allocator.allocate(),
        Ok(EffectId {
            run: NEXT_RUN,
            seq: EffectSeq::FIRST
        })
    );
}

#[test]
fn the_old_runs_effect_ids_stay_distinguishable_from_the_new_runs() {
    // The sequence restarts, so the *pair* is what tells two effects apart — which is why
    // a swap that reused the run id would make the two runs' identities collide, and why
    // the constructor refuses one.
    let mut device = booted();
    let installed = perform(&mut device);

    let mut old = EffectIdAllocator::for_run(RUN);
    let mut new = installed.allocator();
    for _ in 0..4_u32 {
        let (Ok(before), Ok(after)) = (old.allocate(), new.allocate()) else {
            unreachable!("neither run has spent its sequence space")
        };
        assert_eq!(before.seq, after.seq, "the sequence restarts at the swap");
        assert_ne!(before, after, "and the pair is what keeps the two apart");
        assert_ne!(before.run, after.run);
    }
}

#[test]
fn a_swap_refuses_to_install_the_run_it_is_replacing() {
    // The one configuration under which the assertion above cannot hold: with the same run
    // id on both sides, the old run's committed effect ids and the new run's are the same
    // values, and nothing on the device can tell a redelivery from a fresh effect.
    let mut device = booted();
    let retired = Retired::Journal(current_journal(&mut device));
    let same_run = BankHeader {
        run: RUN,
        ..next_header()
    };

    assert_eq!(
        Swap::beginning(
            layout(),
            Authority::Bank {
                id: BankId::A,
                generation: CURRENT
            },
            RUN,
            retired,
            same_run,
        )
        .err(),
        Some(SwapError::RunReused)
    );
}

// ---------------------------------------------------------------------------------------
// What a swap refuses, before it touches media
// ---------------------------------------------------------------------------------------

/// Asserts that planning a swap over `booted()` refuses with `expected`, and moved nothing.
fn refuses(
    at: Authority,
    run: RunId,
    retired: impl FnOnce(&mut Nor) -> Retired,
    next: BankHeader<'_>,
    expected: SwapError,
) {
    let mut device = booted();
    let before = device.snapshot();
    let retired = retired(&mut device);

    assert_eq!(
        Swap::beginning(layout(), at, run, retired, next).err(),
        Some(expected)
    );
    assert_eq!(device.snapshot(), before, "a refusal moved bytes");
    assert_eq!(
        device.ops,
        Vec::new(),
        "a refusal asked the device for a mutation"
    );
}

#[test]
fn a_swap_refuses_a_device_with_no_single_authority() {
    // A device with nothing sealed has no run to continue, and one with two claims has no
    // single run to continue *from*. Neither is a swap; both are reported rather than
    // resolved, for the reason `Authority::Ambiguous` exists at all.
    for at in [
        Authority::Unsealed,
        Authority::Ambiguous {
            generation: CURRENT,
        },
    ] {
        refuses(
            at,
            RUN,
            |device| Retired::Journal(current_journal(device)),
            next_header(),
            SwapError::NoAuthority,
        );
    }
}

#[test]
fn a_swap_refuses_a_generation_that_would_wrap() {
    // `Generation::successor` is what makes the plain `u32` order the order of the swaps.
    // A device at the ceiling refuses to swap rather than reusing a generation, which is
    // this workspace's treatment of every other bounded counter.
    refuses(
        Authority::Bank {
            id: BankId::A,
            generation: Generation::MAX,
        },
        RUN,
        |device| Retired::Journal(current_journal(device)),
        next_header(),
        SwapError::GenerationExhausted,
    );
}

#[test]
fn a_swap_refuses_a_reader_that_is_not_the_bank_it_is_retiring() {
    // The writer being given up is what makes §10 step 1 structural. A reader of the bank
    // the swap is about to *erase* is not that: the caller would still hold a live writer
    // over the run the swap is replacing.
    let Ok(spare) = JournalRegion::of(layout(), BankId::B, &current_header()) else {
        unreachable!("bank B holds a journal behind a header of this shape")
    };
    refuses(
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        |_device| Retired::Recovery(Recovery::new(spare)),
        next_header(),
        SwapError::NotTheActiveBank,
    );
}

#[test]
fn a_swap_refuses_a_next_run_input_a_bank_cannot_hold_a_journal_behind() {
    // §10's roll-over is only an exit if the run it installs can do something. A header
    // that fills its bank is a run with nowhere to write its opening record, and the
    // reserve prices exactly this — but the reserve is a policy, and a swap that was handed
    // an over-long input directly must refuse it too.
    let oversized = std::vec![0x5A_u8; layout().bank(BankId::B).payload_bytes() as usize];
    refuses(
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        |device| Retired::Journal(current_journal(device)),
        BankHeader {
            input: &oversized,
            ..next_header()
        },
        SwapError::Region(RegionError::NoJournalRoom),
    );
}

#[test]
fn a_swap_refuses_a_header_written_at_another_granularity() {
    // `JournalRegion::of` is the one place the writer's granularity and the reader's are
    // welded together, and a swap that installed a header declaring another unit would
    // leave a bank whose journal offset no reader on this device computes.
    let Some(other) = ProgramAlign::new(16) else {
        unreachable!("16 is a power of two within the program-size range")
    };
    refuses(
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        |device| Retired::Journal(current_journal(device)),
        BankHeader {
            align: other,
            ..next_header()
        },
        SwapError::Region(RegionError::AlignDisagreesWithBank),
    );
}

// ---------------------------------------------------------------------------------------
// What a swap refuses once it has started
// ---------------------------------------------------------------------------------------

#[test]
fn every_step_refuses_a_device_the_swap_was_not_planned_for() {
    // The lesson issue #24's review left: a barrier taken on some *other* device orders
    // nothing on this one, and an erase or a program aimed at an offset proved legal on
    // another device is a write outside any bank that device has. So every step compares,
    // not only the first.
    let Ok(other_geometry) = Geometry::new(16384, 4096, 8, 1) else {
        unreachable!("16384 is four whole 4096-byte blocks")
    };

    let mut device = booted();
    let mut elsewhere = Nor::new(other_geometry);
    let mut page = [0_u8; PAGE];

    let swap = planned(&mut device);
    assert_eq!(
        swap.prepare(&mut elsewhere).err(),
        Some(SwapFailure::WrongDevice)
    );

    let Ok(prepared) = planned(&mut device).prepare(&mut device) else {
        unreachable!("this device accepts an erase")
    };
    let Err(refusal) = prepared.stage(&mut elsewhere, &mut page) else {
        unreachable!("a swap must not program a header on another device")
    };
    assert_eq!(refusal, SwapFailure::WrongDevice);

    let Ok(staged) = planned(&mut device)
        .prepare(&mut device)
        .and_then(|prepared| prepared.stage(&mut device, &mut page))
    else {
        unreachable!("this device accepts a header")
    };
    assert_eq!(
        staged.payload_barrier(&mut elsewhere).err(),
        Some(SwapFailure::WrongDevice)
    );

    let mut second = [0_u8; PAGE];
    let Ok(sealable) = planned(&mut device)
        .prepare(&mut device)
        .and_then(|prepared| prepared.stage(&mut device, &mut second))
        .and_then(|staged| staged.payload_barrier(&mut device))
    else {
        unreachable!("this device accepts a payload barrier")
    };
    assert_eq!(
        sealable.commit(&mut elsewhere).err(),
        Some(SwapFailure::WrongDevice)
    );

    let installed = perform(&mut device);
    assert_eq!(
        installed.reclaim(&mut elsewhere).err(),
        Some(SwapFailure::WrongDevice),
        "an erase aimed at a bank another device does not have is the worst of the five"
    );
}

#[test]
fn a_swap_refuses_a_page_too_small_for_the_next_runs_header() {
    let mut device = booted();
    let mut crumb = [0_u8; 8];
    let Ok(prepared) = planned(&mut device).prepare(&mut device) else {
        unreachable!("this device accepts an erase")
    };

    let Err(refusal) = prepared.stage(&mut device, &mut crumb) else {
        unreachable!("a header does not fit eight bytes")
    };
    assert!(
        matches!(refusal, SwapFailure::Encode(_)),
        "a page too small is the caller's buffer, not the media: {refusal:?}"
    );
}

/// A device that fails its `n`th mutation leaves the old run authoritative.
fn fails_at(accepts: usize) {
    let mut device = Nor::new(geometry());
    let stale = BankHeader {
        run: RunId(0x1111_1111_1111_1111),
        input: b"a-run-two-generations-ago",
        ..current_header()
    };
    install(&mut device, BankId::B, Generation(3), &stale);
    install(&mut device, BankId::A, CURRENT, &current_header());
    device.ops.clear();
    let mut device = device.failing_after(accepts);

    let mut page = [0_u8; PAGE];
    let outcome = planned(&mut device)
        .prepare(&mut device)
        .and_then(|prepared| prepared.stage(&mut device, &mut page))
        .and_then(|staged| staged.payload_barrier(&mut device))
        .and_then(|sealable| sealable.commit(&mut device));
    assert!(
        matches!(outcome, Err(SwapFailure::Storage(_))),
        "the device refused mutation {accepts} and the swap carried on"
    );

    // Whatever the step, the old run is still the one a reader boots: a swap that has not
    // sealed its new bank has not happened. §10: "a crash before step 5 recovers the old
    // run."
    assert_eq!(
        authority(&mut device),
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT
        },
        "a swap that failed at mutation {accepts} moved the authority"
    );
}

#[test]
fn a_swap_that_fails_before_its_seal_leaves_the_old_run_authoritative() {
    // Every mutation of steps 2 to 5 in turn: the erase, its barrier, the header, its
    // barrier, and the seal. The last is the sharpest — a seal that was refused is a bank
    // that is written and not authoritative, which is §02 decision 7 exactly.
    for accepts in 0..5 {
        fails_at(accepts);
    }
}

#[test]
fn a_swap_error_says_which_refusal_it_is() {
    // The same contract every other error in this workspace keeps: a device with no
    // debugger attached still has to be able to say which refusal it met.
    let messages = [
        SwapError::NoAuthority,
        SwapError::GenerationExhausted,
        SwapError::RunReused,
        SwapError::NotTheActiveBank,
        SwapError::Region(RegionError::NoJournalRoom),
    ]
    .map(SwapError::message);

    for (index, message) in messages.iter().enumerate() {
        assert!(!message.is_empty());
        assert!(message.is_ascii());
        assert!(message.len() < 64, "{message:?} is longer than a log line");
        assert!(
            !messages
                .iter()
                .enumerate()
                .any(|(other, twin)| other != index && twin == message),
            "{message:?} is two refusals"
        );
    }
    assert_eq!(
        std::format!("{}", SwapError::RunReused),
        SwapError::RunReused.message(),
        "`Display` writes the message and nothing else"
    );
}
