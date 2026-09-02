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

use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

use crate::device::{Device, FaultError, OneWayBits};
use crate::inject::{Effect, Injection, Op, Progress};
use crate::model::{Durability, Ledger, RecordId};

/// The storage handed to a writer under test.
#[derive(Clone, Debug)]
pub struct Session {
    device: Device,
    injection: Option<Injection>,
    ops: Vec<Op>,
    /// Whether operation `i` put anything at all on media. Index-aligned with `ops`.
    touched: Vec<bool>,
    /// Indices of barriers that completed durably.
    barriers: Vec<usize>,
    /// `(record, index of the first operation that could belong to it)`.
    marks: Vec<(RecordId, usize)>,
    powered: bool,
}

impl Session {
    pub(crate) const fn new(device: Device, injection: Option<Injection>) -> Self {
        Self {
            device,
            injection,
            ops: Vec::new(),
            touched: Vec::new(),
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
        self.marks.push((id, self.ops.len()));
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
            .map(|(position, (id, start))| {
                let end = self
                    .marks
                    .get(position.wrapping_add(1))
                    .map_or(self.ops.len(), |(_, next)| *next);
                (*id, self.durability_of(*start, end))
            })
            .collect();
        Ledger::from_entries(entries)
    }

    /// The state of a record whose operations are `start..end`.
    fn durability_of(&self, start: usize, end: usize) -> Durability {
        let range = || start..end.max(start);

        if !range().any(|index| self.touched.get(index) == Some(&true)) {
            return Durability::Attempted;
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

    /// The error an injected `effect` reports, and the power state it leaves behind.
    const fn interrupt(&mut self, effect: Effect) -> FaultError {
        match effect {
            Effect::PowerLoss => {
                self.powered = false;
                FaultError::PowerLoss
            }
            Effect::Failure => FaultError::Injected,
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
        self.device.apply_program(offset, written);
        self.touched.push(!written.is_empty());

        self.armed_for(index)
            .map_or(Ok(()), |injection| Err(self.interrupt(injection.effect)))
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
        self.device.apply_erase(offset, landed);
        self.touched.push(landed > 0);

        self.armed_for(index)
            .map_or(Ok(()), |injection| Err(self.interrupt(injection.effect)))
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        if !self.powered {
            return Err(FaultError::PowerLoss);
        }

        let index = self.ops.len();
        self.ops.push(Op::Barrier);
        self.touched.push(false);

        match self.armed_for(index) {
            // Power lost *after* the barrier returned: the ordering was established, so
            // everything it covered is durable and recovery is required to find it. The
            // caller never learned, which is the whole point of enumerating this crash
            // point separately from the one before the barrier.
            Some(injection) if injection.effect == Effect::PowerLoss => {
                if injection.progress == Progress::Whole {
                    self.barriers.push(index);
                }
                Err(self.interrupt(Effect::PowerLoss))
            }
            // A barrier that returned an error establishes nothing a caller may rely on,
            // whatever really happened on the wire.
            Some(injection) => Err(self.interrupt(injection.effect)),
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
/// Because [`Effect::Failure`] hands the writer an error and lets it carry on, and what it
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
            bits: OneWayBits::Nor,
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
    /// # Postconditions
    ///
    /// `result[0].injection().is_none()`, and `result[1..]` carries exactly
    /// `injections(result[0].ops(), geometry)`, in that order — so the number of runs is a
    /// fact about the write sequence rather than a sampling budget.
    pub fn run<W, E>(&self, mut writer: W) -> Vec<Run>
    where
        W: FnMut(&mut Session) -> Result<(), E>,
    {
        let mut baseline = Session::new(Device::with_bit_rule(self.geometry, self.bits), None);
        drop(writer(&mut baseline));
        let sequence = baseline.ops.clone();

        let mut runs = vec![baseline.finish()];
        for injection in crate::inject::injections(&sequence, self.geometry) {
            let mut session = Session::new(
                Device::with_bit_rule(self.geometry, self.bits),
                Some(injection),
            );
            drop(writer(&mut session));
            runs.push(session.finish());
        }
        runs
    }
}
