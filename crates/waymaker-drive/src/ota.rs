//! Design document §06's OTA example, run against the façade.
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35)'s first "done when". The
//! workflow below is §06's, as an `async fn`: three activities and a completion, with the
//! image crossing every boundary as a handle.
//!
//! # Why a handle
//!
//! A Rust future holds every local that survives an `.await`. A firmware image held across
//! three effect boundaries would be in the workflow future, which is RAM for the life of
//! the run. [`ImageSlot`] is eight bytes and names the image instead.
//!
//! # Here rather than in `tests/`
//!
//! For [`demo`](crate::demo)'s reason. The `drive-firmware` stage builds this crate's
//! library for `thumbv6m-none-eabi`.
//!
//! [`ota_update`] and [`Ota`] are generic, and a generic body no caller names is
//! type-checked rather than compiled. [`Downloader`] and [`poll_ota`] are what make the
//! claim true: they are concrete, so the firmware build monomorphises this workflow's
//! future, [`Bridge`], and the two façade futures §06's example uses —
//! `ActivityFuture` and `TerminalFuture`. `nm` on the rlib finds them.
//!
//! It uses neither `TimerFuture` nor `ContinueFuture`, so neither is in this rlib. The size
//! probe drives all four, which is what the `facade` row of `cargo xtask size` measures.
//!
//! [`Bridge`]: crate::Bridge

use core::convert::Infallible;
use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};

use waymaker_core::EffectId;
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, Outcome};
use waymaker_embassy::ctx::{Conclusion, Ctx, Failure};
use waymaker_embassy::dispatch::Produced;
use waymaker_embassy::{ActivityDispatcher, Decode, Journal};
use waymaker_flash::capacity::Bounds;

use crate::boundary::{Boundary, Suspended};
use crate::facade::Bridge;
use crate::workflow::{Identity, Workflow};

/// Fetch the image and leave it somewhere the device can reach.
pub const DOWNLOAD: ActivityKind = ActivityKind(11);
/// Check the image's signature.
pub const VERIFY_SIGNATURE: ActivityKind = ActivityKind(12);
/// Write the image to the inactive slot.
pub const FLASH_IMAGE: ActivityKind = ActivityKind(13);
/// The workflow's kind, as its `RunStarted` record records it.
pub const WORKFLOW_KIND: u16 = 9;
/// The workflow's version.
pub const WORKFLOW_VERSION: u16 = 1;
/// What this run is asked to fetch.
pub const URL: &[u8] = b"fw://a";

/// What an OTA run's records may be worth, for §10's reserve.
pub const BOUNDS: Bounds = Bounds {
    run_input_bytes: 6,
    effect_result_bytes: 8,
    terminal_bytes: 8,
};

/// A downloaded image, as the workflow keeps it: a handle, never the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageSlot {
    handle: [u8; 8],
}

impl ImageSlot {
    /// The handle, as the next activity's input.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.handle
    }
}

/// The recorded answer is not a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NotAnImageSlot;

impl Decode for ImageSlot {
    type Error = NotAnImageSlot;

    fn decode(bytes: &[u8]) -> Result<Self, NotAnImageSlot> {
        let handle: [u8; 8] = bytes.try_into().map_err(|_ignored| NotAnImageSlot)?;
        Ok(Self { handle })
    }
}

/// What the run was started with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OtaInput<'a> {
    url: &'a [u8],
}

impl<'a> OtaInput<'a> {
    /// A run over `url`.
    #[must_use]
    pub const fn at(url: &'a [u8]) -> Self {
        Self { url }
    }

    /// The bytes the first activity is asked for.
    #[must_use]
    pub const fn url_bytes(&self) -> &'a [u8] {
        self.url
    }
}

/// Why an OTA run did not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OtaError {
    /// An activity failed.
    Activity,
    /// The download's answer is not a handle.
    NotAnImageSlot,
}

impl From<Failure<NotAnImageSlot>> for OtaError {
    fn from(failure: Failure<NotAnImageSlot>) -> Self {
        match failure {
            Failure::Activity { .. } => Self::Activity,
            Failure::Decode(NotAnImageSlot) => Self::NotAnImageSlot,
        }
    }
}

impl From<Failure<Infallible>> for OtaError {
    fn from(failure: Failure<Infallible>) -> Self {
        match failure {
            Failure::Activity { .. } => Self::Activity,
            // `Infallible` has no value, so this arm names one that cannot be built.
            Failure::Decode(never) => match never {},
        }
    }
}

/// Download an image, verify it, flash it.
///
/// Design document §06's example. The `?` after each `.await` is where a failed effect
/// stops the run; the `.await` itself is where a reset does.
///
/// # Errors
///
/// [`OtaError`] when an activity failed or its answer is not a handle.
pub async fn ota_update<D, J>(ctx: &mut Ctx<'_, D, J>, input: OtaInput<'_>) -> Result<(), OtaError>
where
    D: ActivityDispatcher,
    J: Journal,
{
    // A handle, not the image bytes.
    let image: ImageSlot = ctx.activity(DOWNLOAD, input.url_bytes()).await?;

    let () = ctx.activity(VERIFY_SIGNATURE, image.as_bytes()).await?;
    let () = ctx.activity(FLASH_IMAGE, image.as_bytes()).await?;

    ctx.complete(&[]).await
}

/// [`ota_update`] as the synchronous driver runs it.
///
/// One boot is one `run`, and one `run` creates the future, polls it once, and drops it.
/// That is §06's "the future is disposable": nothing it held survives the call, so a reset
/// takes nothing the next boot needs.
///
/// # Why one poll
///
/// There is no executor here. A dispatcher that answers now lets the future run on to the
/// next boundary within the same poll, so one poll carries the run as far as it goes. A
/// dispatcher that answers [`Poll::Pending`] ends the boot, exactly as
/// [`Performed::Pending`](crate::Performed) does on the synchronous path.
///
/// A run that *finished* is `Poll::Pending` too, because `ctx.complete(..)` never resolves.
/// What the run ended with is [`Ctx::conclusion`](waymaker_embassy::Ctx::conclusion), and
/// this reads it whatever the poll said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ota<D> {
    dispatcher: D,
    out: [u8; OUT_BYTES],
}

/// How wide the context buffer must be.
///
/// The wider of the run's two bounds. An activity answer is bounded by
/// `effect_result_bytes` and a terminal payload by `terminal_bytes`, and both are written
/// through this one buffer — so sizing it from either alone refuses a legal run.
const OUT_BYTES: usize = if BOUNDS.effect_result_bytes > BOUNDS.terminal_bytes {
    BOUNDS.effect_result_bytes as usize
} else {
    BOUNDS.terminal_bytes as usize
};

impl<D> Ota<D> {
    /// A run that reaches the world through `dispatcher`.
    pub const fn new(dispatcher: D) -> Self {
        Self {
            dispatcher,
            out: [0; OUT_BYTES],
        }
    }

    /// The dispatcher, for a caller that wants to look at what it did.
    pub const fn dispatcher(&self) -> &D {
        &self.dispatcher
    }
}

/// How the workflow's own boot ended, without the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Completed(usize),
    Failed(usize),
}

impl<D: ActivityDispatcher> Workflow for Ota<D> {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let ended = {
            let mut bridge = Bridge::over(boundary);
            let mut ctx = Ctx::new(&mut bridge, &mut self.dispatcher, &mut self.out);
            let polled = {
                let mut future = pin!(ota_update(&mut ctx, OtaInput::at(URL)));
                future.as_mut().poll(&mut Task::from_waker(Waker::noop()))
            };
            // The recorded ending outranks what the poll said, because `TerminalFuture`
            // never resolves: a workflow that ended is a future that is `Pending` for ever,
            // which is what stops a later boundary overwriting the buffer the ending points
            // into.
            match (ctx.conclusion(), polled) {
                (Some(Conclusion::Ended(Outcome::Completed(bytes))), _) => {
                    Some(Ended::Completed(bytes.len()))
                }
                (Some(Conclusion::Ended(Outcome::Failed(bytes))), _) => {
                    Some(Ended::Failed(bytes.len()))
                }
                // Two runs that did not conclude in this boot. `Refused` is a workflow
                // that asked to end with a payload wider than the buffer: it must not be
                // recorded as ending with something *else*, so no terminal record is
                // written, and it is unreachable here because `OUT_BYTES` is the wider of
                // the run's two bounds. `Pending` with no ending is a workflow that
                // suspended.
                (Some(Conclusion::Refused), _) | (None, Poll::Pending) => None,
                // The workflow returned without recording an ending, so its own `Result` is
                // what the run ended with.
                (None, Poll::Ready(Ok(()))) => Some(Ended::Completed(0)),
                (None, Poll::Ready(Err(_))) => Some(Ended::Failed(0)),
            }
        };
        let Some(ended) = ended else {
            return Err(Suspended::NEW);
        };
        Ok(match ended {
            Ended::Completed(len) => Outcome::Completed(self.out.get(..len).unwrap_or_default()),
            Ended::Failed(len) => Outcome::Failed(self.out.get(..len).unwrap_or_default()),
        })
    }
}

/// What [`Downloader`] answers a [`DOWNLOAD`] with.
pub const HANDLE: &[u8] = b"slot0001";

/// A world that answers from constants.
///
/// It exists so that [`Ota`] has a concrete `D`. Without one, this crate's firmware build
/// type-checks [`ota_update`] and compiles none of it, and the futures the compiler
/// generates for the workflow never reach the part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Downloader;

/// A [`Downloader`] never fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Offline {}

impl ActivityDispatcher for Downloader {
    type Error = Offline;

    fn poll_dispatch(
        &mut self,
        _task: &mut Task<'_>,
        _id: EffectId,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Offline>> {
        let answer: &[u8] = if kind == DOWNLOAD { HANDLE } else { b"ok" };
        let taken = answer.len().min(out.len());
        let (Some(from), Some(into)) = (answer.get(..taken), out.get_mut(..taken)) else {
            return Poll::Ready(Ok(Produced::Completed(answer.len())));
        };
        into.copy_from_slice(from);
        // The answer's whole length, which is what the trait asks for even when it is wider
        // than `out`.
        Poll::Ready(Ok(Produced::Completed(answer.len())))
    }
}

/// The context this firmware links, with every type fixed.
///
/// [`Ctx`] borrows its journal, its dispatcher and its buffer, so its size is the same for
/// every `D` and `J`. Naming the pair the firmware really links is what makes
/// [`CONTEXT_BYTES`] a reading of this image rather than of a fixture.
pub type OtaContext<'a> = Ctx<'a, Downloader, Bridge<'a>>;

/// Design document §04's context term, in bytes.
///
/// §04 states runtime RAM as "cursor, context, record header, and storage scratch". The
/// cursor and the record header are `waymaker_core::budget`'s registry and the scratch page
/// is the caller's; this is the fourth term, and until now nothing measured it.
///
/// The assertion below is the gate on the target the budget is stated for — the
/// `drive-firmware` stage compiles this module for `thumbv6m-none-eabi`. `cargo xtask size`
/// reports the same constant measured on the host, where a pointer is wider, so the
/// reported figure is an upper bound on this one.
pub const CONTEXT_BYTES: usize = size_of::<OtaContext<'static>>();

waymaker_core::assert_context_size!(OtaContext<'static>);

// A ceiling alone lets a narrower type stand in for the context and pass every check: the
// macro above constrains the type, not the constant, and `xtask` gates whatever this holds.
// `Ctx` is a journal borrow, a dispatcher borrow, a slice borrow, a length and an ending, so
// five words is a floor no substitute of a scalar or a thinner reference clears.
const _: () = assert!(
    CONTEXT_BYTES >= 5 * size_of::<usize>(),
    "the context measures less than its own borrows; something narrower than `Ctx` was sized",
);

/// The size of the future `make` returns, without building one.
///
/// `make` is never called. An `async fn`'s return type cannot be written down, and building
/// a value of it would need a journal, a dispatcher and a buffer; the parameter is there so
/// that inference gives `F` from the signature. A function pointer rather than a closure
/// because a closure has a destructor, which a `const fn` may not drop.
const fn returned_future_bytes<F: Future>(
    _make: fn(&'static mut OtaContext<'static>, OtaInput<'static>) -> F,
) -> usize {
    size_of::<F>()
}

/// Every generated workflow future in this crate, with its size in bytes.
///
/// Reported by `cargo xtask size` in a section of its own and summed into nothing. Design
/// document §04 excludes the user workflow future from the runtime RAM budget, and issue
/// [#39](https://github.com/madmax983/waymaker/issues/39) asks that the report make it
/// visible rather than average it away: a small [`CONTEXT_BYTES`] does not pay for a large
/// state machine.
///
/// Sized for whichever target this crate was compiled for, so `xtask` reports host figures
/// and a firmware build holds the part's. Neither is gated — §04 sets no budget for user
/// memory, and a future that grew moves no line above this one.
pub const WORKFLOW_FUTURES: [(&str, usize); 1] = [("ota_update", OTA_FUTURE_BYTES)];

/// [`ota_update`]'s generated state machine, in bytes.
const OTA_FUTURE_BYTES: usize = returned_future_bytes(ota_update);

const _: () = assert!(
    OTA_FUTURE_BYTES > 0,
    "a workflow that holds a context across three boundaries has state; a zero-sized future \
     means something other than the future was measured",
);

/// One boot of the OTA run, with every type fixed.
///
/// The whole reason this function exists is that it names no type parameter. It is what the
/// firmware build monomorphises, so `Ota<Downloader>`'s future, the four façade futures and
/// [`Bridge`] are code on the part rather than code the part type-checked.
///
/// # Errors
///
/// [`Suspended`] whenever the run must stop in this boot.
pub fn poll_ota<'a>(
    workflow: &'a mut Ota<Downloader>,
    boundary: &mut dyn Boundary,
) -> Result<Outcome<'a>, Suspended> {
    workflow.run(boundary)
}
