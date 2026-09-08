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
//! library for `thumbv6m-none-eabi`, so the façade, the futures the compiler generates for
//! this workflow, and the bridge under them are all built for the part.

use core::convert::Infallible;
use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};

use waymaker_core::{ActivityKind, Outcome};
use waymaker_embassy::ctx::{Ctx, Failure};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ota<D> {
    dispatcher: D,
    out: [u8; BOUNDS.effect_result_bytes as usize],
}

impl<D> Ota<D> {
    /// A run that reaches the world through `dispatcher`.
    pub const fn new(dispatcher: D) -> Self {
        Self {
            dispatcher,
            out: [0; BOUNDS.effect_result_bytes as usize],
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
            version: WORKFLOW_VERSION,
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
            match (polled, ctx.conclusion()) {
                // The run did not reach its end in this boot.
                (Poll::Pending, _) => None,
                (Poll::Ready(_), Some(Outcome::Completed(bytes))) => {
                    Some(Ended::Completed(bytes.len()))
                }
                (Poll::Ready(_), Some(Outcome::Failed(bytes))) => Some(Ended::Failed(bytes.len())),
                // The workflow returned without recording an ending. Its own `Result` is
                // then what the run ended with.
                (Poll::Ready(Ok(())), None) => Some(Ended::Completed(0)),
                (Poll::Ready(Err(_)), None) => Some(Ended::Failed(0)),
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
