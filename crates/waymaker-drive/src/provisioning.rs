//! Design document §06's provisioning example, run against the façade.
//!
//! Issue [#38](https://github.com/madmax983/waymaker/issues/38)'s second example.
//! [`ota_update`](crate::ota::ota_update) uses three activities and ends cleanly. This
//! example uses boundaries that one does not: a timer, a retried activity, and a terminal
//! failure with a real payload.
//!
//! # The input gap this closes
//!
//! `ota_update` runs on `ota::URL`, a module constant. No boot reads it back, so `Ota`'s
//! `Workflow::identity` and the input bytes agree by accident, not by design.
//! [`Driver::begin`](crate::Driver) checks the recorded `RunStarted` against
//! `Workflow::identity` on every boot. [`Provisioning`] stores its run's input as a field,
//! and [`provision`] reads that field. A caller that changes the input between boots gets
//! `DriveError::NotThisWorkflow`, not a silent rewind.
//!
//! # Here rather than in `tests/`
//!
//! Same reason as [`ota`](crate::ota). `Provisioning` is generic; [`poll_provisioning`] is
//! concrete. The firmware build monomorphises this workflow's future, [`Bridge`], and two
//! façade futures this example adds: `TimerFuture` and `TerminalFuture`'s failure arm.
//!
//! [`Bridge`]: crate::Bridge

use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};

use waymaker_core::EffectId;
use waymaker_core::timer::TimerSpec;
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, Outcome};
use waymaker_embassy::ctx::{Conclusion, Ctx, Failure};
use waymaker_embassy::dispatch::Produced;
use waymaker_embassy::{ActivityDispatcher, Decode, Journal};
use waymaker_flash::capacity::Bounds;

use crate::boundary::{Boundary, Suspended};
use crate::facade::Bridge;
use crate::workflow::{Identity, Workflow};

/// Register this device with the fleet.
pub const REGISTER: ActivityKind = ActivityKind(14);
/// The workflow's kind, as its `RunStarted` record records it.
pub const WORKFLOW_KIND: u16 = 10;
/// The workflow's version.
pub const WORKFLOW_VERSION: u16 = 1;
/// How wide a device id this example accepts.
pub const DEVICE_ID_BYTES: usize = 7;
/// A device id a caller may use to build a [`Provisioning`] run.
pub const DEVICE_ID: [u8; DEVICE_ID_BYTES] = *b"dev-001";
/// The provisioning window's opening instant.
///
/// `AtPersistentTime`, not `AfterBoot`: a fleet rollout window means the same wall-clock
/// instant across a power cut, which is the policy issue #34's board owes and this example
/// exercises on the host model.
pub const WINDOW: TimerSpec = TimerSpec::AtPersistentTime {
    instant: 1_750_000_000,
};
/// How many registration attempts one run makes before it gives up.
pub const MAX_ATTEMPTS: u32 = 3;
/// How wide a registration token is.
pub const TOKEN_BYTES: usize = 4;
/// The terminal payload of a run that exhausted its attempts.
pub const EXHAUSTED: &[u8] = b"registration failed";

/// What a provisioning run's records may be worth, for §10's reserve.
///
/// `DEVICE_ID`'s length, [`TOKEN_BYTES`], and [`EXHAUSTED`]'s length — checked against the
/// constants below rather than cast from them, so a widened `Bounds` field never silently
/// truncates one.
pub const BOUNDS: Bounds = Bounds {
    run_input_bytes: 7,
    effect_result_bytes: 4,
    terminal_bytes: 19,
};

const _: () = assert!(DEVICE_ID_BYTES == BOUNDS.run_input_bytes as usize);
const _: () = assert!(TOKEN_BYTES == BOUNDS.effect_result_bytes as usize);
const _: () = assert!(EXHAUSTED.len() == BOUNDS.terminal_bytes as usize);

/// A registration token, as the workflow keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    bytes: [u8; TOKEN_BYTES],
}

impl Token {
    /// The token, as the run's terminal payload.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The recorded answer is not a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NotAToken;

impl Decode for Token {
    type Error = NotAToken;

    fn decode(bytes: &[u8]) -> Result<Self, NotAToken> {
        let bytes: [u8; TOKEN_BYTES] = bytes.try_into().map_err(|_ignored| NotAToken)?;
        Ok(Self { bytes })
    }
}

/// What this run registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProvisionInput<'a> {
    device_id: &'a [u8],
}

impl<'a> ProvisionInput<'a> {
    /// A run registering `device_id`.
    #[must_use]
    pub const fn at(device_id: &'a [u8]) -> Self {
        Self { device_id }
    }

    /// The bytes the registration activity is asked for.
    #[must_use]
    pub const fn device_id_bytes(&self) -> &'a [u8] {
        self.device_id
    }
}

/// Why a provisioning run did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProvisionError {
    /// The recorded answer is not a token.
    ///
    /// The bytes replay identically on every boot, so a retry would not help.
    NotAToken,
}

/// Wait for the window, then register — retrying on failure, failing on exhaustion.
///
/// Design document §06's second example. It uses boundaries
/// [`ota_update`](crate::ota::ota_update) does not: a timer, a retried activity, and a
/// terminal failure that carries a payload.
///
/// # Errors
///
/// [`ProvisionError::NotAToken`] when the recorded answer is not a token.
pub async fn provision<D, J>(
    ctx: &mut Ctx<'_, D, J>,
    input: ProvisionInput<'_>,
) -> Result<(), ProvisionError>
where
    D: ActivityDispatcher,
    J: Journal,
{
    ctx.timer(WINDOW).await;

    let mut attempt: u32 = 0;
    let token = loop {
        match ctx
            .activity::<Token>(REGISTER, input.device_id_bytes())
            .await
        {
            Ok(token) => break token,
            Err(Failure::Decode(NotAToken)) => return Err(ProvisionError::NotAToken),
            Err(Failure::Activity { .. }) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return ctx.fail(EXHAUSTED).await;
                }
            }
        }
    };

    ctx.complete(token.as_bytes()).await
}

/// [`provision`] as the synchronous driver runs it.
///
/// One boot is one `run`: it creates the future, polls it once, and drops it. See
/// [`Ota`](crate::ota::Ota) for why one poll and why the future is disposable — the same
/// reasoning applies here unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provisioning<D> {
    dispatcher: D,
    input: [u8; DEVICE_ID_BYTES],
    out: [u8; OUT_BYTES],
}

/// How wide the context buffer must be: the wider of the run's two bounds.
const OUT_BYTES: usize = if BOUNDS.effect_result_bytes > BOUNDS.terminal_bytes {
    BOUNDS.effect_result_bytes as usize
} else {
    BOUNDS.terminal_bytes as usize
};

impl<D> Provisioning<D> {
    /// A run over `dispatcher`, registering `device_id`.
    ///
    /// `device_id` is this run's own input. `Workflow::identity` reports it, and
    /// `Driver::begin` compares it against the recorded `RunStarted` on every boot — so a
    /// caller must pass the same bytes on every boot of one run.
    pub const fn new(dispatcher: D, device_id: [u8; DEVICE_ID_BYTES]) -> Self {
        Self {
            dispatcher,
            input: device_id,
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

impl<D: ActivityDispatcher> Workflow for Provisioning<D> {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: &self.input,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let ended = {
            let mut bridge = Bridge::over(boundary);
            let mut ctx = Ctx::new(&mut bridge, &mut self.dispatcher, &mut self.out);
            let polled = {
                let mut future = pin!(provision(&mut ctx, ProvisionInput::at(&self.input)));
                future.as_mut().poll(&mut Task::from_waker(Waker::noop()))
            };
            // The recorded ending outranks the poll. See `Ota::run`: `TerminalFuture` never
            // resolves, so nothing later in this poll can have overwritten `self.out`.
            match (ctx.conclusion(), polled) {
                (Some(Conclusion::Ended(Outcome::Completed(bytes))), _) => {
                    Some(Ended::Completed(bytes.len()))
                }
                (Some(Conclusion::Ended(Outcome::Failed(bytes))), _) => {
                    Some(Ended::Failed(bytes.len()))
                }
                // `OUT_BYTES` is the wider bound, so `Refused` cannot happen here. `Pending`
                // with no ending means the run is still waiting on the timer or an attempt.
                (Some(Conclusion::Refused), _) | (None, Poll::Pending) => None,
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

/// What [`Registrar`] answers a [`REGISTER`] with.
pub const TOKEN: [u8; TOKEN_BYTES] = *b"tok0";

/// A world that answers from a constant.
///
/// Exists so [`Provisioning`] has a concrete `D`. Without one, this crate's firmware build
/// type-checks [`provision`] and compiles none of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Registrar;

/// A [`Registrar`] never fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unreachable {}

impl ActivityDispatcher for Registrar {
    type Error = Unreachable;

    fn poll_dispatch(
        &mut self,
        _task: &mut Task<'_>,
        _id: EffectId,
        _kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Unreachable>> {
        let taken = TOKEN.len().min(out.len());
        let (Some(from), Some(into)) = (TOKEN.get(..taken), out.get_mut(..taken)) else {
            return Poll::Ready(Ok(Produced::Completed(TOKEN.len())));
        };
        into.copy_from_slice(from);
        Poll::Ready(Ok(Produced::Completed(TOKEN.len())))
    }
}

/// The context this firmware links, with every type fixed.
pub type ProvisioningContext<'a> = Ctx<'a, Registrar, Bridge<'a>>;

waymaker_core::assert_context_size!(ProvisioningContext<'static>);

/// The size of the future `make` returns, without building one.
///
/// See [`ota`](crate::ota)'s copy of this helper for why: `make` is never called, and a
/// function pointer is what lets inference give `F` from the signature.
const fn returned_future_bytes<F: Future>(
    _make: fn(&'static mut ProvisioningContext<'static>, ProvisionInput<'static>) -> F,
) -> usize {
    size_of::<F>()
}

/// [`provision`]'s generated state machine, in bytes.
///
/// Reported by `cargo xtask size` beside [`ota::WORKFLOW_FUTURES`](crate::ota::WORKFLOW_FUTURES),
/// summed into nothing. See that module for why it is measured and not budgeted.
pub const WORKFLOW_FUTURES: [(&str, usize); 1] = [("provision", PROVISION_FUTURE_BYTES)];

/// [`provision`]'s generated state machine, in bytes.
const PROVISION_FUTURE_BYTES: usize = returned_future_bytes(provision);

const _: () = assert!(
    PROVISION_FUTURE_BYTES > 0,
    "a workflow that holds a context across a boundary has state; a zero-sized future means \
     something other than the future was measured",
);

/// One boot of a provisioning run, with every type fixed.
///
/// Names no type parameter, so the firmware build monomorphises `Provisioning<Registrar>`'s
/// future, the façade futures this example uses, and [`Bridge`] as code on the part.
///
/// # Errors
///
/// [`Suspended`] whenever the run must stop in this boot.
pub fn poll_provisioning<'a>(
    workflow: &'a mut Provisioning<Registrar>,
    boundary: &mut dyn Boundary,
) -> Result<Outcome<'a>, Suspended> {
    workflow.run(boundary)
}
