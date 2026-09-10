//! The book's code samples.
//!
//! Issue [#42](https://github.com/madmax983/waymaker/issues/42) asks that the book's samples
//! be "tested, not merely quoted". This is the file the book quotes: every sample is an
//! `ANCHOR` region here, a chapter shows it with an `include` directive, and the `test`
//! stage compiles and runs it. A sample that stopped working is a red pipeline rather than
//! a wrong page.
//!
//! The tie is the anchor's *name*. `cargo xtask check-layering`'s `book` rule refuses an
//! anchor with no `#[test]` of the same name in this file, and refuses a chapter that
//! carries a Rust fence of its own — because mdBook renders an anchor it cannot find as
//! nothing at all, reports no error, and exits zero.
//!
//! Everything below runs against `waymaker-fault`'s model of NOR, the real driver and the
//! real codec. Nothing here is a fixture that agrees with the engine by construction.

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, Outcome, RecordKind, RunId};
use waymaker_drive::{
    Activities, Boundary, Clocks, Conclusion, Driver, DurableIntent, Identity, Performed, Progress,
    Scratch, Suspended, Workflow,
};
use waymaker_embassy::clock::PersistentClock;
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::{Bounds, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// The run every sample below belongs to.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// Fetch bytes from outside the device.
const FETCH: ActivityKind = ActivityKind(1);

/// Ask the world what time it is.
const NOW: ActivityKind = ActivityKind(2);

/// The workflow's kind, as its `RunStarted` record carries it.
const KIND: u16 = 42;

/// The workflow's version.
const VERSION: u16 = 1;

/// What [`Fetch`] is asked to fetch.
const URL: &[u8] = b"url";

/// What [`World`] answers a [`FETCH`] with.
const BODY: &[u8] = b"contents-of-the-thing";

/// The reading [`World`] starts with.
const FIRST_READING: u64 = 1_700_000_000;

/// What these runs declare their records may be worth, for design document §10's reserve.
const BOUNDS: Bounds = Bounds {
    run_input_bytes: 4,
    effect_result_bytes: 32,
    terminal_bytes: 32,
};

/// Copies as much of `src` into `dst` as fits, and says how much that was.
fn copy(src: &[u8], dst: &mut [u8]) -> usize {
    let taken = src.len().min(dst.len());
    let (Some(from), Some(into)) = (src.get(..taken), dst.get_mut(..taken)) else {
        return 0;
    };
    into.copy_from_slice(from);
    taken
}

/// A 4 KiB part with 1 KiB erase blocks, four-byte programs and byte reads.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 1024, 4, 1) else {
        unreachable!("4096/1024/4/1 is a legal geometry")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 1024, align) else {
        unreachable!("a 1024-byte region at offset 0 fits this geometry")
    };
    region
}

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds four erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// The world outside the device: two activities, and a count of what it was asked.
struct World {
    /// What [`NOW`] answers. A sample moves it between boots.
    clock: u64,
    /// How many effects this world was asked to perform.
    performed: usize,
}

impl World {
    const fn new() -> Self {
        Self {
            clock: FIRST_READING,
            performed: 0,
        }
    }
}

impl Activities for World {
    fn perform(
        &mut self,
        _intent: DurableIntent,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Performed {
        self.performed += 1;
        let reading = self.clock.to_le_bytes();
        let answer: &[u8] = if kind == FETCH { BODY } else { &reading };
        if answer.len() > out.len() {
            return Performed::Exhausted;
        }
        Performed::Completed(copy(answer, out))
    }
}

impl Clocks for World {
    fn capability(&self) -> ClockCapability {
        // These samples arm no deadline, so the honest answer is the weaker capability.
        ClockCapability::BootOnly
    }

    fn now(&mut self, _kind: ClockKind) -> Option<u64> {
        None
    }
}

/// Drives `workflow` to completion over `device`, and reports what it concluded with.
fn boot<W: Workflow>(
    device: &mut Device,
    world: &mut World,
    workflow: &mut W,
) -> (Conclusion, Vec<u8>) {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 32];
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("the runs in this file complete")
    };
    let Progress::Finished {
        conclusion,
        result_len,
    } = progress
    else {
        unreachable!("the runs in this file finish rather than suspending")
    };
    (
        conclusion,
        result.get(..result_len).unwrap_or_default().to_vec(),
    )
}

/// Every record the journal holds, by kind, in the order recovery reads them.
fn recorded_kinds(device: &mut Device) -> Vec<RecordKind> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut kinds = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these samples write are legal")
        };
        kinds.push(record.kind());
    }
    kinds
}

// ANCHOR: a_workflow_is_a_value_with_a_method
/// Fetch a URL and report what came back.
struct Fetch {
    body: [u8; 32],
    body_len: usize,
}

impl Workflow for Fetch {
    /// What this run is, as its opening record carries it.
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: KIND,
            versions: VersionRange::exact(VERSION),
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        // `?` is where the run suspends. On the next boot this line is reached again and
        // the recorded answer is returned, without the world being asked.
        let body = match boundary.call(FETCH, URL)? {
            Outcome::Completed(bytes) => bytes,
            Outcome::Failed(_) => return Ok(Outcome::Failed(b"fetch")),
        };
        // The answer is borrowed from the driver's buffer, which the next boundary call
        // overwrites. A workflow keeps what it needs before then.
        self.body_len = copy(body, &mut self.body);
        Ok(Outcome::Completed(
            self.body.get(..self.body_len).unwrap_or_default(),
        ))
    }
}
// ANCHOR_END: a_workflow_is_a_value_with_a_method

impl Fetch {
    const fn new() -> Self {
        Self {
            body: [0; 32],
            body_len: 0,
        }
    }
}

#[test]
fn a_workflow_is_a_value_with_a_method() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut workflow = Fetch::new();

    let (conclusion, result) = boot(&mut device, &mut world, &mut workflow);

    assert_eq!(conclusion, Conclusion::Completed);
    assert_eq!(result, BODY);
}

// ANCHOR: a_replayed_run_asks_the_world_nothing
#[test]
fn a_replayed_run_asks_the_world_nothing() {
    let mut device = Device::new(geometry());
    let mut world = World::new();

    // The first boot performs the effect and commits its outcome.
    let (_, first) = boot(&mut device, &mut world, &mut Fetch::new());
    assert_eq!(world.performed, 1);

    // The second boot re-creates the workflow from its beginning, over the same media,
    // from a value that has run nothing. Replay answers the effect out of the journal, so
    // the world is never asked again.
    let (conclusion, replayed) = boot(&mut device, &mut world, &mut Fetch::new());

    assert_eq!(conclusion, Conclusion::Completed);
    assert_eq!(replayed, first);
    assert_eq!(world.performed, 1);
}
// ANCHOR_END: a_replayed_run_asks_the_world_nothing

/// Ask the world what time it is, and report the reading.
struct Stamp {
    reading: [u8; 8],
}

impl Stamp {
    const fn new() -> Self {
        Self { reading: [0; 8] }
    }
}

impl Workflow for Stamp {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: KIND,
            versions: VersionRange::exact(VERSION),
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        // A workflow does not read a clock. It asks for a reading through an effect, and
        // the answer is recorded — so every later replay sees the instant the first
        // execution saw.
        let now = match boundary.call(NOW, &[])? {
            Outcome::Completed(bytes) => bytes,
            Outcome::Failed(_) => return Ok(Outcome::Failed(b"clock")),
        };
        let taken = copy(now, &mut self.reading);
        Ok(Outcome::Completed(
            self.reading.get(..taken).unwrap_or_default(),
        ))
    }
}

// ANCHOR: a_reading_of_the_world_is_recorded_rather_than_taken_again
#[test]
fn a_reading_of_the_world_is_recorded_rather_than_taken_again() {
    let mut device = Device::new(geometry());
    let mut world = World::new();

    let (_, stamped) = boot(&mut device, &mut world, &mut Stamp::new());
    assert_eq!(stamped, FIRST_READING.to_le_bytes());

    // The world moves on. A workflow that read a clock directly would take a different
    // branch on the next boot and diverge; this one asked through an effect, so the
    // reading it saw is on media.
    world.clock = FIRST_READING + 200_000_000;

    let (conclusion, replayed) = boot(&mut device, &mut world, &mut Stamp::new());

    assert_eq!(conclusion, Conclusion::Completed);
    assert_eq!(replayed, stamped);
    assert_eq!(world.performed, 1);
}
// ANCHOR_END: a_reading_of_the_world_is_recorded_rather_than_taken_again

// ANCHOR: a_journal_records_the_intent_before_the_outcome
#[test]
fn a_journal_records_the_intent_before_the_outcome() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    boot(&mut device, &mut world, &mut Fetch::new());

    // Design document §07: the schedule record crosses a durability barrier before the
    // world is asked, and the outcome is written after the world has answered. On media
    // that is one order and no other.
    assert_eq!(
        recorded_kinds(&mut device),
        [
            RecordKind::RUN_STARTED,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::RUN_COMPLETED,
        ]
    );
}
// ANCHOR_END: a_journal_records_the_intent_before_the_outcome

// ANCHOR: a_storage_adapter_is_four_operations_and_a_barrier
/// A 4 KiB part addressed directly: 1 KiB erase blocks, four-byte programs, byte reads.
///
/// Design document §12's contract in full. `read`, `program` and `erase` act on exactly
/// the region they name, `barrier` changes no media, and every one of them validates
/// against the geometry *before* touching the part. Programming only clears bits, because
/// that is what NOR does.
struct OneChip {
    cells: [u8; 4096],
}

impl StableStorage for OneChip {
    type Error = GeometryError;

    fn geometry(&self) -> Geometry {
        geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let at = self.validated(offset, dst.len(), Operation::Read)?;
        let Some(source) = self.cells.get(at..at.saturating_add(dst.len())) else {
            return Err(GeometryError::OutOfBounds);
        };
        dst.copy_from_slice(source);
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let at = self.validated(offset, src.len(), Operation::Program)?;
        let Some(target) = self.cells.get_mut(at..at.saturating_add(src.len())) else {
            return Err(GeometryError::OutOfBounds);
        };
        for (cell, byte) in target.iter_mut().zip(src) {
            // A program clears bits and never sets them. A driver that assigned instead
            // would hide a caller writing over a record it had already committed.
            *cell &= *byte;
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let at = self.validated(offset, len as usize, Operation::Erase)?;
        let Some(target) = self.cells.get_mut(at..at.saturating_add(len as usize)) else {
            return Err(GeometryError::OutOfBounds);
        };
        target.fill(ERASED);
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        // Nothing to do for a part addressed directly. A driver with a write buffer, a
        // cache or a command queue flushes it here, and returns only once every earlier
        // mutation would survive a reset.
        Ok(())
    }
}
// ANCHOR_END: a_storage_adapter_is_four_operations_and_a_barrier

/// The erased state of a NOR cell.
const ERASED: u8 = 0xFF;

/// Which of the three units an operation is measured against.
#[derive(Clone, Copy)]
enum Operation {
    Read,
    Program,
    Erase,
}

impl OneChip {
    const fn new() -> Self {
        Self {
            cells: [ERASED; 4096],
        }
    }

    /// The offset as an index, once the geometry has accepted the region.
    fn validated(
        &self,
        offset: u32,
        len: usize,
        operation: Operation,
    ) -> Result<usize, GeometryError> {
        let Ok(len) = u32::try_from(len) else {
            return Err(GeometryError::OutOfBounds);
        };
        let geometry = self.geometry();
        match operation {
            Operation::Read => geometry.validate_read(offset, len),
            Operation::Program => geometry.validate_program(offset, len),
            Operation::Erase => geometry.validate_erase(offset, len),
        }?;
        usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)
    }
}

#[test]
fn a_storage_adapter_is_four_operations_and_a_barrier() {
    let mut part = OneChip::new();

    // The engine's own writer and reader over the adapter, rather than an assertion about
    // the adapter alone: a record written through it is a record recovery reads back.
    let mut world = World::new();
    let mut workflow = Fetch::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 32];
    let progress = Driver::new(region(), RUN, reserve())
        .boot(
            &mut part,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect("the run completes over a hand-written adapter");
    assert!(matches!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Completed,
            ..
        }
    ));

    // And the three refusals, each taken before the part is touched.
    assert!(
        part.read(1, &mut [0; 4]).is_ok(),
        "the read unit is one byte"
    );
    assert!(
        part.program(1, &[0; 4]).is_err(),
        "the program unit is four"
    );
    assert!(part.erase(0, 1).is_err(), "the erase unit is one block");
}

// ANCHOR: a_persistent_clock_is_a_reading_and_a_bit
/// A real-time counter in a battery- or supercapacitor-backed domain.
///
/// Two register reads, and the second one is the whole point. A backup domain that lost
/// power leaves the counter at its reset value, which is below every instant a workflow
/// ever waits for — so a driver that reported the number would fire every persistent
/// deadline on the device at once, with no checksum failing and no record malformed.
struct BackedCounter {
    /// The counter register.
    counter: u64,
    /// Whether the backup domain held while the supply was away.
    held: bool,
}

impl PersistentClock for BackedCounter {
    type Error = &'static str;

    fn now(&mut self) -> Result<u64, Self::Error> {
        // The counter is read first, so that a supply which sagged during the read is
        // still caught by the bit that is checked after it.
        let reading = self.counter;
        if self.held {
            Ok(reading)
        } else {
            Err("the backup domain lost power, so this counter is not a time")
        }
    }
}
// ANCHOR_END: a_persistent_clock_is_a_reading_and_a_bit

#[test]
fn a_persistent_clock_is_a_reading_and_a_bit() {
    let mut clock = BackedCounter {
        counter: FIRST_READING,
        held: true,
    };
    assert_eq!(clock.now(), Ok(FIRST_READING));

    // The battery died. The counter still reads *something*, and it is not a time.
    let mut lost = BackedCounter {
        counter: 0,
        held: false,
    };
    assert!(lost.now().is_err());
}
