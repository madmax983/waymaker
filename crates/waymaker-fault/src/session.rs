//! The storage a writer under test is handed, and the run it produces.
//!
//! A [`Session`] is three things at once, and they are together because they have to agree:
//! it is a [`StableStorage`] the writer drives, it is the recorder of the write sequence
//! [`crate::injections`] enumerates, and it is where at most one [`Injection`] is armed. A
//! recorder that could disagree with the injector about which operation was number three
//! would make every crash point off by one.
//!
//! # After power loss
//!
//! The session is dead. Every later call returns [`FaultError::PowerLoss`], records no
//! operation, and touches no media — including `read`, because a reader that could still
//! see the device has power. A writer with a retry loop in it therefore cannot walk past
//! the crash point, which is the property that makes a torn-write image an image a reset
//! could really produce.

use core::fmt;

use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

use crate::device::{Device, FaultError, OneWayBits};
use crate::inject::{Injection, Interruption, Op, Progress};
use crate::model::{Durability, Ledger, RecordId};

/// The storage handed to a writer under test.
#[derive(Clone, Debug)]
pub struct Session {
    device: Device,
    injection: Option<Injection>,
    ops: Vec<Op>,
    /// Whether operation `i` actually changed a cell. Index-aligned with `ops`.
    ///
    /// The change, not the call. Programming `0xFF` over erased media offers four bytes and
    /// alters nothing, and so does erasing a block that is already erased; a record whose
    /// only mutation was one of those has nothing on media, and calling it durable would
    /// oblige recovery to produce something that is not there.
    touched: Vec<bool>,
    /// Whether operation `i` left media differing from what completing it would have left.
    ///
    /// A record with one of these in it is *torn*: part of what it meant to put on media is
    /// there and the rest is not. Design document §15 permits recovery to include "an
    /// unacknowledged **complete** record", and half a record is not one — so a torn record
    /// can never be acknowledged, whatever barrier follows it, and recovery producing one is
    /// a breach.
    ///
    /// The comparison is against the *media*, not against the byte count. A frame is padded
    /// to the program granularity with `0xFF`, and programming `0xFF` over erased media
    /// changes nothing — so a write interrupted inside its own padding leaves exactly the
    /// bytes a completed write would have, and calling that torn would fail a recovery that
    /// is entirely correct.
    torn: Vec<bool>,
    /// Indices of barriers that completed durably.
    barriers: Vec<usize>,
    /// `(record, index of the first operation that could belong to it)`. A `None` record
    /// is a *close*: it ends the span before it and starts one that belongs to nothing.
    marks: Vec<(Option<RecordId>, usize)>,
    powered: bool,
}

impl Session {
    pub(crate) const fn new(device: Device, injection: Option<Injection>) -> Self {
        Self {
            device,
            injection,
            ops: Vec::new(),
            touched: Vec::new(),
            torn: Vec::new(),
            barriers: Vec::new(),
            marks: Vec::new(),
            powered: true,
        }
    }

    /// Declares that the operations from here on belong to `id`.
    ///
    /// # Preconditions
    ///
    /// `id` is distinct within one run. A repeated id is not rejected here — there is
    /// nothing to return it through, and a panic in a harness is a worse failure than a
    /// wrong answer — but [`crate::verify_recovery`] refuses a ledger that contains one, so
    /// it fails at the assertion rather than silently.
    pub fn begin_record(&mut self, id: RecordId) {
        self.marks.push((Some(id), self.ops.len()));
    }

    /// Declares that the operations from here on belong to no record.
    ///
    /// The counterpart of [`begin_record`](Self::begin_record), and the reason it exists is
    /// a silent weakening rather than a missing convenience. An operation belongs to
    /// whichever record was open when it was issued, so housekeeping after a record's
    /// barrier — an erase of the other bank, a scratch write — would otherwise fall inside
    /// that record and leave it with an unordered mutation in it. The record would drop
    /// from [`Durability::Acknowledged`] to [`Durability::PossiblyDurable`], and
    /// [`crate::verify_recovery`] would stop *requiring* a record recovery must not lose.
    /// That is the direction a check must never fail in, and it would show up as nothing at
    /// all.
    ///
    /// Calling it with no record open is a no-op that costs nothing.
    pub fn end_record(&mut self) {
        self.marks.push((None, self.ops.len()));
    }

    /// The bytes as they stand, which is what a reader after a reset would see.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        self.device.image()
    }

    /// The injection armed in this session, if any.
    #[must_use]
    pub const fn injection(&self) -> Option<Injection> {
        self.injection
    }

    /// Turns the session into the run it performed.
    pub(crate) fn finish(self) -> Run {
        let ledger = self.ledger();
        Run {
            injection: self.injection,
            image: self.device.into_image(),
            ops: self.ops,
            ledger,
        }
    }

    /// The state each declared record ended in.
    fn ledger(&self) -> Ledger {
        let entries = self
            .marks
            .iter()
            .enumerate()
            .filter_map(|(position, (id, start))| {
                let id = (*id)?;
                let end = self
                    .marks
                    .get(position.wrapping_add(1))
                    .map_or(self.ops.len(), |(_, next)| *next);
                let torn =
                    (*start..end.max(*start)).any(|index| self.torn.get(index) == Some(&true));
                Some((id, self.durability_of(*start, end, torn), torn))
            })
            .collect();
        Ledger::new(entries)
    }

    /// The state of a record whose operations are `start..end`.
    fn durability_of(&self, start: usize, end: usize, torn: bool) -> Durability {
        let range = || start..end.max(start);

        if !range().any(|index| self.touched.get(index) == Some(&true)) {
            return Durability::Attempted;
        }

        // A barrier orders whatever is on media, and what is on media is half a record. The
        // writer may not even know: an injected failure hands it an error and lets it carry
        // on to the barrier. Acknowledging that would oblige recovery to produce a frame no
        // integrity check would accept.
        if torn {
            return Durability::PossiblyDurable;
        }

        // A barrier acknowledges a record only if it completed after every one of that
        // record's *mutations*. Taking the last mutation rather than the last operation is
        // what lets a record that ends in its own barrier be acknowledged by it.
        let last_mutation = range().rev().find(|index| {
            matches!(
                self.ops.get(*index),
                Some(Op::Program { .. } | Op::Erase { .. })
            )
        });

        match last_mutation {
            Some(mutation) if self.barriers.iter().any(|barrier| *barrier > mutation) => {
                Durability::Acknowledged
            }
            _ => Durability::PossiblyDurable,
        }
    }

    /// The injection armed for the operation about to be recorded, if it is this one.
    fn armed_for(&self, index: usize) -> Option<Injection> {
        self.injection.filter(|injection| injection.op == index)
    }

    /// How many bytes of an operation of `len` bytes `progress` describes.
    const fn landed(progress: Progress, len: u32) -> u32 {
        match progress {
            Progress::None => 0,
            Progress::Bytes(bytes) if bytes < len => bytes,
            Progress::Bytes(_) | Progress::Whole => len,
        }
    }

    /// The error an `interruption` reports, and the power state it leaves behind.
    const fn interrupt(&mut self, interruption: Interruption) -> FaultError {
        match interruption {
            Interruption::PowerLoss => {
                self.powered = false;
                FaultError::PowerLoss
            }
            Interruption::Failure => FaultError::InjectedFailure,
        }
    }
}

impl StableStorage for Session {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        if !self.powered {
            return Err(FaultError::PowerLoss);
        }
        self.device.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        if !self.powered {
            return Err(FaultError::PowerLoss);
        }
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        // Refused before anything is recorded, exactly as an unfaulted device refuses it: a
        // call that never reached media is not an operation a crash point can be inside.
        self.device.geometry().validate_program(offset, len)?;
        if self.device.bit_rule() == OneWayBits::Rejected
            && self.device.would_set_a_bit(offset, src)
        {
            return Err(FaultError::BitSetWithoutErase);
        }

        let index = self.ops.len();
        self.ops.push(Op::Program { offset, len });
        let landed = self
            .armed_for(index)
            .map_or(len, |injection| Self::landed(injection.progress, len));
        let written = src.get(..landed as usize).unwrap_or(src);
        // Torn is "some of what this write meant to change is on media and the rest is
        // not", measured against the *media* rather than against the byte count. Both
        // halves matter: a write that changed nothing is not torn, it never started, and
        // the bytes that did not land may be the ones that would have changed nothing —
        // a frame is padded with `0xFF`, and programming `0xFF` over erased media is the
        // identity, so a write interrupted inside its own padding leaves exactly what a
        // completed write would have.
        let landed_something = self.device.apply_program(offset, written);
        let withheld = src.get(landed as usize..).unwrap_or_default();
        self.touched.push(landed_something);
        self.torn.push(
            landed_something
                && self
                    .device
                    .program_would_change(offset.wrapping_add(landed), withheld),
        );

        self.armed_for(index).map_or(Ok(()), |injection| {
            Err(self.interrupt(injection.interruption))
        })
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        if !self.powered {
            return Err(FaultError::PowerLoss);
        }
        self.device.geometry().validate_erase(offset, len)?;

        let index = self.ops.len();
        self.ops.push(Op::Erase { offset, len });
        let landed = self
            .armed_for(index)
            .map_or(len, |injection| Self::landed(injection.progress, len));
        let landed_something = self.device.apply_erase(offset, landed);
        self.touched.push(landed_something);
        self.torn.push(
            landed_something
                && self
                    .device
                    .erase_would_change(offset.wrapping_add(landed), len.wrapping_sub(landed)),
        );

        self.armed_for(index).map_or(Ok(()), |injection| {
            Err(self.interrupt(injection.interruption))
        })
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        if !self.powered {
            return Err(FaultError::PowerLoss);
        }

        let index = self.ops.len();
        self.ops.push(Op::Barrier);
        self.touched.push(false);
        self.torn.push(false);

        match self.armed_for(index) {
            // Power lost *after* the barrier returned: the ordering was established, so
            // everything it covered is durable and recovery is required to find it. The
            // caller never learned, which is the whole point of enumerating this crash
            // point separately from the one before the barrier.
            Some(injection) if injection.interruption == Interruption::PowerLoss => {
                if injection.progress == Progress::Whole {
                    self.barriers.push(index);
                }
                Err(self.interrupt(Interruption::PowerLoss))
            }
            // A barrier that returned an error establishes nothing a caller may rely on,
            // whatever really happened on the wire.
            Some(injection) => Err(self.interrupt(injection.interruption)),
            None => {
                self.barriers.push(index);
                Ok(())
            }
        }
    }
}

/// One execution of a writer, with at most one crash point in it.
#[derive(Clone, Debug)]
pub struct Run {
    injection: Option<Injection>,
    image: Vec<u8>,
    ops: Vec<Op>,
    ledger: Ledger,
}

impl Run {
    /// The crash point this run carried, or `None` for the fault-free run.
    #[must_use]
    pub const fn injection(&self) -> Option<Injection> {
        self.injection
    }

    /// The media as a reset would find it.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.image
    }

    /// The write sequence the writer actually issued in this run.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// What recovery is allowed, and required, to do with each record.
    #[must_use]
    pub const fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

/// Runs a writer once with no faults, and then once per crash point.
///
/// # Why the writer is a closure rather than a recorded sequence
///
/// Because [`Interruption::Failure`] hands the writer an error and lets it carry on, and what it
/// does next is the thing being tested. Replaying a recorded byte log could not express a
/// retry. The cost is that the writer is run `1 + injections` times and must be
/// deterministic; the benefit is that "failed or interrupted `program`/`erase`" is a real
/// case rather than a documented gap.
#[derive(Clone, Copy, Debug)]
pub struct Harness {
    geometry: Geometry,
    bits: OneWayBits,
}

impl Harness {
    /// A harness over `geometry`, with the bit rule hardware has.
    #[must_use]
    pub const fn new(geometry: Geometry) -> Self {
        Self {
            geometry,
            bits: OneWayBits::Absorbed,
        }
    }

    /// A harness whose devices report one-way bit violations the way `rule` says.
    #[must_use]
    pub const fn with_bit_rule(geometry: Geometry, rule: OneWayBits) -> Self {
        Self {
            geometry,
            bits: rule,
        }
    }

    /// The geometry every device in this harness has.
    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Every run: the fault-free one first, then one per crash point, in enumeration order.
    ///
    /// # Errors
    ///
    /// [`HarnessError::WriterFailedWithNoFaultsArmed`] if the writer returned an error in
    /// the run where nothing was injected, and
    /// [`HarnessError::WriterIsNotDeterministic`] if some run's operations before its crash
    /// point differ from the fault-free run's.
    ///
    /// Both are refusals rather than results, and that is the point. The enumeration is
    /// taken from the fault-free run's write sequence, so a writer that gives up early
    /// there — a misaligned offset, a buffer too small, a geometry that cannot hold what it
    /// wanted to write — produces a short sequence, a handful of crash points, and a suite
    /// in which every assertion passes because nothing was checked. A measurement that did
    /// not happen is not a measurement that passed.
    ///
    /// # Postconditions
    ///
    /// On success, `result[0].injection().is_none()`, and `result[1..]` carries exactly
    /// `injections(result[0].ops(), geometry)`, in that order — so the number of runs is a
    /// fact about the write sequence rather than a sampling budget.
    pub fn run<W, E>(&self, mut writer: W) -> Result<Vec<Run>, HarnessError>
    where
        W: FnMut(&mut Session) -> Result<(), E>,
        E: fmt::Debug,
    {
        let baseline = self.fault_free(&mut writer)?;
        let sequence = baseline.ops.clone();

        let mut runs = vec![baseline];
        for injection in crate::inject::injections(&sequence, self.geometry) {
            runs.push(self.injected(injection, &sequence, &mut writer)?);
        }
        Ok(runs)
    }

    /// One run, with `injection` armed and nothing else.
    ///
    /// [`run`](Self::run) is this in a loop over [`injections`](crate::injections). It
    /// exists on its own because reproducing one failure should not cost the whole
    /// enumeration: a three-record journal is over a hundred runs, and the loop a
    /// contributor sits in while debugging is this one.
    ///
    /// # Errors
    ///
    /// As [`run`](Self::run). A crash point that never fires — an `op` past the end of the
    /// sequence, or one the writer did not reach — is reported as
    /// [`HarnessError::WriterIsNotDeterministic`], because against a deterministic writer
    /// that is the only way it can happen.
    pub fn run_one<W, E>(&self, injection: Injection, mut writer: W) -> Result<Run, HarnessError>
    where
        W: FnMut(&mut Session) -> Result<(), E>,
        E: fmt::Debug,
    {
        let baseline = self.fault_free(&mut writer)?;
        let sequence = baseline.ops;
        self.injected(injection, &sequence, &mut writer)
    }

    /// The run in which nothing is injected, which is where the write sequence comes from.
    fn fault_free<W, E>(&self, writer: &mut W) -> Result<Run, HarnessError>
    where
        W: FnMut(&mut Session) -> Result<(), E>,
        E: fmt::Debug,
    {
        let mut session = Session::new(Device::with_bit_rule(self.geometry, self.bits), None);
        match writer(&mut session) {
            Ok(()) => Ok(session.finish()),
            Err(error) => Err(HarnessError::WriterFailedWithNoFaultsArmed(format!(
                "{error:?}"
            ))),
        }
    }

    /// One run with `injection` armed, checked against the sequence it was enumerated from.
    fn injected<W, E>(
        &self,
        injection: Injection,
        sequence: &[Op],
        writer: &mut W,
    ) -> Result<Run, HarnessError>
    where
        W: FnMut(&mut Session) -> Result<(), E>,
    {
        let mut session = Session::new(
            Device::with_bit_rule(self.geometry, self.bits),
            Some(injection),
        );
        drop(writer(&mut session));
        let run = session.finish();

        // Nothing has gone wrong yet at the moment the crash point fires, so every
        // operation up to and including it must be the one the enumeration was computed
        // from. Where they differ, the writer is not a function of the storage it was
        // handed, and the crash points are aimed at operations that are not there.
        let through = injection.op.saturating_add(1);
        let planned = sequence.get(..through);
        let actual = run.ops.get(..through);
        if planned.is_none() || planned != actual {
            return Err(HarnessError::WriterIsNotDeterministic { injection });
        }
        Ok(run)
    }
}

/// A refusal to report crash points that were not really enumerated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HarnessError {
    /// The writer returned an error in the run where nothing was injected.
    ///
    /// Carries the `Debug` rendering of that error, because the harness is generic over it
    /// and has nowhere to put the value itself.
    WriterFailedWithNoFaultsArmed(String),
    /// A run's operations before its crash point differ from the fault-free run's.
    WriterIsNotDeterministic {
        /// The crash point whose run diverged.
        injection: Injection,
    },
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterFailedWithNoFaultsArmed(error) => write!(
                formatter,
                "the writer failed with no faults armed, so the write sequence the crash \
                 points were enumerated from is not the one it meant to issue: {error}"
            ),
            Self::WriterIsNotDeterministic { injection } => write!(
                formatter,
                "the writer issued different operations before {injection:?} than it did \
                 with no faults armed, so the crash points are aimed at operations that are \
                 not there"
            ),
        }
    }
}

impl core::error::Error for HarnessError {}
