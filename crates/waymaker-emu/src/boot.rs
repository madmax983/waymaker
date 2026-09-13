//! What the emulated image does between the reset vector and the exit code.
//!
//! Three things, in this order, because each one is only worth anything if the one before it
//! held:
//!
//! 1. **The media model is interrogated.** `waymaker-conformance`'s suite is run over
//!    [`Nor`] through the `embedded-storage` port. That crate is `#![no_std]` and
//!    allocation-free precisely so an adapter author can run it *on the target the driver is
//!    for*, and this is the first place in the workspace that takes it up on the offer. A rig
//!    run over a model that had never been asked whether it obeys design document §12 would
//!    be a rig run over an unknown quantity.
//! 2. **The rig runs, is cut, and is judged.** [`Rig::prepare`], then per iteration
//!    [`Rig::iterate`] with the plan's own cut armed, then [`Rig::verify`], then
//!    [`Rig::resume`], then [`Rig::verify`] again. Every verdict must be
//!    [`Outcome::Passed`].
//! 3. **A census is required to be complete.** A boot that ran nothing exits the same way a
//!    boot that ran everything does, so the counts are returned, printed, and checked by the
//!    harness that started the emulator. That is [`Census::complete`].
//!
//! # What this establishes, and what it does not
//!
//! It establishes that the rig's code *executes* on ARMv6-M and on ARMv7E-M: every branch it
//! takes here is a branch a Cortex-M0 and a Cortex-M4 really retired, its `u64` arithmetic
//! went through the compiler-builtin routines a core with no 64-bit ALU uses, and the image
//! linked — which `cargo build --lib` does not establish, because an rlib is never linked and
//! an `extern crate alloc` under one compiles clean.
//!
//! It establishes nothing about a board. There is no NOR part here, no supply to remove, no
//! reset-cause register and no backup domain; the "cut" is the host cut — the iteration stops
//! where it stands — and the RAM survives it. `docs::HARDWARE_TARGETS` stays `Not run`, and
//! [ADR 0040] is where that is argued rather than assumed.
//!
//! [ADR 0040]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md

use waymaker_conformance::nor::NorFlashStorage;
use waymaker_conformance::{CASE_COUNT, REQUIRED_ERASE_BLOCKS, Region};
use waymaker_flash::storage::StableStorage;
use waymaker_rig::cutter::{Dispatcher, PlannedCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Resumed, Rig, Stop};
use waymaker_rig::wear::Metered;

use crate::nor::{self, Nor};

/// What the `embedded-storage` port answers a refusal with.
///
/// Named once at module scope rather than inside [`rig`], where it is a `type` after a
/// statement: `Rig::new` is generic over the storage error it will one day report, and the
/// rig is constructed before any storage call has been made.
type PortError = <NorFlashStorage<&'static mut Nor> as StableStorage>::Error;

/// The seed the plan is drawn from.
///
/// Fixed rather than drawn from anything the emulator provides: a boot whose workload came
/// from a clock would fail differently on different runs, and a rig log line is meant to
/// carry the whole run. §15's randomisation is over the *iteration*, which the plan derives.
pub const SEED: u64 = 0x7761_796D_616B_6572;

/// How many iterations the boot drives.
///
/// Enough that the plan's cut lands in more than one phase — the point of the sweep is that
/// the cut moves — and small enough that every emulated run finishes in a second or so.
pub const ITERATIONS: u32 = 12;

/// How many effects each iteration's run schedules.
pub const EFFECTS: u16 = 3;

/// Why a boot could not conclude.
///
/// Every variant means the run proved nothing, which is why none of them is reported as a
/// pass: a boot that could not lay the part out has not told anybody that the rig works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// The four constants of [`crate::nor`] are not a geometry.
    Geometry,
    /// The `embedded-storage` port could not describe the modelled part.
    Port,
    /// The conformance region is not one this geometry permits.
    Region,
    /// The conformance suite could not be started.
    SuiteNotRun,
    /// The conformance suite ran and the media model broke a clause of design document §12.
    ///
    /// Not "the adapter is wrong" alone: it means the thing the rig is about to be run over
    /// does not behave like NOR, so nothing after it would have meant anything.
    MediaModel,
    /// The part could not be laid out for the rig.
    Layout,
    /// An iteration refused.
    Iterate,
    /// A verification refused.
    Verify,
    /// A resume refused.
    Resume,
    /// A verdict was a breach of one of design document §14's guarantees.
    Breach(Outcome),
    /// Every call returned and the census is not complete.
    ///
    /// The one that catches a boot which exits zero having done nothing.
    Census,
}

impl Trouble {
    /// A short static description, for the semihosting line.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Geometry => "the modelled part's four units are not a geometry",
            Self::Port => "the embedded-storage port cannot describe the modelled part",
            Self::Region => "the conformance region is not one this geometry permits",
            Self::SuiteNotRun => "the conformance suite could not be started",
            Self::MediaModel => "the media model broke a clause of the storage contract",
            Self::Layout => "the part cannot be laid out for the rig",
            Self::Iterate => "an iteration refused",
            Self::Verify => "a verification refused",
            Self::Resume => "a resume refused",
            Self::Breach(_) => "a verdict was a breach",
            Self::Census => "the boot completed and its census is not complete",
        }
    }
}

/// What the boot did, counted as it went.
///
/// Counted rather than assumed. A boot that returned early, or one whose loop never entered,
/// exits with the same status a complete one does, so the numbers are the evidence and
/// [`complete`](Self::complete) is what the exit code rests on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// How many conformance cases the suite reached and passed.
    pub cases_passed: u16,
    /// How many the geometry made unaskable.
    pub cases_exempt: u16,
    /// How many iterations were driven to a stop.
    pub iterations: u32,
    /// How many of them the plan's cutter fired in.
    pub cuts: u32,
    /// How many resumes carried a cut run to its end.
    pub resumes: u32,
    /// How many of those redelivered an effect whose schedule had no completion.
    pub redeliveries: u32,
    /// How many resumes found a journal with no append point.
    pub unextendable: u32,
    /// How many verdicts were [`Outcome::Passed`].
    pub verdicts_passed: u32,
    /// How many effects were dispatched, across every iteration and every resume.
    pub dispatched: u32,
}

impl Census {
    /// Whether this boot measured what it exists to measure.
    ///
    /// Five demands, and each one is a way a green run could otherwise mean nothing: every
    /// conformance case was reached, every iteration ran, the cutter fired at least once —
    /// without which the rig is only being asked about clean runs — every cut run was
    /// resumed, and every verdict passed.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.cases_passed as usize + self.cases_exempt as usize == CASE_COUNT
            && self.iterations == ITERATIONS
            && self.cuts > 0
            && self.resumes + self.unextendable == self.cuts
            && self.verdicts_passed == self.iterations + self.resumes
    }
}

/// Counts what the rig asked the world to do, and nothing else.
///
/// A `Vec` on the host; a counter here, because this runs where there is no allocator.
struct Counting {
    dispatched: u32,
}

impl Dispatcher for Counting {
    type Error = core::convert::Infallible;

    fn dispatch(&mut self, _effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        self.dispatched = self.dispatched.saturating_add(1);
        Ok(())
    }
}

/// Runs the conformance suite and then the rig over `part`, and returns what it counted.
///
/// # Errors
///
/// [`Trouble`], every variant of which means the boot proved nothing.
pub fn run(part: &mut Nor, page: &mut [u8]) -> Result<Census, Trouble> {
    let geometry = nor::geometry().map_err(|_| Trouble::Geometry)?;
    let mut census = Census::default();

    conformance(part, geometry, page, &mut census)?;
    // The suite programs and erases the region it was given, so the part goes back to erased
    // before the rig is laid out on it. A rig prepared over the suite's leavings would still
    // work — `prepare` erases what it uses — but it would be a different part from the one
    // the boot says it ran on.
    part.reset();
    rig(part, geometry, page, &mut census)?;

    if census.complete() {
        Ok(census)
    } else {
        Err(Trouble::Census)
    }
}

/// Asks the media model whether it obeys design document §12.
fn conformance(
    part: &mut Nor,
    geometry: waymaker_flash::storage::Geometry,
    page: &mut [u8],
    census: &mut Census,
) -> Result<(), Trouble> {
    let len = nor::ERASE_BYTES
        .checked_mul(REQUIRED_ERASE_BLOCKS)
        .ok_or(Trouble::Region)?;
    let region = Region::new(geometry, 0, len).map_err(|_| Trouble::Region)?;
    let mut storage = NorFlashStorage::new(&mut *part).map_err(|_| Trouble::Port)?;
    let report =
        waymaker_conformance::run(&mut storage, region, page).map_err(|_| Trouble::SuiteNotRun)?;

    report.verdict().map_err(|_| Trouble::MediaModel)?;
    for (_, outcome) in report.entries() {
        match outcome {
            waymaker_conformance::Outcome::Passed => {
                census.cases_passed = census.cases_passed.saturating_add(1);
            }
            waymaker_conformance::Outcome::NotApplicable(_) => {
                census.cases_exempt = census.cases_exempt.saturating_add(1);
            }
            // `verdict` above has already refused both of these; counting them would be a
            // second opinion about a question that is already settled.
            waymaker_conformance::Outcome::NotRun | waymaker_conformance::Outcome::Failed(_) => {
                return Err(Trouble::MediaModel);
            }
        }
    }
    Ok(())
}

/// Drives the rig's three moments over every iteration of the plan.
fn rig(
    part: &mut Nor,
    geometry: waymaker_flash::storage::Geometry,
    page: &mut [u8],
    census: &mut Census,
) -> Result<(), Trouble> {
    let mut storage = NorFlashStorage::new(&mut *part).map_err(|_| Trouble::Port)?;
    let rig = Rig::new::<PortError>(geometry, Plan::new(SEED), EFFECTS).map_err(|_| {
        // Every variant here is the same statement — this part cannot hold this rig — and
        // the emulated part is a constant, so the distinction has nowhere to go.
        Trouble::Layout
    })?;

    for iteration in 0..ITERATIONS {
        let mut dispatcher = Counting { dispatched: 0 };
        let stop = {
            let mut metered = Metered::new(&mut storage);
            rig.prepare(&mut metered, iteration, page)
                .map_err(|_| Trouble::Layout)?;
            let mut cutter = PlannedCut::at(rig.cut_at(iteration), EFFECTS);
            rig.iterate(iteration, &mut metered, &mut dispatcher, &mut cutter, page)
                .map_err(|_| Trouble::Iterate)?
        };
        census.iterations = census.iterations.saturating_add(1);

        judge(&rig, iteration, &mut storage, page, census)?;

        if let Stop::Cut { .. } = stop {
            census.cuts = census.cuts.saturating_add(1);
            let resumed = {
                let mut metered = Metered::new(&mut storage);
                rig.resume(iteration, &mut metered, &mut dispatcher, page)
                    .map_err(|_| Trouble::Resume)?
            };
            match resumed {
                Resumed::Completed { redelivered, .. } => {
                    census.resumes = census.resumes.saturating_add(1);
                    if redelivered.is_some() {
                        census.redeliveries = census.redeliveries.saturating_add(1);
                    }
                    judge(&rig, iteration, &mut storage, page, census)?;
                }
                // ADR 0018's anti-bricking rule: a torn or unsealed tail has no append point,
                // and the continuation is §10's `continue_as_new`, which this rig does not
                // perform. Counted rather than treated as a failure — and counted separately,
                // so a boot in which *every* cut landed there could not be mistaken for a
                // boot that resumed.
                Resumed::Unextendable { .. } => {
                    census.unextendable = census.unextendable.saturating_add(1);
                }
            }
        }
        census.dispatched = census.dispatched.saturating_add(dispatcher.dispatched);
    }
    Ok(())
}

/// Verifies the part and requires the verdict to be a pass.
fn judge(
    rig: &Rig,
    iteration: u32,
    storage: &mut NorFlashStorage<&mut Nor>,
    page: &mut [u8],
    census: &mut Census,
) -> Result<(), Trouble> {
    let verdict = rig
        .verify(iteration, storage, page)
        .map_err(|_| Trouble::Verify)?;
    if verdict.outcome() == Outcome::Passed {
        census.verdicts_passed = census.verdicts_passed.saturating_add(1);
        Ok(())
    } else {
        Err(Trouble::Breach(verdict.outcome()))
    }
}
