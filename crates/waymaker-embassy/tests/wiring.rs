//! The dispatch wiring: a table of numeric kinds, and names that are only ever metadata.
//!
//! Issue [#36](https://github.com/madmax983/waymaker/issues/36). What is here is that a
//! table selects a row by its number, that a name reaches a log and nothing else, and that
//! a kind no row declares is a clean error rather than a panic.
//!
//! The end-to-end run over real media is `crates/waymaker-drive/tests/dispatch.rs`.

use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};
use std::sync::Arc;
use std::task::Wake;

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, EffectId, EffectSeq, Outcome, RunId};
use waymaker_embassy::dispatch::Produced;
use waymaker_embassy::wiring::{Activity, Table, Unhandled};
use waymaker_embassy::{ActivityDispatcher, Answer, Ctx, Halted, Handoff, Journal};

const RUN: RunId = RunId(4);
const DOWNLOAD: ActivityKind = ActivityKind(11);
const VERIFY: ActivityKind = ActivityKind(12);
const ABSENT: ActivityKind = ActivityKind(99);

/// Why a row could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offline;

/// What the rows were asked, and how they answer.
#[derive(Debug, Default)]
struct World {
    /// One entry per row that ran, in order.
    ran: Vec<(&'static str, EffectId)>,
    /// How many polls answer `Pending` before an answer.
    stalls: usize,
    /// What a `DOWNLOAD` answers with.
    download: Vec<u8>,
    /// Whether a `DOWNLOAD` fails.
    fails: bool,
}

fn download(
    world: &mut World,
    _task: &mut Task<'_>,
    id: EffectId,
    _input: &[u8],
    out: &mut [u8],
) -> Poll<Result<Produced, Offline>> {
    if world.stalls > 0 {
        world.stalls -= 1;
        return Poll::Pending;
    }
    world.ran.push(("download", id));
    if world.fails {
        return Poll::Ready(Err(Offline));
    }
    let taken = world.download.len().min(out.len());
    let (Some(from), Some(into)) = (world.download.get(..taken), out.get_mut(..taken)) else {
        return Poll::Ready(Err(Offline));
    };
    into.copy_from_slice(from);
    Poll::Ready(Ok(Produced::Completed(world.download.len())))
}

fn verify(
    world: &mut World,
    _task: &mut Task<'_>,
    id: EffectId,
    _input: &[u8],
    _out: &mut [u8],
) -> Poll<Result<Produced, Offline>> {
    world.ran.push(("verify", id));
    Poll::Ready(Ok(Produced::Failed(0)))
}

/// A second row for `DOWNLOAD`, to say which of two the table runs.
fn shadow(
    world: &mut World,
    _task: &mut Task<'_>,
    id: EffectId,
    _input: &[u8],
    _out: &mut [u8],
) -> Poll<Result<Produced, Offline>> {
    world.ran.push(("shadow", id));
    Poll::Ready(Ok(Produced::Completed(0)))
}

const ACTIVITIES: &[Activity<World, Offline>] = &[
    Activity::new(DOWNLOAD, "download", download),
    Activity::new(VERIFY, "verify", verify),
];

/// Two rows for one number, so that "the first one wins" is measured rather than assumed.
const SHADOWED: &[Activity<World, Offline>] = &[
    Activity::new(DOWNLOAD, "download", download),
    Activity::new(DOWNLOAD, "shadow", shadow),
];

const fn table(world: World) -> Table<'static, World, Offline> {
    Table::over(world, ACTIVITIES)
}

const fn effect(seq: u32) -> EffectId {
    EffectId {
        run: RUN,
        seq: EffectSeq(seq),
    }
}

/// Dispatches `kind` once, the way the façade does.
fn dispatch_once(
    table: &mut Table<'_, World, Offline>,
    kind: ActivityKind,
    out: &mut [u8],
) -> Poll<Result<Produced, Unhandled<Offline>>> {
    let mut task = Task::from_waker(Waker::noop());
    table.poll_dispatch(&mut task, effect(0), kind, b"in", out)
}

#[test]
fn an_activity_is_selected_by_its_number() {
    let mut table = table(World {
        download: b"slot".to_vec(),
        ..World::default()
    });
    let mut out = [0_u8; 8];

    let answered = dispatch_once(&mut table, DOWNLOAD, &mut out);

    assert_eq!(answered, Poll::Ready(Ok(Produced::Completed(4))));
    assert_eq!(out.get(..4), Some(b"slot".as_slice()));
    assert_eq!(
        table.world().ran,
        vec![("download", effect(0))],
        "the row whose number the workflow asked for is the row that ran"
    );
}

#[test]
fn a_second_number_reaches_a_second_row() {
    let mut table = table(World::default());
    let mut out = [0_u8; 8];

    let answered = dispatch_once(&mut table, VERIFY, &mut out);

    assert_eq!(answered, Poll::Ready(Ok(Produced::Failed(0))));
    assert_eq!(table.world().ran, vec![("verify", effect(0))]);
}

#[test]
fn a_kind_no_row_declares_is_an_error_rather_than_a_panic() {
    let mut table = table(World::default());
    let mut out = [0_u8; 8];

    let answered = dispatch_once(&mut table, ABSENT, &mut out);

    assert_eq!(
        answered,
        Poll::Ready(Err(Unhandled::NoSuchActivity(ABSENT))),
        "a workflow this firmware cannot service is a named refusal, not a panic"
    );
    assert!(table.world().ran.is_empty(), "no row ran");
}

#[test]
fn a_rows_own_failure_travels_as_the_worlds_error() {
    let mut table = table(World {
        fails: true,
        ..World::default()
    });
    let mut out = [0_u8; 8];

    let answered = dispatch_once(&mut table, DOWNLOAD, &mut out);

    assert_eq!(answered, Poll::Ready(Err(Unhandled::Activity(Offline))));
}

#[test]
fn the_first_row_declaring_a_number_is_the_one_that_runs() {
    let mut table = Table::over(World::default(), SHADOWED);
    let mut out = [0_u8; 8];

    let answered = dispatch_once(&mut table, DOWNLOAD, &mut out);

    assert_eq!(answered, Poll::Ready(Ok(Produced::Completed(0))));
    assert_eq!(
        table.world().ran,
        vec![("download", effect(0))],
        "the first row wins, so a table is read top to bottom"
    );
}

#[test]
fn a_name_is_metadata_a_log_reads_and_never_a_key() {
    let table = table(World::default());

    assert_eq!(table.name_of(DOWNLOAD), Some("download"));
    assert_eq!(table.name_of(VERIFY), Some("verify"));
    assert_eq!(
        table.name_of(ABSENT),
        None,
        "a kind no row declares has no name to log"
    );
    assert_eq!(ACTIVITIES.first().map(Activity::kind), Some(DOWNLOAD));
    assert_eq!(ACTIVITIES.first().map(Activity::name), Some("download"));
}

#[test]
fn the_identity_the_schedule_record_committed_reaches_the_row() {
    let mut table = table(World::default());
    let mut out = [0_u8; 8];
    let mut task = Task::from_waker(Waker::noop());

    let _answered = table.poll_dispatch(&mut task, effect(7), VERIFY, b"in", &mut out);

    assert_eq!(
        table.world().ran,
        vec![("verify", effect(7))],
        "a downstream system deduplicates on this pair, so the row must see it"
    );
}

/// A waker that counts, so that "the table plumbs the executor's waker" is measured.
#[derive(Debug, Default)]
struct Counter {
    woken: std::sync::atomic::AtomicUsize,
}

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.woken
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn waking(
    world: &mut World,
    task: &mut Task<'_>,
    _id: EffectId,
    _input: &[u8],
    _out: &mut [u8],
) -> Poll<Result<Produced, Offline>> {
    world.ran.push(("waking", effect(0)));
    task.waker().wake_by_ref();
    Poll::Pending
}

const WAKING: &[Activity<World, Offline>] = &[Activity::new(DOWNLOAD, "waking", waking)];

#[test]
fn a_row_that_is_not_ready_reaches_the_executors_own_waker() {
    let counter = Arc::new(Counter::default());
    let waker = Waker::from(Arc::clone(&counter));
    let mut task = Task::from_waker(&waker);
    let mut table = Table::over(World::default(), WAKING);
    let mut out = [0_u8; 8];

    let answered = table.poll_dispatch(&mut task, effect(0), DOWNLOAD, b"in", &mut out);

    assert_eq!(answered, Poll::Pending);
    assert_eq!(
        counter.woken.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the table adds no waker of its own; it hands the row the executor's"
    );
}

#[test]
fn the_world_is_reachable_after_the_boot_that_used_it() {
    let mut table = table(World::default());
    let mut out = [0_u8; 8];

    let _answered = dispatch_once(&mut table, VERIFY, &mut out);
    table.world_mut().ran.clear();

    assert!(table.world().ran.is_empty());
}

/// A journal that hands out one dispatch and records what it was resolved with.
struct Ledger {
    handed: bool,
    resolved: Option<Vec<u8>>,
    failed: bool,
    kept: Vec<u8>,
}

impl Journal for Ledger {
    fn schedule(&mut self, _kind: ActivityKind, _input: &[u8]) -> Result<Handoff<'_>, Halted> {
        if self.handed {
            return Err(Halted);
        }
        self.handed = true;
        Ok(Handoff::Dispatch {
            id: effect(0),
            result_bytes: 8,
        })
    }

    fn resolve(&mut self, answer: Answer<'_>) -> Result<Outcome<'_>, Halted> {
        let (bytes, completed): (&[u8], bool) = match answer {
            Answer::Completed(bytes) => (bytes, true),
            Answer::Failed(bytes) => (bytes, false),
            Answer::Exhausted => (&[], false),
        };
        self.kept = bytes.to_vec();
        self.resolved = Some(self.kept.clone());
        self.failed = !completed;
        Ok(if completed {
            Outcome::Completed(&self.kept)
        } else {
            Outcome::Failed(&self.kept)
        })
    }

    fn wait(&mut self, _spec: TimerSpec) -> Result<(), Halted> {
        Err(Halted)
    }

    fn continue_as_new(&mut self, _input: &[u8]) -> Halted {
        Halted
    }
}

#[test]
fn a_table_reaches_the_world_only_after_the_journal_says_the_intent_is_durable() {
    let mut ledger = Ledger {
        handed: false,
        resolved: None,
        failed: false,
        kept: Vec::new(),
    };
    let mut table = table(World {
        download: b"slot".to_vec(),
        ..World::default()
    });
    let mut out = [0_u8; 8];
    let mut ctx = Ctx::new(&mut ledger, &mut table, &mut out);

    let mut task = Task::from_waker(Waker::noop());
    let answered = pin!(ctx.activity::<()>(DOWNLOAD, b"url")).poll(&mut task);

    assert!(matches!(answered, Poll::Ready(Ok(()))));
    assert_eq!(ledger.resolved, Some(b"slot".to_vec()));
    assert!(!ledger.failed);
}

#[test]
fn a_table_that_cannot_service_a_kind_records_a_failure_with_no_payload() {
    let mut ledger = Ledger {
        handed: false,
        resolved: None,
        failed: false,
        kept: Vec::new(),
    };
    let mut table = table(World::default());
    let mut out = [0_u8; 8];
    let mut ctx = Ctx::new(&mut ledger, &mut table, &mut out);

    let mut task = Task::from_waker(Waker::noop());
    let _answered = pin!(ctx.activity::<()>(ABSENT, b"url")).poll(&mut task);

    assert_eq!(
        ledger.resolved,
        Some(Vec::new()),
        "the run makes progress: §08 has no edge from an unresolved effect to a terminal \
         record, so a refusal here would strand it"
    );
    assert!(ledger.failed);
}
