//! Rules over the repository's documentation scaffolding.
//!
//! Issue #11 asks for four things: a `CLAUDE.md` carrying the invariants and the design
//! document's "must not own" table, an ADR record with a template, Mermaid diagrams for
//! the crate dependency flow, the durable effect protocol, and the two-bank swap, and
//! `missing_docs` warned on in every crate.
//!
//! Written as prose alone, each of those is true on the day it is written and untrue some
//! weeks later, silently. The repository already has an answer to that shape of problem:
//! the layering is a table in [`crate::policy`] and the pipeline is a table in
//! [`crate::pipeline`], and a rule fails a pull request when the file and the table stop
//! agreeing. This module does the same for the documentation.
//!
//! What it deliberately does *not* do is check prose. The anchors are the things that
//! have a machine-readable counterpart — a crate name, a `must_not_own` cell, a settled
//! decision's id, a protocol step, an ADR number — so that the words around them stay
//! free to be rewritten.

use std::collections::BTreeMap;

use crate::Violation;
use crate::policy::LAYERS;
use crate::source::{enables_lint, inner_attributes, silences_lint};

/// Where the contributor-facing invariants live, relative to the workspace root.
pub const CLAUDE_MD_PATH: &str = "CLAUDE.md";

/// Where the architecture diagrams live, relative to the workspace root.
pub const ARCHITECTURE_PATH: &str = "docs/architecture.md";

/// Where the decision record lives, relative to the workspace root.
pub const ADR_DIR: &str = "docs/adr";

/// The index file inside [`ADR_DIR`], which is not itself an ADR.
pub const ADR_INDEX: &str = "README.md";

/// The template every ADR is written from, numbered zero so that it sorts first and is
/// visibly not a decision anyone took.
pub const ADR_TEMPLATE: &str = "0000-template.md";

/// The first number a real decision may carry.
pub const ADR_NUMBER_START: u32 = 1;

/// The ADR that records design document §02.
///
/// Issue #11 asks for this to be ADR-0001. By the time the issue was worked, 0001 and 0002
/// were already accepted decisions with a cross-link between them and a merge history;
/// renumbering them to free 0001 would have rewritten a record whose whole purpose is to
/// be stable. The eight decisions are therefore recorded here, and [`ADR_INDEX`] states
/// the ordering so that "the oldest decisions have the highest number" is a documented
/// fact rather than a surprise.
pub const SETTLED_DECISIONS_ADR: &str = "0003-the-eight-settled-design-decisions.md";

/// The ADR that records the recovery specification of issue #20.
pub const RECOVERY_SPEC_ADR: &str =
    "0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md";

/// Where the specification's own clause table lives, relative to the workspace root.
///
/// Read rather than trusted: [`SPEC_CLAUSES`] is what `CLAUDE.md` and the ADR are compared
/// against, and this file is what the proofs are compared against. Without reading both, a
/// clause could be deleted from the crate with the documentation still describing it, which
/// is one of the exact two ways a specification rots.
pub const SPEC_OBLIGATIONS_PATH: &str = "crates/waymaker-spec/src/obligation.rs";

/// One of design document §14's guarantees, as the gate knows it.
///
/// The counterpart of [`waymaker-spec`'s own table][spec]; `recovery-spec` fails a build in
/// which the two disagree, in either direction.
///
/// [spec]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-spec/src/obligation.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecClause {
    /// Stable identifier, cited by `CLAUDE.md`, by the ADR and by the crate.
    pub id: &'static str,
    /// The guarantee, quoted from §14 closely enough to be recognisable.
    pub headline: &'static str,
    /// The test target that discharges it, relative to `crates/waymaker-spec`.
    ///
    /// Named in `CLAUDE.md` too, so that a reader who wants to know what holds a guarantee
    /// up is one grep away from the file rather than one guess away.
    pub discharged_by: &'static str,
}

/// Design document §14's five guarantees, plus §02 decision 7's single authority.
///
/// Six rather than issue [#20](https://github.com/madmax983/waymaker/issues/20)'s five:
/// the issue lists single authority where §14 lists stable redelivery, and a specification
/// of the recovery guarantees that dropped either would be a specification of most of them.
pub const SPEC_CLAUSES: &[SpecClause] = &[
    SpecClause {
        id: "prefix-safety",
        headline: "recovery exposes only a legal prefix of committed records",
        discharged_by: "tests/spine.rs",
    },
    SpecClause {
        id: "acknowledged-durability",
        headline: "any record acknowledged after its barrier is recovered after reset",
        discharged_by: "tests/spine.rs",
    },
    SpecClause {
        id: "durable-intent",
        headline: "no Waymaker-dispatched effect lacks a recoverable schedule record",
        discharged_by: "tests/spine.rs",
    },
    SpecClause {
        id: "single-authority",
        headline: "exactly one bank is authoritative after any crash",
        discharged_by: "tests/spine.rs",
    },
    SpecClause {
        id: "stable-redelivery",
        headline: "retries and reboot redelivery reuse the original effect identity",
        discharged_by: "tests/redelivery.rs",
    },
    SpecClause {
        id: "bounded-decoding",
        headline: "malformed storage cannot cause out-of-bounds reads or allocation",
        discharged_by: "tests/bounded_decoding.rs",
    },
];

/// The id of the diagram that has to agree with [`LAYERS`].
pub const CRATE_DEPENDENCY_DIAGRAM: &str = "crate-dependency-flow";

/// Headings every ADR must carry, in any order.
///
/// The template is checked against the same list, so a template that stops asking for a
/// section is a violation rather than a slow drift in what an ADR contains.
pub const ADR_REQUIRED_HEADINGS: &[&str] = &["## Context", "## Decision", "## Consequences"];

/// Front-matter keys every ADR must carry.
pub const ADR_REQUIRED_FIELDS: &[&str] = &["- Status:", "- Date:"];

/// The statuses an ADR may be in.
///
/// A closed vocabulary, because "Status: mostly" is how a decision nobody took ends up
/// looking like one that was.
pub const ADR_STATUSES: &[&str] = &[
    "proposed",
    "accepted",
    "rejected",
    "deprecated",
    "superseded",
];

/// The lint that makes an undocumented public item cost something.
///
/// Matched by name at a lint level rather than as a whole attribute string, so
/// `#![warn(missing_docs, unreachable_pub)]` counts and `#![expect(missing_docs)]`,
/// `#![allow(warnings)]` and `#![cfg_attr(all(), allow(missing_docs))]` do not. See
/// `crate::source::enables_lint` and `crate::source::silences_lint`.
pub const MISSING_DOCS_LINT: &str = "missing_docs";

/// One of the decisions design document §02 settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettledDecision {
    /// Stable identifier, cited by `CLAUDE.md`, by the ADR that records it, and by any
    /// later ADR that revisits it.
    pub id: &'static str,
    /// The decision itself, quoted from §02 closely enough to be recognisable.
    pub headline: &'static str,
}

/// The eight decisions design document §02 settles.
///
/// This list is the spec. The ADR that records them is checked against it, so a decision
/// cannot be quietly dropped from the record, and a ninth decision cannot be added to §02
/// without a row here failing until it has an ADR of its own.
pub const SETTLED_DECISIONS: &[SettledDecision] = &[
    SettledDecision {
        id: "kernel-is-dependency-free",
        headline: "The kernel is `no_std`, `no_alloc`, and dependency-free",
    },
    SettledDecision {
        id: "replay-is-sequential",
        headline: "Replay is sequential",
    },
    SettledDecision {
        id: "durable-intent-before-effect",
        headline: "Physical effects happen only after durable intent",
    },
    SettledDecision {
        id: "numeric-kinds-and-borrowed-bytes",
        headline: "History records use numeric kinds and borrowed bytes",
    },
    SettledDecision {
        id: "async-syntax-is-an-adapter",
        headline: "Normal async syntax is an adapter",
    },
    SettledDecision {
        id: "no-snapshotted-futures",
        headline: "Arbitrary suspended futures are not snapshotted",
    },
    SettledDecision {
        id: "two-banks-for-atomic-replacement",
        headline: "Two flash banks provide atomic run replacement",
    },
    SettledDecision {
        id: "durable-timers-need-durable-time",
        headline: "Durable timers require durable time",
    },
];

/// The line an ADR carries to claim one of §16's deferred questions.
///
/// A marker rather than a heuristic on the prose. "This ADR is about the integrity check"
/// is a thing a reader concludes and a thing a rule cannot; a line that says which question
/// id the ADR closes is a thing both can read, and it is what makes the
/// `deferred-questions` rule able to fail in the direction that matters — an ADR that
/// answers a question the table still lists as open.
///
/// The id follows on the same line, optionally in backticks. Markers inside an HTML comment
/// do not count, for the reason every rule in this module strips them: a claim a reader
/// cannot see is not a record.
pub const DEFERRED_QUESTION_MARKER: &str = "Settles deferred question:";

/// Whether one of §16's questions has been answered, and by what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionStatus {
    /// Nobody has answered it yet.
    Open {
        /// The roadmap rung that will, from design document §16's rung table.
        owned_by: &'static str,
        /// What would settle it — the evidence, not the opinion.
        ///
        /// Written down so that "deferred" means "waiting for a specific thing" rather
        /// than "nobody got to it". A question with no exit criterion is a question that
        /// stays open by default.
        settled_when: &'static str,
    },
    /// Answered, and recorded in this ADR.
    Settled {
        /// The ADR file name, inside [`ADR_DIR`].
        adr: &'static str,
    },
}

/// One of the questions design document §16 leaves open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredQuestion {
    /// Stable identifier, cited by `CLAUDE.md` and by the ADR that settles it.
    pub id: &'static str,
    /// The question itself, quoted from §16 closely enough to be recognisable.
    pub headline: &'static str,
    /// Whether it has been answered.
    pub status: QuestionStatus,
}

impl DeferredQuestion {
    /// How `CLAUDE.md` must state where this question stands, on the question's own row.
    ///
    /// Rendered rather than described, for the reason `LAYERS::render_allowed` is: a
    /// contributor reading the file reads the string the gate reads, or the gate says so.
    /// Codex caught the first version of this rule searching the whole file for the id and
    /// the headline and nothing else, which let a row call a settled question open, name
    /// the wrong rung, or point at the wrong ADR.
    ///
    /// A settled question renders its ADR's file name, so the row a reader lands on is one
    /// click from the answer.
    #[must_use]
    pub fn render_status(&self) -> String {
        match self.status {
            QuestionStatus::Settled { adr } => format!("Settled by {adr}"),
            QuestionStatus::Open {
                owned_by,
                settled_when,
            } => format!("Open, owned by {owned_by}. Settles when {settled_when}"),
        }
    }
}

/// The five questions design document §16 leaves open, and where each one stands.
///
/// Issue [#16](https://github.com/madmax983/waymaker/issues/16) tracks them: "each needs an
/// answer before the wire format freezes at 1.0", and "each gets its own ADR when settled".
/// This list is the spec, in the same sense [`SETTLED_DECISIONS`] is: a sixth question
/// added to §16 without a row here is a question nothing tracks, and a row deleted is a
/// question that stopped being open without anyone deciding it.
///
/// Two of the five are settled here because the code already answers them and the evidence
/// exists: §09's frame has been carrying a checksum and an `EffectScheduled` digest since
/// rung 0.1. The other three are not settled, and deliberately: each names the rung that
/// owns it and the evidence that would close it. An ADR written for a question whose
/// implementation does not exist yet is a snapshot of an opinion, which is the one thing
/// [the record](https://github.com/madmax983/waymaker/blob/main/docs/adr/README.md) says a
/// decision record must never be.
pub const DEFERRED_QUESTIONS: &[DeferredQuestion] = &[
    DeferredQuestion {
        id: "integrity-check-algorithm",
        headline: "Whether the default integrity check is CRC32C or a smaller table-free CRC \
                   implementation.",
        status: QuestionStatus::Settled {
            adr: INTEGRITY_CHECK_ADR,
        },
    },
    DeferredQuestion {
        id: "retry-policy-placement",
        headline: "Whether retry policy belongs in the Embassy façade or remains workflow \
                   code.",
        status: QuestionStatus::Open {
            owned_by: "rung 0.4 · embassy",
            settled_when: "the dispatcher exists and the cost of a recorded retry \
                           representation can be measured against reimplementing backoff in \
                           every workflow",
        },
    },
    DeferredQuestion {
        id: "effect-scheduled-metadata",
        headline: "How much input metadata an EffectScheduled record stores beyond length \
                   and digest.",
        status: QuestionStatus::Settled {
            adr: EFFECT_SCHEDULED_METADATA_ADR,
        },
    },
    DeferredQuestion {
        id: "explicit-state-snapshots",
        headline: "Whether a future explicit-state workflow API may support true storage \
                   snapshots.",
        status: QuestionStatus::Open {
            owned_by: "after rung 1.0",
            settled_when: "a non-async, explicit-state API has been designed far enough that \
                           the snapshot it would take can be described in records, without \
                           relaxing the no-snapshotted-futures decision for the async façade",
        },
    },
    DeferredQuestion {
        id: "wire-format-migration",
        headline: "How stable wire-format migration is performed after a deployed fleet \
                   outlives v1.",
        status: QuestionStatus::Open {
            owned_by: "rung 1.0",
            settled_when: "the version-marker record of §09 is implemented and a fleet with \
                           two format versions in it can be described end to end",
        },
    },
];

/// The ADR that settles the `integrity-check-algorithm` question.
pub const INTEGRITY_CHECK_ADR: &str = "0010-the-integrity-check-is-catalogued-and-table-free.md";

/// The ADR that settles the `effect-scheduled-metadata` question.
pub const EFFECT_SCHEDULED_METADATA_ADR: &str =
    "0011-a-scheduled-effect-records-a-length-and-a-digest.md";

/// A diagram the architecture document must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagramSpec {
    /// The id in the diagram's `<!-- diagram: ... -->` label.
    pub id: &'static str,
    /// What the diagram is called, for a violation message.
    pub title: &'static str,
    /// Text the diagram body must contain.
    ///
    /// These are the load-bearing labels — a protocol step, a barrier, a bank generation —
    /// not the whole diagram. A diagram is free to be redrawn; it is not free to lose a
    /// step.
    pub required_labels: &'static [&'static str],
    /// Which section of the design document the diagram draws.
    pub source_section: &'static str,
}

/// The seven steps of the durable effect protocol, from design document §07.
///
/// Named here rather than inside [`DIAGRAMS`] so that the count can be asserted: §07 is a
/// seven-step protocol, and a diagram with six steps in it is a diagram that has lost one.
pub const EFFECT_PROTOCOL_STEPS: &[&str] = &[
    "Write schedule frame",
    "Payload barrier",
    "Write schedule seal, then barrier",
    "Dispatch physical activity",
    "Write outcome frame",
    "Payload barrier",
    "Write outcome seal, then barrier",
];

/// The seven steps of `continue_as_new`, from design document §10.
pub const TWO_BANK_SWAP_STEPS: &[&str] = &[
    "Stop accepting new effects",
    "Erase the inactive bank",
    "Write the new bank header",
    "Barrier: the new bank payload becomes durable",
    "Write the higher-generation bank seal",
    "Barrier: the new bank becomes authoritative",
    "lazily erase the old bank",
];

/// The six steps of cold-start replay, from design document §06.
///
/// Named here rather than inside [`DIAGRAMS`] so that the count can be asserted: §06 is a
/// six-step sequence, and a picture with five in it is a picture that has lost one.
///
/// Two of these steps draw code that does not exist yet, which is deliberate and is worth
/// stating so the diagram is not read as a claim about today's workspace. Steps 3 and 4 name
/// the workflow future, which is rung 0.4's; a recovery sequence with the polling left out
/// would read as though history were replayed by something other than the workflow itself.
/// And step 5's first clause, "consumes the matching history records", is the §08 transition
/// table of issue #15 — rung 0.1 implements its second clause, "identifies the first
/// unresolved effect". This is the one diagram whose table was written in the same commit as
/// the drawing, so unlike the other five it does not yet hold the code to an independent
/// source.
pub const COLD_START_STEPS: &[&str] = &[
    "Recover the active bank",
    "Decode the run input",
    "Create a fresh workflow future and replay cursor",
    "Poll the workflow from its beginning",
    "Each effect consumes the matching history records",
    "Stop at pending work or a terminal record",
];

/// What the crash injector enumerates and what it concludes, from design document §15.
///
/// Named here rather than inside [`DIAGRAMS`] so that the count can be asserted. The first
/// four are §15's fault families and issue #18's "fault injection covering" list; the last
/// three are the model's three record states, which the same issue asks for by name. A
/// picture that lost one of the four would understate what is checked; a picture that lost
/// one of the three would collapse "may be recovered" into "must be", which is the one
/// distinction §15 spends a paragraph on.
pub const CRASH_INJECTION_LABELS: &[&str] = &[
    "Torn write at every byte",
    "Interrupted erase at every erase block",
    "Power loss before and after every barrier",
    "Failed program or erase, writer carries on",
    "Attempted",
    "PossiblyDurable",
    "Acknowledged",
];

/// The fields of the record frame, from design document §09.
///
/// Named here rather than inside [`DIAGRAMS`] so that the count can be asserted, and named
/// as §09 names them rather than as `waymaker-flash` spells them: the diagram is a picture
/// of the design document's frame, and a field that quietly disappeared from it would be a
/// field the picture stopped accounting for. `commit_seal` is in the list even though rung
/// 0.1 does not write one — a deferred field left out of the drawing is a deferral that
/// stops being visible.
pub const RECORD_FRAME_FIELDS: &[&str] = &[
    "magic",
    "format_version",
    "record_kind",
    "effect_seq",
    "payload_len",
    "header_crc",
    // §09's own spelling, brackets and all. Plain `payload` would be satisfied by
    // `payload_len` sitting two labels above it — so the one field the whole frame exists to
    // carry could vanish from the picture with this rule still green.
    "payload [payload_len]",
    "payload_crc",
    "commit_seal",
];

/// The five rows of the replay transition table, from design document §08.
///
/// Named here rather than inside [`DIAGRAMS`] so that the count can be asserted: §08's
/// table has five "next history state" rows, and a picture with four in it is a picture
/// that has lost one — most likely the fourth, which is the only row whose engine action
/// is to stop.
///
/// Spelled as §08 spells them. "Matching schedule only" is a row of its own rather than a
/// substring of "Matching schedule + completion", so both have to be drawn.
pub const TRANSITION_TABLE_ROWS: &[&str] = &[
    "Matching schedule + completion",
    "Matching schedule only",
    "End of history",
    "Different kind, digest, or sequence",
    "Terminal workflow record",
];

/// The states and preconditions the recovery specification's journal machine has.
///
/// Design document §15 names three record states; issue #20 asks for the legal transitions
/// between them, and between bank generations, to be *stated*. The states are the nodes and
/// the preconditions are the edge labels, because a picture of the states without the guards
/// is a picture of the part that was never in question.
const RECOVERY_STATE_MACHINE_LABELS: &[&str] = &[
    "attempted",
    "possibly durable",
    "acknowledged",
    "torn",
    "append-only",
    "barrier claims only whole records",
    "durable intent before dispatch",
    "erased",
    "erasing",
    "sealing",
    "sealed",
    "never erase the authority",
    "strictly greater generation",
];

/// Every diagram issue #11 asks for, plus the recovery state machine of issue #20.
pub const DIAGRAMS: &[DiagramSpec] = &[
    DiagramSpec {
        id: CRATE_DEPENDENCY_DIAGRAM,
        title: "the crate dependency flow",
        // The nodes and edges are checked against `policy::LAYERS` instead, by
        // `check_dependency_diagram`: a hard-coded label list would have to be edited
        // every time a layer is added, which is exactly the drift this module exists to
        // stop.
        required_labels: &[],
        source_section: "§05 Architecture",
    },
    DiagramSpec {
        id: "durable-effect-protocol",
        title: "the seven-step durable effect protocol",
        required_labels: EFFECT_PROTOCOL_STEPS,
        source_section: "§07 Durable effect protocol",
    },
    DiagramSpec {
        id: "two-bank-swap",
        title: "the two-bank swap",
        required_labels: TWO_BANK_SWAP_STEPS,
        source_section: "§10 Two-bank lifecycle",
    },
    DiagramSpec {
        id: "record-frame",
        title: "the record frame",
        required_labels: RECORD_FRAME_FIELDS,
        source_section: "§09 Journal and wire format",
    },
    DiagramSpec {
        id: "cold-start-replay",
        title: "the six-step cold-start replay sequence",
        required_labels: COLD_START_STEPS,
        source_section: "§06 Cold-start replay",
    },
    DiagramSpec {
        id: "replay-transition",
        title: "the five-row replay transition table",
        required_labels: TRANSITION_TABLE_ROWS,
        source_section: "§08 Replay and determinism",
    },
    DiagramSpec {
        id: "crash-injection",
        title: "the crash injector's fault families and record states",
        required_labels: CRASH_INJECTION_LABELS,
        source_section: "\u{a7}15 Testing and verification",
    },
    DiagramSpec {
        id: "two-bank-generations",
        title: "the banks before and after the swap",
        // The generation seal is the whole mechanism: the bank with the highest valid one
        // is authoritative, so a picture of the two banks that does not show the
        // generations rising is a picture of the wrong thing.
        required_labels: &["BANK A", "BANK B", "generation 41", "generation 42"],
        source_section: "§10 Two-bank lifecycle",
    },
    DiagramSpec {
        id: "recovery-state-machine",
        title: "the journal and bank state machines the recovery specification proves over",
        required_labels: RECOVERY_STATE_MACHINE_LABELS,
        source_section: "§14 Guarantees and §15 Testing and verification",
    },
];

/// One ADR file, read off disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrFile {
    /// The file name, without any directory part.
    pub name: String,
    /// The file's contents.
    pub contents: String,
}

/// A crate root, for the `missing_docs` rule.
///
/// Every crate in the workspace, not only the firmware layers: `xtask` and the size probe
/// have public items too, and a rule that skipped them would be a rule that let the two
/// crates with the most code go undocumented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateRoot {
    /// The package the root belongs to.
    pub package: String,
    /// The target's path, relative to the workspace root, for a violation message.
    pub path: String,
    /// The file's contents.
    pub contents: String,
}

/// Everything the documentation rules read, already collected off disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsInputs {
    /// Contents of `CLAUDE.md`, when the repository has one.
    pub claude_md: Option<String>,
    /// Contents of the architecture document, when the repository has one.
    pub architecture: Option<String>,
    /// Contents of the ADR index, when the repository has one.
    pub adr_index: Option<String>,
    /// Every file in [`ADR_DIR`] except the index, in name order.
    pub adrs: Vec<AdrFile>,
    /// Contents of [`SPEC_OBLIGATIONS_PATH`], when the workspace has it.
    ///
    /// `None` is a violation rather than a skip: a specification whose clause table cannot
    /// be read is a specification nothing is holding to its documentation.
    pub spec_obligations: Option<String>,
    /// Every crate root in the workspace, in package-then-path order.
    ///
    /// A workspace member that contributes no root at all is reported by
    /// `inputs-incomplete`, which is the rule that already exists for "a crate no rule
    /// could be run against"; it is not a second thing for this module to say.
    pub crate_roots: Vec<CrateRoot>,
}

/// A labelled Mermaid block from a Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MermaidBlock {
    /// The id from the `<!-- diagram: ... -->` label above the fence.
    id: String,
    /// The fence's contents, without the fence lines or the `%%` comments.
    body: String,
}

/// The number an ADR file name carries, or `None` if it is not a numbered ADR.
///
/// `0003-the-eight-settled-design-decisions.md` is 3. `README.md` is `None`, and so is
/// `003-short.md`: a four-digit prefix is what makes the files sort in decision order for
/// the rest of the record's life.
#[must_use]
// `.md`, lowercase, on purpose: the record's file names are a fixed shape, and treating
// `README.MD` as an ADR would be a bug rather than a courtesy.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "ADR file names are lowercase by policy"
)]
pub fn adr_number(file_name: &str) -> Option<u32> {
    let (digits, rest) = file_name.split_at_checked(4)?;
    if !rest.starts_with('-') || !file_name.ends_with(".md") {
        return None;
    }
    if digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

/// Every `<!-- diagram: id -->`-labelled Mermaid block in `contents`.
///
/// The label rather than the nearest heading, because a heading is prose: rewording one is
/// a normal thing to do to a document and would silently detach the diagram from the rule
/// that checks it. Blank lines between the label and the fence are allowed; anything else
/// between them means the label belongs to something other than the fence.
///
/// Three things a naive scan gets wrong, each of which is a way to satisfy a rule with
/// something a reader never sees:
///
/// * **An indented fence.** Four spaces make Markdown render the block as literal text, so
///   an indented ```` ```mermaid ```` is a diagram that is quoted rather than drawn.
/// * **A fence inside a longer fence.** A worked example of how to label a diagram, quoted
///   inside a ```` ````` ```` block, is not a diagram either.
/// * **A Mermaid `%%` comment.** It is in the source and not in the picture, so it is
///   stripped from [`MermaidBlock::body`] before any rule matches against it.
#[must_use]
fn mermaid_blocks(contents: &str) -> Vec<MermaidBlock> {
    let mut blocks = Vec::new();
    let mut pending_id: Option<String> = None;
    let mut collecting: Option<(String, Vec<&str>)> = None;
    // The character and length of the fence that opened the block we are inside, mermaid or
    // not. A fence is closed only by a run of at least as many of its own character, which
    // is what keeps a quoted example from being read as the real thing.
    let mut open_fence: Option<(u8, usize)> = None;

    for line in contents.lines() {
        let indent = line.len().saturating_sub(line.trim_start().len());
        let trimmed = line.trim();
        let fence = fence_length(trimmed);

        if let Some((marker, width)) = open_fence {
            // Inside a fence: only a long-enough closing fence of the *same* character ends
            // it, and only when it is not itself indented into a code block.
            if fence.is_some_and(|(found, length)| found == marker && length >= width)
                && indent < 4
                && trimmed.len() == fence.map_or(0, |(_, length)| length)
            {
                if let Some((id, body)) = collecting.take() {
                    blocks.push(MermaidBlock {
                        id,
                        body: strip_mermaid_comments(&body),
                    });
                }
                open_fence = None;
            } else if let Some((_, body)) = collecting.as_mut() {
                body.push(line);
            }
            continue;
        }

        if let Some((marker, length)) = fence {
            // An indented fence is literal text in the rendered page, so it opens nothing
            // a reader would call a diagram.
            if indent < 4 {
                open_fence = Some((marker, length));
                if trimmed
                    .get(length..)
                    .is_some_and(|info| info.trim() == "mermaid")
                    && let Some(id) = pending_id.take()
                {
                    collecting = Some((id, Vec::new()));
                }
            }
            continue;
        }

        if let Some(id) = diagram_label(trimmed) {
            pending_id = Some(id.to_owned());
            continue;
        }

        if !trimmed.is_empty() {
            pending_id = None;
        }
    }

    blocks
}

/// The fence character and its run length, when `line` starts a fence of three or more.
///
/// Markdown fences with `~~~` as well as with backticks, and a fence is closed only by a
/// run of its own character — Codex pointed out on PR #58 that knowing about one of them
/// leaves a hole of exactly the shape the fence check exists to close.
fn fence_length(line: &str) -> Option<(u8, usize)> {
    let marker = line
        .bytes()
        .next()
        .filter(|byte| matches!(byte, b'`' | b'~'))?;
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((marker, length))
}

/// `body` without the Mermaid `%%` comment lines, which are in the source and not in the
/// picture.
fn strip_mermaid_comments(body: &[&str]) -> String {
    body.iter()
        .filter(|line| !line.trim_start().starts_with("%%"))
        .copied()
        .collect::<Vec<&str>>()
        .join("\n")
}

/// The id in `<!-- diagram: some-id -->`, if `line` is such a label.
fn diagram_label(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    Some(inner.strip_prefix("diagram:")?.trim())
}

/// `contents` with every `<!-- ... -->` comment removed.
///
/// A rule that matches on text a reader can see is a rule about the document. One that also
/// matches text inside an HTML comment can be satisfied by a block of ids pasted at the
/// bottom of an otherwise empty file, which is the opposite of what these rules are for.
#[must_use]
fn without_html_comments(contents: &str) -> String {
    let mut kept = String::new();
    let mut rest = contents;
    while let Some((before, after)) = rest.split_once("<!--") {
        kept.push_str(before);
        // A space so that text either side of a comment does not fuse into one token.
        kept.push(' ');
        let Some((_, resumed)) = after.split_once("-->") else {
            return kept;
        };
        rest = resumed;
    }
    kept.push_str(rest);
    kept
}

/// `contents` with every fenced code block removed.
///
/// A link or a heading inside a fence is displayed as literal text: it is an example of
/// Markdown, not Markdown. Used where a rule asks whether a reader can follow something.
#[must_use]
fn without_fenced_code(contents: &str) -> String {
    let mut kept = Vec::new();
    let mut open_fence: Option<(u8, usize)> = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        let fence = fence_length(trimmed);
        match open_fence {
            Some((marker, width))
                if fence.is_some_and(|(found, length)| found == marker && length >= width) =>
            {
                open_fence = None;
            }
            Some(_) => {}
            None if fence.is_some() => open_fence = fence,
            None => kept.push(line),
        }
    }
    kept.join("\n")
}

/// Rule: `CLAUDE.md` exists, quotes the layering table, and names every gate rule.
///
/// The "must not own" cells are compared against [`LAYERS`] verbatim. A contributor
/// reading `CLAUDE.md` is reading the same string the gate reads, or the gate says so.
#[must_use]
fn check_claude_md(contents: Option<&str>, rules: &[&str]) -> Vec<Violation> {
    let Some(contents) = contents else {
        return vec![Violation::new(
            "claude-md",
            CLAUDE_MD_PATH,
            "the repository has no CLAUDE.md; the invariants, the layering rules and the \
             must-not-own table live there",
        )];
    };

    // Matched against what a reader sees. A block of ids inside an HTML comment would
    // otherwise satisfy every check below in a file that says nothing.
    let contents = &without_html_comments(contents);
    let mut violations = Vec::new();

    for spec in LAYERS {
        if !contents.contains(spec.name) {
            violations.push(Violation::new(
                "claude-md",
                spec.name,
                "CLAUDE.md does not mention this layer",
            ));
        }
        if !contents.contains(spec.must_not_own) {
            violations.push(Violation::new(
                "claude-md",
                spec.name,
                format!(
                    "CLAUDE.md's must-not-own entry does not match policy::LAYERS, which reads \
                     `{}`",
                    spec.must_not_own
                ),
            ));
        }
        // The other half of the layering contract. Without this a row could say the kernel
        // may depend on the façade — the invariant inverted, in the table the file
        // introduces as the layering rules — and the gate would have nothing to say.
        if !contents.contains(&spec.render_allowed()) {
            violations.push(Violation::new(
                "claude-md",
                spec.name,
                format!(
                    "CLAUDE.md does not say this layer may depend on `{}`, which is what \
                     policy::LAYERS permits",
                    spec.render_allowed()
                ),
            ));
        }
    }

    for decision in SETTLED_DECISIONS {
        if !contents.contains(decision.id) {
            violations.push(Violation::new(
                "claude-md",
                decision.id,
                "CLAUDE.md does not cite this settled decision",
            ));
        }
    }

    for rule in rules {
        // Backticked, so that `size-probe` is not satisfied by `size-probe-reach` on the
        // next line — a bare substring makes one rule vouch for another.
        if !contents.contains(&format!("`{rule}`")) {
            violations.push(Violation::new(
                "claude-md",
                *rule,
                "CLAUDE.md does not name this gate rule in backticks, so a contributor \
                 cannot tell in advance what the gate will reject",
            ));
        }
    }

    // The count the gate prints on success. Prose until it is checked, and it was wrong
    // once already: the rule table grew and the sentence above it did not.
    let count = format!("{} rule", rules.len());
    if !contents.contains(&count) {
        violations.push(Violation::new(
            "claude-md",
            "rule count",
            format!("CLAUDE.md does not say `{count}s`, which is what the gate reports"),
        ));
    }

    for command in crate::pipeline::STAGES {
        if !contents.contains(command.command) {
            violations.push(Violation::new(
                "claude-md",
                command.name,
                format!(
                    "CLAUDE.md's command list does not include `{}`; a contributor who runs \
                     the list and claims it works would still be discovering that stage in CI",
                    command.command
                ),
            ));
        }
    }

    for path in [ADR_DIR, ARCHITECTURE_PATH] {
        if !contents.contains(path) {
            violations.push(Violation::new(
                "claude-md",
                path,
                "CLAUDE.md does not point at this, so the decision record is discoverable \
                 only by browsing the tree",
            ));
        }
    }

    violations
}

/// Rule: the recovery specification, its documentation and its proofs name one set of
/// clauses.
///
/// Issue [#20](https://github.com/madmax983/waymaker/issues/20)'s third exit criterion is
/// that "any change to the record representation updates the model and invariants first,
/// then the proofs, then the code". Nothing mechanical can check the *order*, but this can
/// check that the four places a clause lives never disagree: [`SPEC_CLAUSES`], `CLAUDE.md`,
/// [`RECOVERY_SPEC_ADR`], and the crate's own table at [`SPEC_OBLIGATIONS_PATH`]. A clause
/// deleted from the crate with the documentation still describing it, and a clause written
/// into the documentation that no proof discharges, are the two ways a specification rots,
/// and each fails here.
#[must_use]
fn check_recovery_spec(
    claude_md: Option<&str>,
    adrs: &[AdrFile],
    obligations: Option<&str>,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    match obligations {
        None => violations.push(Violation::new(
            "recovery-spec",
            SPEC_OBLIGATIONS_PATH,
            "the specification's clause table is not where the gate looks for it, so \
             nothing holds the proofs to the documentation",
        )),
        Some(contents) => {
            let declared = spec_clause_rows(contents);
            for clause in SPEC_CLAUSES {
                let Some(proof) = declared.get(clause.id) else {
                    violations.push(Violation::new(
                        "recovery-spec",
                        clause.id,
                        format!(
                            "{SPEC_OBLIGATIONS_PATH} declares no clause with this id, so the \
                             guarantee has documentation and no proof behind it"
                        ),
                    ));
                    continue;
                };
                // The proof as well as the id, because the id alone leaves the crate free to
                // point a guarantee at a different file than the one CLAUDE.md tells a
                // reader to look in — two tables agreeing on the names of six things and
                // disagreeing about all six of them.
                if *proof != Some(clause.discharged_by) {
                    violations.push(Violation::new(
                        "recovery-spec",
                        clause.id,
                        proof.map_or_else(
                            || {
                                format!(
                                    "{SPEC_OBLIGATIONS_PATH}'s row for this guarantee names \
                                     no proof, and docs::SPEC_CLAUSES says `{}`",
                                    clause.discharged_by
                                )
                            },
                            |named| {
                                format!(
                                    "{SPEC_OBLIGATIONS_PATH} discharges this guarantee with \
                                     `{named}` and docs::SPEC_CLAUSES says `{}`",
                                    clause.discharged_by
                                )
                            },
                        ),
                    ));
                }
            }
            for id in declared.keys() {
                if !SPEC_CLAUSES.iter().any(|clause| clause.id == *id) {
                    violations.push(Violation::new(
                        "recovery-spec",
                        (*id).to_owned(),
                        format!(
                            "{SPEC_OBLIGATIONS_PATH} declares this clause and docs::SPEC_CLAUSES \
                             does not, so a guarantee is proved and undocumented"
                        ),
                    ));
                }
            }
        }
    }

    violations.extend(check_spec_clauses_are_written_down(claude_md));
    violations.extend(check_spec_clauses_are_decided(adrs));
    violations
}

/// Every `(clause id, proof)` pair [`SPEC_OBLIGATIONS_PATH`] declares.
///
/// Matched on the table's own `id: "..."` and `proof: "..."` fields rather than on the whole
/// file, so a clause id that only appears in a doc comment does not vouch for a row that is
/// not there. A row's proof is the field that follows its id, which is the order the type
/// declares them in; a row that reorders its fields reads as having no proof, which is a
/// violation rather than a silent pass.
fn spec_clause_rows(contents: &str) -> BTreeMap<&str, Option<&str>> {
    let mut rows = BTreeMap::new();
    for (index, _) in contents.match_indices("id: \"") {
        let Some(after_key) = index.checked_add("id: \"".len()) else {
            continue;
        };
        let Some(rest) = contents.get(after_key..) else {
            continue;
        };
        let Some(end) = rest.find('"') else { continue };
        let Some(id) = rest.get(..end) else { continue };
        if id.is_empty() {
            continue;
        }
        // The row runs to the next clause's id, so a proof belongs to the row above it.
        let tail = rest.get(end..).unwrap_or_default();
        let row = tail
            .find("id: \"")
            .and_then(|next| tail.get(..next))
            .unwrap_or(tail);
        rows.insert(id, quoted_field(row, "proof: \""));
    }
    rows
}

/// The string literal following `key` in `row`, if there is one.
fn quoted_field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    let index = row.find(key)?;
    let rest = row.get(index.checked_add(key.len())?..)?;
    let end = rest.find('"')?;
    rest.get(..end)
}

/// The half of `recovery-spec` that reads `CLAUDE.md`.
fn check_spec_clauses_are_written_down(claude_md: Option<&str>) -> Vec<Violation> {
    let Some(contents) = claude_md else {
        // `claude-md` already reports the missing file.
        return Vec::new();
    };
    // Fences as well as comments, for the reason the ADR half strips them: a fenced example
    // listing the six ids would otherwise satisfy every check below in a file whose table
    // has been deleted.
    let contents = without_fenced_code(&without_html_comments(contents));
    let mut violations = Vec::new();
    for clause in SPEC_CLAUSES {
        // The clause's own table row, found by its backticked id — not three global
        // `contains` calls, which is the defect `deferred-questions` was fixed for on
        // PR #58 and which bites harder here: four of the six clauses name the same proof
        // file, so a whole-file check for `tests/spine.rs` is satisfied by any one of their
        // rows on behalf of all four.
        let rows: Vec<&str> = contents
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('|') && line.contains(&format!("`{}`", clause.id)))
            .collect();
        let [row] = rows.as_slice() else {
            violations.push(Violation::new(
                "recovery-spec",
                clause.id,
                if rows.is_empty() {
                    "CLAUDE.md has no table row naming this recovery invariant in backticks, \
                     so a contributor cannot tell which guarantee a change is touching"
                        .to_owned()
                } else {
                    format!(
                        "CLAUDE.md has {} table rows naming this recovery invariant, so which \
                         one a reader believes depends on which they reach first",
                        rows.len()
                    )
                },
            ));
            continue;
        };

        if !row.contains(clause.headline) {
            violations.push(Violation::new(
                "recovery-spec",
                clause.id,
                format!(
                    "CLAUDE.md's table row for this guarantee does not state it as `{}`, \
                     which is what docs::SPEC_CLAUSES reads",
                    clause.headline
                ),
            ));
        }
        if !row.contains(clause.discharged_by) {
            violations.push(Violation::new(
                "recovery-spec",
                clause.id,
                format!(
                    "CLAUDE.md's table row does not say this guarantee is discharged by \
                     `{}`, so the row describes a proof without pointing at one",
                    clause.discharged_by
                ),
            ));
        }
    }
    let count = format!("All {} recovery invariants", SPEC_CLAUSES.len());
    if !contents.contains(&count) {
        violations.push(Violation::new(
            "recovery-spec",
            "clause count",
            format!("CLAUDE.md does not say `{count}`, which is what the table holds"),
        ));
    }
    violations
}

/// The half of `recovery-spec` that reads the decision record.
fn check_spec_clauses_are_decided(adrs: &[AdrFile]) -> Vec<Violation> {
    let Some(adr) = adrs.iter().find(|adr| adr.name == RECOVERY_SPEC_ADR) else {
        return vec![Violation::new(
            "recovery-spec",
            RECOVERY_SPEC_ADR,
            "the recovery specification has no decision record, so the shape of the proof \
             is a choice nobody wrote down",
        )];
    };
    let contents = without_fenced_code(&without_html_comments(&adr.contents));
    SPEC_CLAUSES
        .iter()
        .filter(|clause| !contents.contains(&format!("`{}`", clause.id)))
        .map(|clause| {
            Violation::new(
                "recovery-spec",
                clause.id,
                format!(
                    "{RECOVERY_SPEC_ADR} does not name this clause in backticks, so the \
                     guarantee has a proof and no decision record behind it"
                ),
            )
        })
        .collect()
}

/// Rule: the ADR record is numbered without gaps or duplicates, and has its template.
#[must_use]
fn check_adr_numbering(adrs: &[AdrFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut by_number: BTreeMap<u32, Vec<&str>> = BTreeMap::new();

    for adr in adrs {
        let Some(number) = adr_number(&adr.name) else {
            violations.push(Violation::new(
                "adr-numbering",
                adr.name.clone(),
                "is in docs/adr but is not named `NNNN-slug.md`; four digits are what keep \
                 the record in decision order",
            ));
            continue;
        };
        by_number.entry(number).or_default().push(&adr.name);
    }

    for (number, names) in &by_number {
        if names.len() > 1 {
            violations.push(Violation::new(
                "adr-numbering",
                format!("{number:04}"),
                format!("is claimed by more than one ADR: {}", names.join(", ")),
            ));
        }
    }

    // By name, not by number: any `0000-*.md` would satisfy "something is numbered zero"
    // while the path the index and CLAUDE.md link to no longer exists.
    if !adrs.iter().any(|adr| adr.name == ADR_TEMPLATE) {
        violations.push(Violation::new(
            "adr-numbering",
            ADR_TEMPLATE,
            "the record has no template at that exact path; a new ADR is then written from \
             whichever one the author happened to open, and the links to it are dead",
        ));
    }

    let mut expected = ADR_NUMBER_START;
    for number in by_number.keys().copied().filter(|number| *number > 0) {
        if number != expected {
            violations.push(Violation::new(
                "adr-numbering",
                format!("{number:04}"),
                format!(
                    "leaves a gap: the record has no ADR {expected:04}. Numbers are \
                     consecutive so that a missing decision is visible rather than assumed \
                     to be unwritten"
                ),
            ));
        }
        expected = number.saturating_add(1);
    }

    violations
}

/// Rule: every ADR carries a title, a recognised status, a date, and the required
/// sections.
#[must_use]
fn check_adr_structure(adrs: &[AdrFile]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for adr in adrs {
        // An ADR whose `- Status:` and `- Date:` sit inside an HTML comment renders with no
        // metadata at all, and every check below would otherwise find them.
        let contents = without_html_comments(&adr.contents);
        if !contents.lines().any(|line| line.starts_with("# ")) {
            violations.push(Violation::new(
                "adr-structure",
                adr.name.clone(),
                "has no `# ` title line",
            ));
        }

        for field in ADR_REQUIRED_FIELDS {
            if !contents
                .lines()
                .any(|line| line.trim_start().starts_with(field))
            {
                violations.push(Violation::new(
                    "adr-structure",
                    adr.name.clone(),
                    format!("has no `{field}` line"),
                ));
            }
        }

        if let Some(status) = adr_status(&contents) {
            // The template's placeholder is the one status that is allowed to be
            // unrecognised, because the template records no decision.
            let is_template = adr_number(&adr.name) == Some(0);
            if !is_template && !ADR_STATUSES.contains(&status.as_str()) {
                violations.push(Violation::new(
                    "adr-structure",
                    adr.name.clone(),
                    format!(
                        "has status `{status}`, which is not one of: {}",
                        ADR_STATUSES.join(", ")
                    ),
                ));
            }
        }

        for heading in ADR_REQUIRED_HEADINGS {
            if !contents.lines().any(|line| line.trim_end() == *heading) {
                violations.push(Violation::new(
                    "adr-structure",
                    adr.name.clone(),
                    format!("has no `{heading}` section"),
                ));
            }
        }
    }

    violations
}

/// The lowercased value of an ADR's `- Status:` line, if it has one.
fn adr_status(contents: &str) -> Option<String> {
    contents
        .lines()
        .map(str::trim_start)
        .find_map(|line| line.strip_prefix("- Status:"))
        .map(|status| status.trim().to_lowercase())
}

/// Rule: the ADR index links every ADR, and links nothing that is not one.
#[must_use]
fn check_adr_index(index: Option<&str>, adrs: &[AdrFile]) -> Vec<Violation> {
    let Some(index) = index else {
        return vec![Violation::new(
            "adr-index",
            format!("{ADR_DIR}/{ADR_INDEX}"),
            "the decision record has no index",
        )];
    };

    // Both directions read the same parsed link list, and it is parsed from what renders.
    // Asking only whether the file *name* appears would accept
    // `[0001-one.md](../architecture.md)`, where the ADR is mentioned and not linked; and a
    // link inside an HTML comment or a fenced example is text about a link rather than one
    // — which is the whole of what an index is for.
    let linked = linked_markdown_files(&without_fenced_code(&without_html_comments(index)));

    let mut violations: Vec<Violation> = adrs
        .iter()
        .filter(|adr| !linked.contains(&adr.name))
        .map(|adr| {
            Violation::new(
                "adr-index",
                adr.name.clone(),
                "exists in docs/adr but no link in the index points at it",
            )
        })
        .collect();

    // The reverse direction: a link to an ADR that was renamed or never landed reads as a
    // decision the project took, and clicking it is the only way to find out otherwise.
    for target in linked {
        if adr_number(file_name(&target)).is_some() && !adrs.iter().any(|adr| adr.name == target) {
            violations.push(Violation::new(
                "adr-index",
                target,
                "is linked from the index but does not exist in docs/adr",
            ));
        }
    }

    violations
}

/// Every `*.md` target of a Markdown link in `contents`, as written.
///
/// The directory part is kept: `missing/0001-one.md` is a broken link, not a link to
/// `0001-one.md`, and comparing basenames would count it as indexing the real ADR while
/// leaving a link a reader cannot follow in the file.
///
/// Handles the four spellings this record actually uses or could grow into — a bare target,
/// an angle-bracketed one, a target followed by a `"title"`, and a reference definition
/// (`[label]: target`). Each of the first three otherwise reads as "no link at all", which
/// makes the reverse half of [`check_adr_index`] fail open on a dead link.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "ADR file names are lowercase by policy"
)]
fn linked_markdown_files(contents: &str) -> Vec<String> {
    let mut found = Vec::new();

    let mut rest = contents;
    while let Some(open) = rest.find("](") {
        let after = rest.get(open.saturating_add(2)..).unwrap_or_default();
        let Some(close) = after.find(')') else {
            break;
        };
        if let Some(name) = link_target(after.get(..close).unwrap_or_default()) {
            found.push(name);
        }
        rest = after.get(close..).unwrap_or_default();
    }

    // Reference definitions: `[0007-ghost]: 0007-ghost.md "ADR 7"`.
    for line in contents.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix('[')
            && let Some((_, target)) = rest.split_once("]:")
            && let Some(name) = link_target(target)
        {
            found.push(name);
        }
    }

    found.retain(|target| file_name(target).ends_with(".md"));
    found
}

/// The last path segment of `target`.
fn file_name(target: &str) -> &str {
    target.rsplit('/').next().unwrap_or(target)
}

/// The file name a Markdown link target names, stripped of angle brackets and a title.
fn link_target(target: &str) -> Option<String> {
    let target = target.trim();
    let target = target
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(target);
    // `path.md "A title"` and `path.md 'A title'` both name `path.md`.
    let target = target
        .split_once(['"', '\''])
        .map_or(target, |(before, _)| before)
        .trim();
    (!target.is_empty()).then(|| target.to_owned())
}

/// Rule: the ADR recording design document §02 records all eight decisions.
#[must_use]
fn check_settled_decisions(adrs: &[AdrFile]) -> Vec<Violation> {
    let Some(adr) = adrs.iter().find(|adr| adr.name == SETTLED_DECISIONS_ADR) else {
        return vec![Violation::new(
            "settled-decisions",
            SETTLED_DECISIONS_ADR,
            "the eight decisions design document §02 settles have no ADR",
        )];
    };

    let contents = without_html_comments(&adr.contents);
    SETTLED_DECISIONS
        .iter()
        .flat_map(|decision| {
            let mut missing = Vec::new();
            if !contents.contains(decision.id) {
                missing.push(Violation::new(
                    "settled-decisions",
                    decision.id,
                    format!("is not recorded in {SETTLED_DECISIONS_ADR}"),
                ));
            }
            if !contents.contains(decision.headline) {
                missing.push(Violation::new(
                    "settled-decisions",
                    decision.id,
                    format!(
                        "is recorded without its headline, which reads `{}`",
                        decision.headline
                    ),
                ));
            }
            missing
        })
        .collect()
}

/// Every `Settles deferred question:` claim in `contents`, in source order.
///
/// `contents` is expected to have had its HTML comments and its fenced code blocks stripped
/// already: a marker a reader cannot see claims nothing, and a marker inside a fence is an
/// example of the syntax rather than a use of it — `docs/adr/README.md` contains exactly
/// such an example. The id is the rest of the line, trimmed of whitespace and of the
/// backticks a Markdown author naturally puts round an identifier.
#[must_use]
fn deferred_question_claims(contents: &str) -> Vec<&str> {
    contents
        .lines()
        .filter_map(|line| line.split_once(DEFERRED_QUESTION_MARKER))
        .map(|(_, rest)| rest.trim().trim_matches('`').trim())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Rule: §16's deferred questions are tracked, and a settled one has an accepted ADR.
///
/// Three things, and the second and third are the ones that make this more than a list.
///
/// * `CLAUDE.md` carries every question — its id, its headline, and the count — so a
///   contributor can see what is undecided without opening the design document.
/// * A question the table calls **settled** has an ADR that exists, is `accepted`, and says
///   so with [`DEFERRED_QUESTION_MARKER`]. An ADR that is merely *about* the subject does
///   not count: that is a judgement about prose, and prose is what this rule exists to stop
///   standing in for a record.
/// * A question the table calls **open** has no ADR claiming it, and no ADR claims a
///   question the table has never heard of. This is the direction a list like this normally
///   rots in — somebody settles a question, writes the ADR, and leaves the row saying
///   nobody has.
///
/// What it does not check, so nobody reads more into a green build than is there: whether
/// the ADR's answer is any *good*, and whether §16 of the design document still lists these
/// five. The design document is a checked-in HTML file that predates the table; the table is
/// the spec, and a sixth question is a row somebody writes.
#[must_use]
fn check_deferred_questions(claude_md: Option<&str>, adrs: &[AdrFile]) -> Vec<Violation> {
    // Every claim in the record, with the ADR that made it, so both directions below can be
    // answered from one pass.
    let mut claims: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    // Comments *and* fences. Codex raised the second on PR #58: an ADR that documents the
    // marker syntax in a fenced example would otherwise be read as claiming the question,
    // and — worse — an ADR that kept the example while losing its real marker would still
    // satisfy the settled check. `check_adr_index` already reads documents this way.
    let stripped: Vec<(&str, String)> = adrs
        .iter()
        .map(|adr| {
            (
                adr.name.as_str(),
                without_fenced_code(&without_html_comments(&adr.contents)),
            )
        })
        .collect();
    for (name, contents) in &stripped {
        for id in deferred_question_claims(contents) {
            claims.entry(id).or_default().push(name);
        }
    }

    let mut violations = check_questions_are_written_down(claude_md);
    violations.extend(check_questions_match_the_record(adrs, &claims));

    for (id, names) in &claims {
        if DEFERRED_QUESTIONS.iter().any(|question| question.id == *id) {
            continue;
        }
        for name in names {
            violations.push(Violation::new(
                "deferred-questions",
                (*name).to_owned(),
                format!(
                    "settles `{id}`, which is not a question in DEFERRED_QUESTIONS; either \
                     the id is a typo or §16 grew a question nothing tracks"
                ),
            ));
        }
    }

    violations
}

/// The half of `deferred-questions` that reads `CLAUDE.md`.
///
/// A contributor should be able to see what is undecided without opening the design
/// document, so every question's id, its headline and the count have to be in the file a
/// contributor is pointed at first.
#[must_use]
fn check_questions_are_written_down(claude_md: Option<&str>) -> Vec<Violation> {
    let Some(contents) = claude_md else {
        // `claude-md` already reports the missing file. Saying it twice would make the
        // report longer without making it more informative, and the record half of this
        // rule still runs.
        return vec![Violation::new(
            "deferred-questions",
            CLAUDE_MD_PATH,
            "the repository has no CLAUDE.md, so the deferred questions are recorded \
             nowhere a contributor reads",
        )];
    };

    let contents = without_html_comments(contents);
    let mut violations = Vec::new();

    for question in DEFERRED_QUESTIONS {
        // The question's own table row, found by its backticked id so that
        // `integrity-check-algorithm` cannot be vouched for by a longer id containing it.
        //
        // A row rather than the whole file, which is what Codex caught on PR #58: three
        // global `contains` calls are satisfied by the id, the headline and a status sitting
        // in three unrelated places, so a row could state the opposite of the table and pass.
        // And a *table* row rather than any line, which Codex caught in the round after
        // that: prose mentioning an id above the table shadowed the row, which fails a
        // correct file and passes one whose row has gone stale.
        let rows: Vec<&str> = contents
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('|') && line.contains(&format!("`{}`", question.id)))
            .collect();
        let [row] = rows.as_slice() else {
            violations.push(Violation::new(
                "deferred-questions",
                question.id,
                if rows.is_empty() {
                    "CLAUDE.md has no table row naming this deferred question in backticks, \
                     so a contributor cannot tell which design questions are still open"
                        .to_owned()
                } else {
                    format!(
                        "CLAUDE.md has {} table rows naming this deferred question, so which \
                         one a reader believes depends on which they reach first",
                        rows.len()
                    )
                },
            ));
            continue;
        };

        if !row.contains(question.headline) {
            violations.push(Violation::new(
                "deferred-questions",
                question.id,
                format!(
                    "CLAUDE.md's table row for this question does not carry its headline, \
                     which reads `{}`",
                    question.headline
                ),
            ));
        }
        let status = question.render_status();
        if !row.contains(&status) {
            violations.push(Violation::new(
                "deferred-questions",
                question.id,
                format!(
                    "CLAUDE.md's table row does not say where it stands, which \
                     DEFERRED_QUESTIONS renders as `{status}`; a row free to disagree with \
                     the table is a row that eventually does"
                ),
            ));
        }
    }

    let count = format!("{} deferred question", DEFERRED_QUESTIONS.len());
    if !contents.contains(&count) {
        violations.push(Violation::new(
            "deferred-questions",
            "question count",
            format!("CLAUDE.md does not say `{count}s`, which is what §16 leaves open"),
        ));
    }

    violations
}

/// The half of `deferred-questions` that reads the decision record.
///
/// `claims` maps a question id to every ADR carrying [`DEFERRED_QUESTION_MARKER`] for it.
/// A settled question needs exactly one, accepted; an open question needs none.
#[must_use]
fn check_questions_match_the_record(
    adrs: &[AdrFile],
    claims: &BTreeMap<&str, Vec<&str>>,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    for question in DEFERRED_QUESTIONS {
        let claimed_by = claims.get(question.id).map_or(&[][..], Vec::as_slice);
        match question.status {
            QuestionStatus::Settled { adr } => {
                let Some(file) = adrs.iter().find(|file| file.name == adr) else {
                    violations.push(Violation::new(
                        "deferred-questions",
                        question.id,
                        format!(
                            "is settled by {adr}, which is not in {ADR_DIR}; a question \
                             whose record is missing is a question nobody can read the \
                             answer to"
                        ),
                    ));
                    continue;
                };
                if !claimed_by.contains(&adr) {
                    violations.push(Violation::new(
                        "deferred-questions",
                        question.id,
                        format!(
                            "is settled by {adr}, which carries no `{DEFERRED_QUESTION_MARKER} \
                             {}` line; an ADR that is merely about the subject is not a \
                             record that it was decided",
                            question.id
                        ),
                    ));
                }
                if let Some(status) = adr_status(&file.contents)
                    && status != "accepted"
                {
                    violations.push(Violation::new(
                        "deferred-questions",
                        question.id,
                        format!(
                            "is settled by {adr}, whose status is `{status}`; a question \
                             answered by a draft is not answered"
                        ),
                    ));
                }
                for extra in claimed_by.iter().filter(|name| **name != adr) {
                    violations.push(Violation::new(
                        "deferred-questions",
                        question.id,
                        format!(
                            "is claimed by {extra} as well as by {adr}; which one settled it \
                             would depend on the order the record is read in"
                        ),
                    ));
                }
            }
            QuestionStatus::Open { .. } => {
                for name in claimed_by {
                    violations.push(Violation::new(
                        "deferred-questions",
                        question.id,
                        format!(
                            "is open in DEFERRED_QUESTIONS, but {name} already claims to \
                             settle it; the table is what CLAUDE.md is checked against, so \
                             it is the table that is stale"
                        ),
                    ));
                }
            }
        }
    }

    violations
}

/// Rule: every diagram issue #11 asks for exists, is Mermaid, and shows every step.
#[must_use]
fn check_diagrams(architecture: Option<&str>) -> Vec<Violation> {
    let Some(architecture) = architecture else {
        return vec![Violation::new(
            "diagrams",
            ARCHITECTURE_PATH,
            "the repository has no architecture document, so it has no diagrams",
        )];
    };

    let blocks = mermaid_blocks(architecture);
    let mut violations = Vec::new();

    // A second block carrying an id would otherwise shadow the real diagram, because the
    // lookup below takes the first match — an example of how to label a diagram, left in
    // the document, would satisfy every rule for the diagram it is an example of.
    for (position, block) in blocks.iter().enumerate() {
        if blocks
            .iter()
            .take(position)
            .any(|earlier| earlier.id == block.id)
        {
            violations.push(Violation::new(
                "diagrams",
                block.id.clone(),
                "labels more than one mermaid block, so which one the rules read depends on \
                 the order they appear in",
            ));
        }
    }

    for spec in DIAGRAMS {
        let Some(block) = blocks.iter().find(|block| block.id == spec.id) else {
            violations.push(Violation::new(
                "diagrams",
                spec.id,
                format!(
                    "{ARCHITECTURE_PATH} has no mermaid block labelled `<!-- diagram: {} -->` \
                     for {} (design document {})",
                    spec.id, spec.title, spec.source_section
                ),
            ));
            continue;
        };

        for label in spec.required_labels {
            // Counted, not merely found. `Payload barrier` is two of the seven steps in
            // §07's protocol, so a diagram that drops the second one still contains the
            // string — and the step it drops is the barrier before the outcome seal.
            let required = spec
                .required_labels
                .iter()
                .filter(|other| *other == label)
                .count();
            let drawn = block.body.matches(label).count();
            if drawn < required {
                violations.push(Violation::new(
                    "diagrams",
                    spec.id,
                    format!(
                        "shows `{label}` {drawn} time(s) where design document {} lists it \
                         {required} time(s)",
                        spec.source_section
                    ),
                ));
            }
        }
    }

    violations.extend(check_dependency_diagram(&blocks));
    violations
}

/// The crate dependency diagram, checked against [`LAYERS`] rather than against a list.
///
/// Both directions. Asking only that every permitted edge is drawn would accept a diagram
/// that also draws `waymaker-core --> waymaker-embassy` — the layering inverted, in the one
/// picture a reader trusts to tell them which way it goes — so an edge between two layers
/// that the table does not permit is a violation too.
fn check_dependency_diagram(blocks: &[MermaidBlock]) -> Vec<Violation> {
    let Some(block) = blocks
        .iter()
        .find(|block| block.id == CRATE_DEPENDENCY_DIAGRAM)
    else {
        // Already reported as a missing diagram.
        return Vec::new();
    };

    let mut violations = Vec::new();
    let drawn = drawn_edges(&block.body);

    for spec in LAYERS {
        if !block.body.contains(spec.name) {
            violations.push(Violation::new(
                "diagrams",
                CRATE_DEPENDENCY_DIAGRAM,
                format!("does not draw `{}`, which policy::LAYERS lists", spec.name),
            ));
        }
        for dependency in spec.may_depend_on {
            if !drawn
                .iter()
                .any(|(from, to)| from == spec.name && to == dependency)
            {
                violations.push(Violation::new(
                    "diagrams",
                    CRATE_DEPENDENCY_DIAGRAM,
                    format!(
                        "does not draw the edge `{} --> {dependency}`, which policy::LAYERS \
                         permits and the layering gate enforces",
                        spec.name
                    ),
                ));
            }
        }
    }

    for (from, to) in &drawn {
        let permitted = crate::policy::layer(from)
            .is_some_and(|spec| spec.may_depend_on.contains(&to.as_str()));
        if !permitted {
            violations.push(Violation::new(
                "diagrams",
                CRATE_DEPENDENCY_DIAGRAM,
                format!(
                    "draws the edge `{from} --> {to}`, which policy::LAYERS does not permit; \
                     the layering gate would fail the workspace this diagram describes"
                ),
            ));
        }
    }

    violations
}

/// Every `a --> b` edge in `body` whose head or tail is a layer.
///
/// Only solid arrows: the size probe's dashed `-.->` edges are drawn for the reader and are
/// not part of the contract, and `-.->` does not contain `-->`.
///
/// Inline node shapes are stripped first. `waymaker-core[Kernel] --> waymaker-embassy[Facade]`
/// is valid Mermaid drawing exactly the same arrow, so a parser that skipped any line with a
/// bracket in it would ignore a forbidden edge written that way — and a label containing the
/// text `-->` would be read as one.
fn drawn_edges(body: &str) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for line in body.lines() {
        let bare = strip_node_shapes(line);
        // `a --> b --> c` is two edges, so consecutive segments are paired rather than the
        // line being split once.
        let nodes: Vec<&str> = bare.split("-->").map(str::trim).collect();
        for pair in nodes.windows(2) {
            let (Some(from), Some(to)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let from = from.trim_end_matches(';').trim().to_owned();
            let to = to.trim_end_matches(';').trim().to_owned();
            if crate::policy::layer(&from).is_some() || crate::policy::layer(&to).is_some() {
                edges.push((from, to));
            }
        }
    }
    edges
}

/// `line` with every Mermaid node label and edge label removed.
///
/// `[...]`, `(...)`, `{...}` and `|...|` all carry text that is not structure. Dropping them
/// leaves the node ids and the arrows between them.
fn strip_node_shapes(line: &str) -> String {
    let mut kept = String::new();
    let mut depth: u32 = 0;
    let mut in_pipe = false;
    for character in line.chars() {
        match character {
            '[' | '(' | '{' => depth = depth.saturating_add(1),
            ']' | ')' | '}' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => in_pipe = !in_pipe,
            _ if depth == 0 && !in_pipe => kept.push(character),
            _ => {}
        }
    }
    kept
}

/// Rule: every crate root in the workspace warns on an undocumented public item.
///
/// Issue #11: "Doc comments required on all public APIs (`#[warn(missing_docs)]` in each
/// crate)." The attribute is one line, and deleting it is invisible: nothing fails, the
/// warnings simply stop. So is silencing it again, which has more spellings than the one
/// everybody thinks of — `expect` instead of `allow`, the `warnings` group instead of the
/// lint, a `cfg_attr` wrapper, three lines instead of one. Every spelling is the same rule
/// here, because they are the same regression.
#[must_use]
fn check_missing_docs(roots: &[CrateRoot]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for root in roots {
        let attributes = inner_attributes(&root.contents);
        if !enables_lint(&attributes, MISSING_DOCS_LINT) {
            violations.push(Violation::new(
                "missing-docs",
                root.package.clone(),
                format!(
                    "{} does not warn, deny or forbid `{MISSING_DOCS_LINT}`; without it an \
                     undocumented public item costs nothing",
                    root.path
                ),
            ));
        } else if silences_lint(&attributes, MISSING_DOCS_LINT) {
            violations.push(Violation::new(
                "missing-docs",
                root.package.clone(),
                format!(
                    "{} turns `{MISSING_DOCS_LINT}` back off again, which leaves the warning \
                     in the file and not in the build",
                    root.path
                ),
            ));
        }
    }

    violations
}

/// Runs every documentation rule.
///
/// The individual rules are private, as in [`crate::size`]: seven of them is more surface
/// than any caller needs, and a rule reachable from outside is a rule a test can assert
/// about without the wiring in [`crate::check_inputs`] being what runs it.
#[must_use]
pub fn check_documentation(inputs: &DocsInputs, rules: &[&str]) -> Vec<Violation> {
    let mut violations = check_claude_md(inputs.claude_md.as_deref(), rules);
    violations.extend(check_adr_numbering(&inputs.adrs));
    violations.extend(check_adr_structure(&inputs.adrs));
    violations.extend(check_adr_index(inputs.adr_index.as_deref(), &inputs.adrs));
    violations.extend(check_settled_decisions(&inputs.adrs));
    violations.extend(check_deferred_questions(
        inputs.claude_md.as_deref(),
        &inputs.adrs,
    ));
    violations.extend(check_recovery_spec(
        inputs.claude_md.as_deref(),
        &inputs.adrs,
        inputs.spec_obligations.as_deref(),
    ));
    violations.extend(check_diagrams(inputs.architecture.as_deref()));
    violations.extend(check_missing_docs(&inputs.crate_roots));
    violations
}

/// Fixtures describing documentation that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use std::fmt::Write as _;

    use super::{
        AdrFile, CRATE_DEPENDENCY_DIAGRAM, CrateRoot, DEFERRED_QUESTION_MARKER, DEFERRED_QUESTIONS,
        DIAGRAMS, DocsInputs, QuestionStatus, RECOVERY_SPEC_ADR, SETTLED_DECISIONS,
        SETTLED_DECISIONS_ADR, SPEC_CLAUSES, adr_number,
    };
    use crate::policy::LAYERS;

    /// Appends `line` and a newline, discarding the `fmt::Result` a `String` never returns.
    fn line(body: &mut String, arguments: std::fmt::Arguments<'_>) {
        let _ = writeln!(body, "{arguments}");
    }

    /// An ADR body that satisfies every structural rule.
    #[must_use]
    pub fn clean_adr(title: &str) -> String {
        format!(
            "# ADR: {title}\n\n- Status: accepted\n- Date: 2026-09-01\n\n## Context\n\nx\n\n\
             ## Decision\n\nx\n\n## Consequences\n\nx\n"
        )
    }

    /// The settled-decisions ADR, with every decision recorded.
    #[must_use]
    pub fn clean_settled_decisions_adr() -> String {
        let mut body = clean_adr("the eight settled design decisions");
        for decision in SETTLED_DECISIONS {
            line(
                &mut body,
                format_args!("\n### {} ({})", decision.headline, decision.id),
            );
        }
        body
    }

    /// An architecture document carrying every required diagram.
    ///
    /// Rendered from [`DIAGRAMS`] and [`LAYERS`] rather than written out, so a diagram
    /// added to the table cannot be left out of the fixture that is supposed to satisfy it.
    #[must_use]
    pub fn clean_architecture() -> String {
        let mut body = String::from("# Architecture\n");

        line(
            &mut body,
            format_args!("\n<!-- diagram: {CRATE_DEPENDENCY_DIAGRAM} -->\n"),
        );
        line(&mut body, format_args!("```mermaid\ngraph LR"));
        for spec in LAYERS {
            line(&mut body, format_args!("  {}[{}]", spec.name, spec.name));
            for dependency in spec.may_depend_on {
                line(&mut body, format_args!("  {} --> {dependency}", spec.name));
            }
        }
        line(&mut body, format_args!("```\n"));

        for spec in DIAGRAMS {
            if spec.id == CRATE_DEPENDENCY_DIAGRAM {
                continue;
            }
            line(&mut body, format_args!("<!-- diagram: {} -->\n", spec.id));
            line(&mut body, format_args!("```mermaid\nflowchart TD"));
            for (index, label) in spec.required_labels.iter().enumerate() {
                line(&mut body, format_args!("  s{index}[\"{label}\"]"));
            }
            line(&mut body, format_args!("```\n"));
        }

        body
    }

    /// A `CLAUDE.md` that satisfies every rule, for the given gate rule ids.
    #[must_use]
    pub fn clean_claude_md(rules: &[&str]) -> String {
        let mut body = String::from("# CLAUDE.md\n\nSee docs/adr and docs/architecture.md.\n\n");
        for spec in LAYERS {
            line(
                &mut body,
                format_args!(
                    "| {} | {} | {} |",
                    spec.name,
                    spec.must_not_own,
                    spec.render_allowed()
                ),
            );
        }
        for stage in crate::pipeline::STAGES {
            line(&mut body, format_args!("    {}", stage.command));
        }
        for decision in SETTLED_DECISIONS {
            line(&mut body, format_args!("- {}", decision.id));
        }
        for rule in rules {
            line(&mut body, format_args!("- `{rule}`"));
        }
        line(&mut body, format_args!("All {} rules.", rules.len()));
        line(
            &mut body,
            format_args!("| Id | Question | Where it stands |"),
        );
        line(&mut body, format_args!("| --- | --- | --- |"));
        for question in DEFERRED_QUESTIONS {
            line(
                &mut body,
                format_args!(
                    "| `{}` | {} | {} |",
                    question.id,
                    question.headline,
                    question.render_status()
                ),
            );
        }
        line(
            &mut body,
            format_args!("All {} deferred questions.", DEFERRED_QUESTIONS.len()),
        );
        for clause in SPEC_CLAUSES {
            line(
                &mut body,
                format_args!(
                    "| `{}` | {} | {} |",
                    clause.id, clause.headline, clause.discharged_by
                ),
            );
        }
        line(
            &mut body,
            format_args!("All {} recovery invariants.", SPEC_CLAUSES.len()),
        );
        body
    }

    /// A clause table that declares exactly the clauses the gate expects.
    #[must_use]
    pub fn clean_spec_obligations() -> String {
        let mut body = String::from("//! The clause table.\npub const CLAUSES: &[Clause] = &[\n");
        for clause in SPEC_CLAUSES {
            line(
                &mut body,
                format_args!(
                    "    Clause {{ id: \"{}\", proof: \"{}\" }},",
                    clause.id, clause.discharged_by
                ),
            );
        }
        body.push_str("];\n");
        body
    }

    /// The ADR that records the recovery specification, naming every clause.
    #[must_use]
    pub fn clean_recovery_spec_adr() -> String {
        let mut body = clean_adr("the recovery invariants");
        for clause in SPEC_CLAUSES {
            line(&mut body, format_args!("- `{}`", clause.id));
        }
        body
    }

    /// The ADRs a clean record holds: one per number from 1 to the highest the tables name.
    ///
    /// Rendered rather than written out, because `adr-numbering` refuses a gap and
    /// `DEFERRED_QUESTIONS` names ADRs by number. A fixture with a hand-written list would
    /// have to be renumbered by hand every time a question is settled, which is exactly the
    /// drift these rules exist to catch.
    ///
    /// # Panics
    ///
    /// If a table names an ADR that is not a numbered `NNNN-slug.md` file. That is the gate
    /// misdescribing its own record, and a fixture that quietly worked around it would hide
    /// the one thing worth knowing.
    #[must_use]
    pub fn clean_adrs() -> Vec<AdrFile> {
        let mut bodies: std::collections::BTreeMap<u32, (String, String)> =
            std::collections::BTreeMap::new();

        let settled_number =
            adr_number(SETTLED_DECISIONS_ADR).expect("the settled-decisions ADR is numbered");
        bodies.insert(
            settled_number,
            (
                SETTLED_DECISIONS_ADR.to_owned(),
                clean_settled_decisions_adr(),
            ),
        );

        for question in DEFERRED_QUESTIONS {
            let QuestionStatus::Settled { adr } = question.status else {
                continue;
            };
            let number = adr_number(adr).expect("a settled question names a numbered ADR");
            let body = format!(
                "{}\n{DEFERRED_QUESTION_MARKER} `{}`\n",
                clean_adr(question.id),
                question.id
            );
            bodies.insert(number, (adr.to_owned(), body));
        }

        let spec_number =
            adr_number(RECOVERY_SPEC_ADR).expect("the recovery specification ADR is numbered");
        bodies.insert(
            spec_number,
            (RECOVERY_SPEC_ADR.to_owned(), clean_recovery_spec_adr()),
        );

        let highest = bodies.keys().copied().max().unwrap_or(0);
        for number in super::ADR_NUMBER_START..=highest {
            bodies
                .entry(number)
                .or_insert_with(|| (format!("{number:04}-filler.md"), clean_adr("filler")));
        }

        let mut adrs = vec![AdrFile {
            name: super::ADR_TEMPLATE.to_owned(),
            contents: "# ADR NNNN: title\n\n- Status: proposed | accepted\n\
                       - Date: YYYY-MM-DD\n\n## Context\n\n## Decision\n\n\
                       ## Consequences\n"
                .to_owned(),
        }];
        adrs.extend(
            bodies
                .into_values()
                .map(|(name, contents)| AdrFile { name, contents }),
        );
        adrs
    }

    /// Documentation inputs that every rule passes.
    #[must_use]
    pub fn clean_inputs(rules: &[&str]) -> DocsInputs {
        DocsInputs {
            claude_md: Some(clean_claude_md(rules)),
            architecture: Some(clean_architecture()),
            adr_index: Some({
                let mut index = String::from("# Decisions\n\n");
                for adr in clean_adrs() {
                    line(&mut index, format_args!("- [{0}]({0})", adr.name));
                }
                index
            }),
            adrs: clean_adrs(),
            spec_obligations: Some(clean_spec_obligations()),
            // One root per crate the layering covers, so that a fixture describing a clean
            // workspace really has one for every member `inputs-incomplete` looks for.
            crate_roots: crate::policy::checked_members()
                .map(|name| CrateRoot {
                    package: name.to_owned(),
                    path: format!("crates/{name}/src/lib.rs"),
                    contents: "//! Docs.\n#![no_std]\n#![warn(missing_docs)]\n".to_owned(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::tests_support::{clean_adr, clean_architecture, clean_claude_md, clean_inputs};
    use super::*;

    const RULES: &[&str] = &["claude-md", "diagrams"];

    fn rule_ids(violations: &[Violation]) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = violations.iter().map(|v| v.rule).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[test]
    fn clean_documentation_passes_every_rule() {
        let violations = check_documentation(&clean_inputs(RULES), RULES);
        assert!(violations.is_empty(), "{violations:?}");
    }

    // Issue #16: the five deferred questions of design document §16.

    /// A settled question's id, or `None` if every question in the table is open.
    fn a_settled_question() -> Option<&'static DeferredQuestion> {
        DEFERRED_QUESTIONS
            .iter()
            .find(|question| matches!(question.status, QuestionStatus::Settled { .. }))
    }

    /// An open question, or `None` if every question in the table has been settled.
    fn an_open_question() -> Option<&'static DeferredQuestion> {
        DEFERRED_QUESTIONS
            .iter()
            .find(|question| matches!(question.status, QuestionStatus::Open { .. }))
    }

    #[test]
    fn the_deferred_question_table_has_the_five_questions_section_sixteen_leaves_open() {
        // §16 lists five. A sixth added to the design document without a row here is the
        // drift this table exists to make impossible; a row deleted is a question that
        // stopped being tracked.
        assert_eq!(DEFERRED_QUESTIONS.len(), 5);

        let mut ids: Vec<&str> = DEFERRED_QUESTIONS.iter().map(|q| q.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two questions share an id");

        // Several tests below pick a settled or an open question and return early if there
        // is none. Internal review of PR #58 pointed out where that ends: settle the last
        // three questions — the plan for rungs 0.4 and 1.0 — and those tests go silently
        // green while this one still counts five. Asserted here so that the day the mix
        // changes, the failure is this line rather than a quiet loss of coverage.
        assert!(
            DEFERRED_QUESTIONS
                .iter()
                .any(|question| matches!(question.status, QuestionStatus::Settled { .. })),
            "no settled question, so the settled-side tests test nothing"
        );
        assert!(
            DEFERRED_QUESTIONS
                .iter()
                .any(|question| matches!(question.status, QuestionStatus::Open { .. })),
            "no open question, so the open-side tests test nothing"
        );
    }

    #[test]
    fn clean_documentation_passes_the_deferred_question_rule() {
        let violations = check_deferred_questions(
            clean_claude_md(RULES).as_str().into(),
            &clean_inputs(RULES).adrs,
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_missing_claude_md_leaves_the_deferred_questions_unreported() {
        let violations = check_deferred_questions(None, &clean_inputs(RULES).adrs);
        assert_eq!(rule_ids(&violations), vec!["deferred-questions"]);
        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_claude_md_that_drops_a_question_id_is_reported() {
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let dropped =
            clean_claude_md(RULES).replace(&format!("`{}`", question.id), "(this one, whatever)");
        let violations = check_deferred_questions(Some(&dropped), &clean_inputs(RULES).adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "deferred-questions" && v.subject == question.id),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_that_drops_a_question_headline_is_reported() {
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let dropped = clean_claude_md(RULES).replace(question.headline, "something else");
        let violations = check_deferred_questions(Some(&dropped), &clean_inputs(RULES).adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("headline")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_row_that_contradicts_the_table_is_reported() {
        // Codex review, PR #58. The first version searched the whole file for the id and
        // the headline, so a row could say a settled question was open, name the wrong
        // owner, or point at the wrong ADR, and the rule would still pass — which is the
        // exact drift this rule exists to catch, in the file it exists to check.
        for question in DEFERRED_QUESTIONS {
            let clean = clean_claude_md(RULES);
            let broken = clean.replace(&question.render_status(), "whatever it likes");
            assert_ne!(
                broken, clean,
                "the fixture should state every question's status"
            );
            let violations = check_deferred_questions(Some(&broken), &clean_inputs(RULES).adrs);
            assert!(
                violations
                    .iter()
                    .any(|v| v.subject == question.id && v.detail.contains("where it stands")),
                "`{}` was not held to its status: {violations:?}",
                question.id
            );
        }
    }

    #[test]
    fn a_question_stated_across_two_rows_is_reported() {
        // The id on one line and the headline on another is two half-rows, not a row. A
        // reader following the table gets nothing from it, and a global search would have
        // called it fine.
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let split = clean_claude_md(RULES).replace(
            &format!("| `{}` | {} |", question.id, question.headline),
            &format!("| `{}` | |\n| {} |", question.id, question.headline),
        );
        let violations = check_deferred_questions(Some(&split), &clean_inputs(RULES).adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("headline")),
            "{violations:?}"
        );
    }

    #[test]
    fn every_question_renders_a_status_a_reader_can_act_on() {
        // A settled question's rendering has to name the ADR, because "settled" without a
        // record is the thing this whole rule is against.
        for question in DEFERRED_QUESTIONS {
            let rendered = question.render_status();
            match question.status {
                QuestionStatus::Settled { adr } => {
                    assert!(rendered.contains(adr), "{rendered}");
                }
                QuestionStatus::Open { owned_by, .. } => {
                    assert!(rendered.contains(owned_by), "{rendered}");
                }
            }
        }
    }

    #[test]
    fn a_question_documented_only_inside_an_html_comment_is_reported() {
        // Same reason every rule in this module strips comments, and nothing pinned it
        // here: a block of ids a reader never sees is not documentation.
        let hidden = format!("<!--\n{}\n-->\n", clean_claude_md(RULES));
        let violations = check_deferred_questions(Some(&hidden), &clean_inputs(RULES).adrs);
        assert!(
            violations.iter().any(|v| v.subject == "question count"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unbackticked_question_id_does_not_count() {
        // The backticks are deliberate — they stop a longer id being vouched for by a
        // shorter one — and the drop-the-id test could not tell the two rules apart,
        // because it deleted the backticked form altogether.
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let bare = clean_claude_md(RULES).replace(&format!("`{}`", question.id), question.id);
        let violations = check_deferred_questions(Some(&bare), &clean_inputs(RULES).adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("backticks")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_that_states_the_wrong_number_of_questions_is_reported() {
        let wrong = clean_claude_md(RULES).replace(
            &format!("{} deferred question", DEFERRED_QUESTIONS.len()),
            "two deferred question",
        );
        let violations = check_deferred_questions(Some(&wrong), &clean_inputs(RULES).adrs);
        assert!(
            violations.iter().any(|v| v.subject == "question count"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_settled_question_whose_adr_is_missing_is_reported() {
        let Some(question) = a_settled_question() else {
            return;
        };
        let QuestionStatus::Settled { adr } = question.status else {
            unreachable!("filtered above");
        };
        let adrs: Vec<AdrFile> = clean_inputs(RULES)
            .adrs
            .into_iter()
            .filter(|file| file.name != adr)
            .collect();
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains(adr)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_settled_question_whose_adr_does_not_claim_it_is_reported() {
        // The half that matters: an ADR can be *about* a question without saying so, and a
        // rule that only checked the file existed would be satisfied by any ADR at all.
        let Some(question) = a_settled_question() else {
            return;
        };
        let QuestionStatus::Settled { adr } = question.status else {
            unreachable!("filtered above");
        };
        let adrs: Vec<AdrFile> = clean_inputs(RULES)
            .adrs
            .into_iter()
            .map(|mut file| {
                if file.name == adr {
                    file.contents = clean_adr("an adr that claims nothing");
                }
                file
            })
            .collect();
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains(DEFERRED_QUESTION_MARKER)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_settled_question_whose_adr_is_not_accepted_is_reported() {
        // A `proposed` ADR is a draft. A question answered by a draft is not answered.
        let Some(question) = a_settled_question() else {
            return;
        };
        let QuestionStatus::Settled { adr } = question.status else {
            unreachable!("filtered above");
        };
        let adrs: Vec<AdrFile> = clean_inputs(RULES)
            .adrs
            .into_iter()
            .map(|mut file| {
                if file.name == adr {
                    file.contents = file
                        .contents
                        .replace("- Status: accepted", "- Status: proposed");
                }
                file
            })
            .collect();
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("proposed")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_open_question_an_adr_has_already_answered_is_reported() {
        // The table going stale in the other direction: somebody wrote the ADR and left
        // the row saying nobody had.
        let Some(question) = an_open_question() else {
            return;
        };
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-premature.md".to_owned(),
            contents: format!(
                "{}\n{DEFERRED_QUESTION_MARKER} {}\n",
                clean_adr("premature"),
                question.id
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("0099-premature.md")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_claiming_a_question_the_table_does_not_declare_is_reported() {
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-invented.md".to_owned(),
            contents: format!(
                "{}\n{DEFERRED_QUESTION_MARKER} `a-question-nobody-asked`\n",
                clean_adr("invented")
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "0099-invented.md"
                    && v.detail.contains("a-question-nobody-asked")),
            "{violations:?}"
        );
    }

    #[test]
    fn two_adrs_claiming_one_question_are_reported() {
        // Which one settled it becomes a matter of file order, which is how a decision
        // ends up with two records that can disagree.
        let Some(question) = a_settled_question() else {
            return;
        };
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-second-claim.md".to_owned(),
            contents: format!(
                "{}\n{DEFERRED_QUESTION_MARKER} {}\n",
                clean_adr("second claim"),
                question.id
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains("0099-second-claim.md")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_marker_inside_an_html_comment_does_not_settle_anything() {
        // Same reason every other rule here strips comments: a claim a reader cannot see
        // is not a record.
        let Some(question) = an_open_question() else {
            return;
        };
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-hidden.md".to_owned(),
            // The marker on a line of its own inside a multi-line comment. Written on one
            // line, the id parsed as `retry-policy-placement -->`, matched no row, and was
            // reported under a different subject — so the assertion below held even with
            // the comment-stripping deleted, which is the one thing this test exists to
            // catch. Internal review of PR #58.
            contents: format!(
                "{}\n<!--\n{DEFERRED_QUESTION_MARKER} {}\n-->\n",
                clean_adr("hidden"),
                question.id
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations.is_empty(),
            "a marker a reader cannot see settled something: {violations:?}"
        );
    }

    #[test]
    fn a_marker_inside_a_fenced_example_does_not_settle_anything() {
        // Codex, PR #58. An ADR explaining how the marker works must not be read as using
        // it — and the direction that matters more is the other one: an ADR that kept the
        // example while losing its real marker would still have passed the settled check.
        let Some(question) = an_open_question() else {
            return;
        };
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-explains-the-syntax.md".to_owned(),
            contents: format!(
                "{}\n```text\n{DEFERRED_QUESTION_MARKER} `{}`\n```\n",
                clean_adr("explains the syntax"),
                question.id
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations.is_empty(),
            "an example was read as a claim: {violations:?}"
        );
    }

    #[test]
    fn a_settled_question_whose_marker_is_only_an_example_is_reported() {
        let Some(question) = a_settled_question() else {
            return;
        };
        let QuestionStatus::Settled { adr } = question.status else {
            unreachable!("filtered above");
        };
        let adrs: Vec<AdrFile> = clean_inputs(RULES)
            .adrs
            .into_iter()
            .map(|mut file| {
                if file.name == adr {
                    file.contents = format!(
                        "{}\n```text\n{DEFERRED_QUESTION_MARKER} `{}`\n```\n",
                        clean_adr(question.id),
                        question.id
                    );
                }
                file
            })
            .collect();
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == question.id && v.detail.contains(DEFERRED_QUESTION_MARKER)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_marker_inside_a_tilde_fenced_example_does_not_settle_anything() {
        // Codex, PR #58 round 3. Markdown fences with `~~~` as well as backticks, and the
        // scan only knew one of them — so the fix for the fenced-example hole had a hole
        // of exactly the same shape.
        let Some(question) = an_open_question() else {
            return;
        };
        let mut adrs = clean_inputs(RULES).adrs;
        adrs.push(AdrFile {
            name: "0099-tilde-fenced.md".to_owned(),
            contents: format!(
                "{}\n~~~text\n{DEFERRED_QUESTION_MARKER} `{}`\n~~~\n",
                clean_adr("tilde fenced"),
                question.id
            ),
        });
        let violations = check_deferred_questions(clean_claude_md(RULES).as_str().into(), &adrs);
        assert!(
            violations.is_empty(),
            "a tilde-fenced example was read as a claim: {violations:?}"
        );
    }

    #[test]
    fn a_fence_is_closed_only_by_its_own_character() {
        // The other half of knowing about two fence characters: a backtick block must not
        // be closed by a tilde line, or everything after it stops being fenced.
        assert_eq!(
            without_fenced_code("keep\n```\nhidden\n~~~\nstill hidden\n```\nkeep too"),
            "keep\nkeep too"
        );
    }

    #[test]
    fn prose_naming_a_question_before_the_table_does_not_stand_in_for_its_row() {
        // Codex, PR #58 round 3. The scan took the first line containing the backticked id,
        // so a mention in prose above the table shadowed the row — failing the gate on a
        // correct file, and passing it on one whose row had gone stale.
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let with_prose = format!(
            "Some prose mentioning `{}` well before the table.\n\n{}",
            question.id,
            clean_claude_md(RULES)
        );
        assert!(
            check_deferred_questions(Some(&with_prose), &clean_inputs(RULES).adrs).is_empty(),
            "prose above the table shadowed the row"
        );
    }

    #[test]
    fn a_question_whose_row_is_gone_is_reported_even_when_prose_names_it() {
        // The direction that matters: prose that happens to repeat the headline and the
        // status must not stand in for the row itself.
        let Some(question) = DEFERRED_QUESTIONS.first() else {
            return;
        };
        let row = format!(
            "| `{}` | {} | {} |",
            question.id,
            question.headline,
            question.render_status()
        );
        let prose_only = clean_claude_md(RULES).replace(
            &row,
            &format!(
                "`{}` {} {} — stated in prose, in no table at all",
                question.id,
                question.headline,
                question.render_status()
            ),
        );
        let violations = check_deferred_questions(Some(&prose_only), &clean_inputs(RULES).adrs);
        assert!(
            violations.iter().any(|v| v.subject == question.id),
            "{violations:?}"
        );
    }

    #[test]
    fn check_documentation_runs_the_deferred_question_rule() {
        let mut inputs = clean_inputs(RULES);
        inputs.claude_md = None;
        let violations = check_documentation(&inputs, RULES);
        assert!(
            violations.iter().any(|v| v.rule == "deferred-questions"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_claude_md_is_reported_once() {
        let violations = check_claude_md(None, RULES);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "claude-md");
    }

    #[test]
    fn a_must_not_own_row_that_drifts_from_the_table_is_reported() {
        let kernel = crate::policy::layer("waymaker-core").expect("the kernel is a layer");
        let drifted = clean_claude_md(RULES).replace(kernel.must_not_own, "whatever it likes");
        let violations = check_claude_md(Some(&drifted), RULES);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "waymaker-core" && v.detail.contains("policy::LAYERS")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_that_does_not_name_a_gate_rule_is_reported() {
        // A CLAUDE.md written for no rules at all: one violation per rule it fails to
        // name, plus the count it now states wrongly.
        let violations = check_claude_md(Some(&clean_claude_md(&[])), RULES);
        assert_eq!(violations.len(), RULES.len() + 1, "{violations:?}");
        assert!(violations.iter().any(|v| v.subject == "rule count"));
    }

    #[test]
    fn one_rule_id_does_not_vouch_for_another_it_is_a_prefix_of() {
        // `size-probe` is a prefix of `size-probe-reach`, so a bare substring match lets
        // deleting one row pass because the next row contains it.
        let rules = ["size-probe", "size-probe-reach"];
        let without = clean_claude_md(&rules).replace("- `size-probe`\n", "");
        let violations = check_claude_md(Some(&without), &rules);
        assert!(
            violations.iter().any(|v| v.subject == "size-probe"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_whose_anchors_are_all_in_an_html_comment_is_reported() {
        let hidden = format!(
            "# CLAUDE.md\n\nTODO: rewrite.\n\n<!--\n{}\n-->\n",
            clean_claude_md(RULES)
        );
        let violations = check_claude_md(Some(&hidden), RULES);
        assert!(
            !violations.is_empty(),
            "a file that says nothing must not pass"
        );
    }

    #[test]
    fn a_claude_md_that_does_not_say_what_a_layer_may_depend_on_is_reported() {
        let facade = crate::policy::layer("waymaker-embassy").expect("the façade is a layer");
        let without = clean_claude_md(RULES).replace(&facade.render_allowed(), "anything");
        let violations = check_claude_md(Some(&without), RULES);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "waymaker-embassy" && v.detail.contains("may depend on")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_whose_rule_count_is_wrong_is_reported() {
        let wrong = clean_claude_md(RULES).replace(
            &format!("{} rules", RULES.len()),
            &format!("{} rules", RULES.len() + 1),
        );
        let violations = check_claude_md(Some(&wrong), RULES);
        assert!(
            violations.iter().any(|v| v.subject == "rule count"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_that_omits_a_pipeline_command_is_reported() {
        let stage = crate::pipeline::STAGES
            .first()
            .expect("the pipeline has stages");
        let without = clean_claude_md(RULES).replace(stage.command, "");
        let violations = check_claude_md(Some(&without), RULES);
        assert!(
            violations.iter().any(|v| v.subject == stage.name),
            "{violations:?}"
        );
    }

    #[test]
    fn a_claude_md_that_does_not_point_at_the_decision_record_is_reported() {
        let without = clean_claude_md(RULES).replace("docs/adr", "somewhere");
        let violations = check_claude_md(Some(&without), RULES);
        assert!(
            violations.iter().any(|v| v.subject == ADR_DIR),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_number_is_four_digits_and_a_dash() {
        assert_eq!(adr_number("0003-a-decision.md"), Some(3));
        assert_eq!(adr_number("0000-template.md"), Some(0));
        assert_eq!(adr_number("README.md"), None);
        assert_eq!(adr_number("003-short.md"), None);
        assert_eq!(adr_number("00003-long.md"), None);
        assert_eq!(adr_number("0003_underscore.md"), None);
        assert_eq!(adr_number("0003-not-markdown.txt"), None);
        assert_eq!(adr_number("abcd-letters.md"), None);
        assert_eq!(adr_number("0003"), None);
    }

    #[test]
    fn a_gap_in_the_numbering_is_reported() {
        let adrs = vec![
            AdrFile {
                name: "0000-template.md".to_owned(),
                contents: String::new(),
            },
            AdrFile {
                name: "0001-one.md".to_owned(),
                contents: String::new(),
            },
            AdrFile {
                name: "0003-three.md".to_owned(),
                contents: String::new(),
            },
        ];
        let violations = check_adr_numbering(&adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "0003" && v.detail.contains("0002")),
            "{violations:?}"
        );
    }

    #[test]
    fn two_adrs_claiming_one_number_are_reported() {
        let adrs = vec![
            AdrFile {
                name: "0000-template.md".to_owned(),
                contents: String::new(),
            },
            AdrFile {
                name: "0001-one.md".to_owned(),
                contents: String::new(),
            },
            AdrFile {
                name: "0001-also-one.md".to_owned(),
                contents: String::new(),
            },
        ];
        let violations = check_adr_numbering(&adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("more than one ADR")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_record_with_no_template_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_numbering(&adrs);
        assert!(
            violations.iter().any(|v| v.subject == ADR_TEMPLATE),
            "{violations:?}"
        );
    }

    #[test]
    fn a_file_that_is_not_named_like_an_adr_is_reported() {
        let adrs = vec![AdrFile {
            name: "notes.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_numbering(&adrs);
        assert!(
            violations.iter().any(|v| v.subject == "notes.md"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_missing_a_required_section_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: clean_adr("one").replace("## Consequences", "## Aftermath"),
        }];
        let violations = check_adr_structure(&adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("## Consequences")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_with_no_status_line_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: clean_adr("one").replace("- Status: accepted\n", ""),
        }];
        let violations = check_adr_structure(&adrs);
        assert!(
            violations.iter().any(|v| v.detail.contains("- Status:")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_with_an_unrecognised_status_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: clean_adr("one").replace("accepted", "mostly"),
        }];
        let violations = check_adr_structure(&adrs);
        assert!(
            violations.iter().any(|v| v.detail.contains("`mostly`")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_templates_placeholder_status_is_not_a_violation() {
        let adrs = vec![AdrFile {
            name: "0000-template.md".to_owned(),
            contents: clean_adr("template").replace("accepted", "proposed | accepted"),
        }];
        assert!(check_adr_structure(&adrs).is_empty());
    }

    #[test]
    fn an_adr_with_no_title_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: clean_adr("one").replace("# ADR: one", "ADR: one"),
        }];
        let violations = check_adr_structure(&adrs);
        assert!(
            violations.iter().any(|v| v.detail.contains("title")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_the_index_does_not_link_is_reported() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_index(Some("# Decisions\n"), &adrs);
        assert!(
            violations.iter().any(|v| v.subject == "0001-one.md"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_index_link_to_an_adr_that_does_not_exist_is_reported() {
        let violations = check_adr_index(Some("- [ghost](0007-ghost.md)\n"), &[]);
        assert!(
            violations.iter().any(|v| v.subject == "0007-ghost.md"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_adr_mentioned_but_not_linked_is_reported() {
        // `[0001-one.md](../architecture.md)` names the ADR and links somewhere else.
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_index(Some("- [0001-one.md](../architecture.md)\n"), &adrs);
        assert!(
            violations.iter().any(|v| v.subject == "0001-one.md"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_dead_link_is_found_whatever_syntax_it_uses() {
        for index in [
            "- [ghost](0007-ghost.md)\n",
            "- [ghost](0007-ghost.md \"ADR 7\")\n",
            "- [ghost](<0007-ghost.md>)\n",
            "[ghost]: 0007-ghost.md\n",
        ] {
            let violations = check_adr_index(Some(index), &[]);
            assert!(
                violations.iter().any(|v| v.subject == "0007-ghost.md"),
                "{index} was not caught: {violations:?}"
            );
        }
    }

    #[test]
    fn a_template_renamed_to_another_zero_is_reported() {
        // Any `0000-*.md` satisfies "something is numbered zero" while the path the index
        // and CLAUDE.md link to no longer exists.
        let adrs = vec![AdrFile {
            name: "0000-the-shape-of-an-adr.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_numbering(&adrs);
        assert!(
            violations.iter().any(|v| v.subject == ADR_TEMPLATE),
            "{violations:?}"
        );
    }

    #[test]
    fn adr_metadata_hidden_in_a_comment_does_not_satisfy_the_structure_rule() {
        // The rendered decision would carry no status and no date at all.
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: clean_adr("one")
                .replace("- Status: accepted", "<!-- - Status: accepted")
                .replace("- Date: 2026-09-01", "- Date: 2026-09-01 -->"),
        }];
        let violations = check_adr_structure(&adrs);
        assert!(
            violations.iter().any(|v| v.detail.contains("- Status:"))
                && violations.iter().any(|v| v.detail.contains("- Date:")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_link_a_reader_cannot_follow_does_not_index_an_adr() {
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: String::new(),
        }];
        for index in [
            "<!-- - [one](0001-one.md) -->\n",
            "```\n- [one](0001-one.md)\n```\n",
        ] {
            let violations = check_adr_index(Some(index), &adrs);
            assert!(
                violations.iter().any(|v| v.subject == "0001-one.md"),
                "{index} was accepted as a link: {violations:?}"
            );
        }
    }

    #[test]
    fn a_dead_link_shown_only_as_an_example_is_not_a_dead_link() {
        // The other direction of the same rule: a link inside a fenced example is text
        // about a link, so it cannot be broken.
        let index = "How to link one:\n\n```\n- [ghost](0007-ghost.md)\n```\n";
        assert!(check_adr_index(Some(index), &[]).is_empty());
    }

    #[test]
    fn a_missing_index_is_reported() {
        let violations = check_adr_index(None, &[]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "adr-index");
    }

    #[test]
    fn a_link_into_a_directory_that_does_not_exist_is_a_dead_link() {
        // `missing/0001-one.md` is not `0001-one.md`: comparing basenames would call a
        // broken link an index entry, and the real ADR unindexed.
        let adrs = vec![AdrFile {
            name: "0001-one.md".to_owned(),
            contents: String::new(),
        }];
        let violations = check_adr_index(Some("- [one](missing/0001-one.md)\n"), &adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "0001-one.md" && v.detail.contains("no link")),
            "{violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "missing/0001-one.md"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_link_to_something_other_than_an_adr_is_not_reported() {
        // The index links the design document and the architecture page too; only the
        // links that look like ADR file names are the index's own record.
        let violations = check_adr_index(
            Some("- [design](../design/waymaker-design-v0.2.html)\n- [arch](../architecture.md)\n"),
            &[],
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_settled_decision_missing_from_the_adr_is_reported() {
        let mut inputs = clean_inputs(RULES);
        let first = SETTLED_DECISIONS[0];
        for adr in &mut inputs.adrs {
            if adr.name == SETTLED_DECISIONS_ADR {
                adr.contents = adr.contents.replace(first.id, "something-else");
            }
        }
        let violations = check_settled_decisions(&inputs.adrs);
        assert!(
            violations.iter().any(|v| v.subject == first.id),
            "{violations:?}"
        );
    }

    #[test]
    fn a_settled_decision_recorded_without_its_headline_is_reported() {
        let mut inputs = clean_inputs(RULES);
        let first = SETTLED_DECISIONS[0];
        for adr in &mut inputs.adrs {
            if adr.name == SETTLED_DECISIONS_ADR {
                adr.contents = adr.contents.replace(first.headline, "reworded beyond use");
            }
        }
        let violations = check_settled_decisions(&inputs.adrs);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == first.id && v.detail.contains("headline")),
            "{violations:?}"
        );
    }

    #[test]
    fn ids_hidden_in_an_html_comment_do_not_record_a_decision() {
        // Otherwise an ADR reduced to a title and a block of ids in a comment passes.
        let mut ids = String::new();
        for decision in SETTLED_DECISIONS {
            use std::fmt::Write as _;
            let _ = writeln!(ids, "{} {}", decision.id, decision.headline);
        }
        let adrs = vec![AdrFile {
            name: SETTLED_DECISIONS_ADR.to_owned(),
            contents: format!("# ADR 0003: nothing here\n\n<!--\n{ids}-->\n"),
        }];
        let violations = check_settled_decisions(&adrs);
        assert_eq!(
            violations.len(),
            SETTLED_DECISIONS.len() * 2,
            "{violations:?}"
        );
    }

    #[test]
    fn a_record_with_no_settled_decisions_adr_is_reported() {
        let violations = check_settled_decisions(&[]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].subject, SETTLED_DECISIONS_ADR);
    }

    #[test]
    fn the_settled_decision_ids_are_unique() {
        let mut ids: Vec<&str> = SETTLED_DECISIONS.iter().map(|d| d.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn design_document_section_02_settles_eight_decisions() {
        assert_eq!(SETTLED_DECISIONS.len(), 8);
    }

    #[test]
    fn each_step_list_has_the_number_of_steps_its_design_document_section_states() {
        assert_eq!(EFFECT_PROTOCOL_STEPS.len(), 7);
        assert_eq!(TWO_BANK_SWAP_STEPS.len(), 7);
        assert_eq!(COLD_START_STEPS.len(), 6);
        assert_eq!(TRANSITION_TABLE_ROWS.len(), 5);
        // Four fault families from design document §15 and issue #18's injection list,
        // plus the three record states the same issue asks the model to distinguish.
        assert_eq!(CRASH_INJECTION_LABELS.len(), 7);
    }

    #[test]
    fn a_labelled_mermaid_block_is_collected() {
        let blocks = mermaid_blocks(
            "# Title\n\n<!-- diagram: one -->\n\n```mermaid\ngraph LR\n  a --> b\n```\n",
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id, "one");
        assert!(blocks[0].body.contains("a --> b"));
    }

    #[test]
    fn an_unlabelled_mermaid_block_is_not_collected() {
        let blocks = mermaid_blocks("```mermaid\ngraph LR\n```\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn a_label_separated_from_its_fence_by_prose_is_not_collected() {
        // Otherwise a label could drift away from the diagram it names and keep passing.
        let blocks =
            mermaid_blocks("<!-- diagram: one -->\n\nSome prose.\n\n```mermaid\ngraph LR\n```\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn a_non_mermaid_fence_is_not_collected() {
        let blocks = mermaid_blocks("<!-- diagram: one -->\n\n```text\ngraph LR\n```\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn an_unterminated_mermaid_fence_is_not_collected() {
        let blocks = mermaid_blocks("<!-- diagram: one -->\n\n```mermaid\ngraph LR\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn a_diagram_that_lost_a_protocol_step_is_reported() {
        let architecture = clean_architecture().replace(EFFECT_PROTOCOL_STEPS[3], "something else");
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "durable-effect-protocol"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_dependency_diagram_missing_an_edge_is_reported() {
        let architecture =
            clean_architecture().replace("waymaker-flash --> waymaker-core", "flash --> core");
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("waymaker-flash --> waymaker-core")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_step_that_appears_twice_must_be_drawn_twice() {
        // §07's protocol has two payload barriers. A diagram that drops the second still
        // contains the string, and the step it drops is the barrier before the outcome
        // seal — the one that makes a result safe to observe.
        let architecture = clean_architecture().replacen("  s1[\"Payload barrier\"]\n", "", 1);
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("Payload barrier")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_step_written_only_as_a_mermaid_comment_does_not_count() {
        // `%%` is in the source and not in the picture.
        let architecture = clean_architecture().replace(
            "  s3[\"Dispatch physical activity\"]",
            "  %% Dispatch physical activity",
        );
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("Dispatch physical activity")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_edge_the_layering_does_not_permit_is_reported() {
        // The picture a reader trusts to say which way the layering goes must not be able
        // to say the opposite of the table the gate enforces.
        let architecture = clean_architecture().replace(
            "  waymaker-flash --> waymaker-core",
            "  waymaker-flash --> waymaker-core\n  waymaker-core --> waymaker-embassy",
        );
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("waymaker-core --> waymaker-embassy")
                    && v.detail.contains("does not permit")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_edge_written_only_as_a_mermaid_comment_does_not_count() {
        let architecture =
            clean_architecture().replace("  waymaker-flash -->", "  %% waymaker-flash -->");
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("waymaker-flash --> waymaker-core")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_indented_fence_is_a_quoted_block_and_not_a_diagram() {
        // Four spaces make Markdown render the fence as literal text.
        let blocks =
            mermaid_blocks("<!-- diagram: one -->\n\n    ```mermaid\n    graph LR\n    ```\n");
        assert!(blocks.is_empty(), "{blocks:?}");
    }

    #[test]
    fn a_diagram_quoted_inside_a_longer_fence_is_not_a_diagram() {
        let quoted = "````\n<!-- diagram: one -->\n\n```mermaid\ngraph LR\n```\n````\n";
        assert!(mermaid_blocks(quoted).is_empty(), "{quoted}");
    }

    #[test]
    fn two_blocks_with_one_id_are_reported_rather_than_silently_shadowing() {
        let architecture = format!(
            "{}\n<!-- diagram: {CRATE_DEPENDENCY_DIAGRAM} -->\n\n```mermaid\ngraph LR\n```\n",
            clean_architecture()
        );
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("more than one mermaid block")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_edge_written_with_inline_node_shapes_is_still_an_edge() {
        // `a[Label] --> b[Label]` is valid Mermaid and draws the same arrow. A parser that
        // skipped any line with a bracket in it would ignore a forbidden edge written that
        // way, while the rendered picture contradicted the layering.
        let architecture = clean_architecture().replace(
            "  waymaker-flash --> waymaker-core",
            "  waymaker-flash --> waymaker-core\n  waymaker-core[Kernel] --> waymaker-embassy[Facade]",
        );
        let violations = check_diagrams(Some(&architecture));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("waymaker-core --> waymaker-embassy")
                    && v.detail.contains("does not permit")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_permitted_edge_written_with_inline_node_shapes_counts() {
        let architecture = clean_architecture().replace(
            "  waymaker-flash --> waymaker-core",
            "  waymaker-flash[Flash] --> waymaker-core[Core]",
        );
        let violations = check_diagrams(Some(&architecture));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_dashed_edge_is_not_part_of_the_contract() {
        // The size probe's edges are drawn for the reader, not enforced.
        let architecture = format!(
            "{}\n  waymaker-size-probe -.-> waymaker-core\n",
            clean_architecture()
        );
        assert!(check_diagrams(Some(&architecture)).is_empty());
    }

    #[test]
    fn a_missing_architecture_document_is_reported_once() {
        let violations = check_diagrams(None);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "diagrams");
    }

    /// One crate root carrying `contents`, for the `missing-docs` rule.
    fn roots(contents: &str) -> Vec<CrateRoot> {
        vec![CrateRoot {
            package: "waymaker-core".to_owned(),
            path: "crates/waymaker-core/src/lib.rs".to_owned(),
            contents: contents.to_owned(),
        }]
    }

    #[test]
    fn a_crate_root_without_the_missing_docs_attribute_is_reported() {
        let violations = check_missing_docs(&roots("//! Docs.\n#![no_std]\n"));
        assert!(
            violations.iter().any(|v| v.detail.contains("missing_docs")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_crate_root_that_allows_missing_docs_is_reported() {
        let violations =
            check_missing_docs(&roots("#![warn(missing_docs)]\n#![allow(missing_docs)]\n"));
        assert!(
            violations.iter().any(|v| v.detail.contains("back off")),
            "{violations:?}"
        );
    }

    #[test]
    fn every_way_of_silencing_missing_docs_is_the_same_regression() {
        // Each of these leaves `#![warn(missing_docs)]` in the file and the lint off in
        // the build, so each has to be the same violation rather than four holes.
        for silencer in [
            "#![allow(missing_docs)]",
            "#![expect(missing_docs)]",
            "#![allow(warnings)]",
            "#![cfg_attr(all(), allow(missing_docs))]",
            "#![allow(\n    missing_docs\n)]",
        ] {
            let violations =
                check_missing_docs(&roots(&format!("#![warn(missing_docs)]\n{silencer}\n")));
            assert!(
                violations.iter().any(|v| v.detail.contains("back off")),
                "{silencer} was not caught: {violations:?}"
            );
        }
    }

    #[test]
    fn a_block_commented_missing_docs_attribute_does_not_count() {
        let violations = check_missing_docs(&roots(
            "/* off for now:\n#![warn(missing_docs)]\n*/\npub struct Undocumented;\n",
        ));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("does not warn")),
            "{violations:?}"
        );
    }

    #[test]
    fn deny_and_forbid_satisfy_the_missing_docs_rule() {
        for attribute in ["#![deny(missing_docs)]", "#![forbid(missing_docs)]"] {
            assert!(
                check_missing_docs(&roots(&format!("//! Docs.\n{attribute}\n"))).is_empty(),
                "{attribute}"
            );
        }
    }

    #[test]
    fn a_lint_named_beside_another_in_one_attribute_still_counts() {
        // A correct crate root must not be rejected for having said two things at once.
        assert!(check_missing_docs(&roots("#![warn(missing_docs, unreachable_pub)]\n")).is_empty());
    }

    #[test]
    fn a_clippy_lint_whose_name_contains_the_rustc_one_is_not_a_silencer() {
        assert!(
            check_missing_docs(&roots(
                "#![warn(missing_docs)]\n#![allow(clippy::missing_docs_in_private_items)]\n"
            ))
            .is_empty()
        );
    }

    fn spec_inputs() -> (String, Vec<AdrFile>, String) {
        (
            clean_claude_md(RULES),
            super::tests_support::clean_adrs(),
            super::tests_support::clean_spec_obligations(),
        )
    }

    #[test]
    fn a_documented_and_proved_specification_passes() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&obligations));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_clause_the_crate_stopped_declaring_is_a_guarantee_with_no_proof() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let first = SPEC_CLAUSES.first().expect("the table is not empty");
        let gutted = obligations.replace(&format!("id: \"{}\"", first.id), "id: \"\"");
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&gutted));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == first.id
                    && violation.detail.contains("no clause with this id")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_pointed_at_a_different_proof_than_the_documentation_names_is_reported() {
        // The id alone is not enough: two tables can agree on the names of six things and
        // disagree about all six of them.
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.last().expect("the table is not empty");
        let moved = obligations.replace(
            &format!("proof: \"{}\"", clause.discharged_by),
            "proof: \"tests/somewhere_else.rs\"",
        );
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&moved));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id
                    && violation.detail.contains("tests/somewhere_else.rs")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_row_that_names_no_proof_at_all_is_reported() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.last().expect("the table is not empty");
        let stripped = obligations.replace(&format!(", proof: \"{}\"", clause.discharged_by), "");
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&stripped));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id
                    && violation.detail.contains("names no proof")),
            "{violations:?}"
        );
    }

    #[test]
    fn one_row_naming_a_shared_proof_does_not_vouch_for_the_others() {
        // Four of the six clauses are discharged by the same file. Three global `contains`
        // calls would let any one of their rows satisfy the check on behalf of all four —
        // the defect `deferred-questions` was fixed for on PR #58, which bites harder here
        // because the shared string is the one a reader most needs to be right.
        let (claude_md, adrs, obligations) = spec_inputs();
        let shared = "tests/spine.rs";
        let sharing: Vec<&str> = SPEC_CLAUSES
            .iter()
            .filter(|clause| clause.discharged_by == shared)
            .map(|clause| clause.id)
            .collect();
        assert!(sharing.len() > 1, "this test needs a shared proof file");
        let gutted: String = claude_md
            .lines()
            .map(|row| {
                let names_a_sharer = sharing
                    .iter()
                    .skip(1)
                    .any(|id| row.contains(&format!("`{id}`")));
                if names_a_sharer {
                    row.replace(shared, "somewhere")
                } else {
                    row.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let violations = check_recovery_spec(Some(&gutted), &adrs, Some(&obligations));
        for id in sharing.iter().skip(1) {
            assert!(
                violations.iter().any(|violation| violation.subject == *id
                    && violation.detail.contains("discharged by")),
                "`{id}` lost its proof and nothing said so: {violations:?}"
            );
        }
    }

    #[test]
    fn a_clause_only_inside_a_fenced_block_in_claude_md_does_not_vouch_for_it() {
        // The counterpart of the ADR half's fence test. Without it, the whole guarantees
        // section could be replaced by a fenced listing of the six ids.
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let fenced = claude_md.replace(
            &format!("| `{}` |", clause.id),
            &format!("```text\n| `{}` |", clause.id),
        ) + "\n```\n";
        let violations = check_recovery_spec(Some(&fenced), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_named_by_two_rows_is_reported_rather_than_resolved() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let row = format!(
            "| `{}` | {} | {} |",
            clause.id, clause.headline, clause.discharged_by
        );
        let doubled = claude_md.replace(&row, &format!("{row}\n{row}"));
        let violations = check_recovery_spec(Some(&doubled), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id
                    && violation.detail.contains("2 table rows")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_id_in_prose_above_the_table_does_not_shadow_its_row() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let prefixed = format!("Prose about `{}` before the table.\n{claude_md}", clause.id);
        let violations = check_recovery_spec(Some(&prefixed), &adrs, Some(&obligations));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_clause_the_crate_invented_is_a_proof_with_no_documentation() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let extended = format!("{obligations}\n    Clause {{ id: \"time-travel\" }},\n");
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&extended));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == "time-travel"
                    && violation.detail.contains("does not")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_table_the_gate_cannot_read_fails_closed() {
        let (claude_md, adrs, _) = spec_inputs();
        let violations = check_recovery_spec(Some(&claude_md), &adrs, None);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "recovery-spec");
        assert!(violations[0].detail.contains("nothing holds the proofs"));
    }

    #[test]
    fn an_unbackticked_clause_id_does_not_count() {
        // The backticks are what stop `durable-intent` being vouched for by a longer id, or
        // by the prose that happens to use the same words.
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let stripped = claude_md.replace(&format!("`{}`", clause.id), clause.id);
        let violations = check_recovery_spec(Some(&stripped), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id
                    && violation.detail.contains("backticks")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_row_that_names_no_proof_is_a_row_describing_one() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES
            .iter()
            .find(|clause| clause.discharged_by == "tests/redelivery.rs")
            .expect("one clause is discharged by the redelivery proof");
        let stripped = claude_md.replace(clause.discharged_by, "somewhere");
        let violations = check_recovery_spec(Some(&stripped), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("discharged by")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_guarantee_stated_in_other_words_is_a_guarantee_that_drifted() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let reworded = claude_md.replace(clause.headline, "recovery does the right thing");
        let violations = check_recovery_spec(Some(&reworded), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("docs::SPEC_CLAUSES reads")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_count_that_does_not_match_the_table_is_reported() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let miscounted = claude_md.replace(
            &format!("All {} recovery invariants", SPEC_CLAUSES.len()),
            "All 2 recovery invariants",
        );
        let violations = check_recovery_spec(Some(&miscounted), &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == "clause count"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_specification_with_no_decision_record_is_reported_once() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let without: Vec<AdrFile> = adrs
            .into_iter()
            .filter(|adr| adr.name != RECOVERY_SPEC_ADR)
            .collect();
        let violations = check_recovery_spec(Some(&claude_md), &without, Some(&obligations));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].subject, RECOVERY_SPEC_ADR);
    }

    #[test]
    fn a_decision_record_that_stops_naming_a_clause_is_reported() {
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.last().expect("the table is not empty");
        let edited: Vec<AdrFile> = adrs
            .into_iter()
            .map(|adr| {
                if adr.name == RECOVERY_SPEC_ADR {
                    AdrFile {
                        contents: adr.contents.replace(&format!("`{}`", clause.id), ""),
                        ..adr
                    }
                } else {
                    adr
                }
            })
            .collect();
        let violations = check_recovery_spec(Some(&claude_md), &edited, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id
                    && violation.detail.contains("no decision record")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_id_only_in_a_fenced_example_in_the_adr_does_not_vouch_for_it() {
        // The same trap `deferred-questions` was caught in: an ADR that documents the clause
        // syntax in a fenced example would otherwise satisfy the check while having lost the
        // real citation.
        let (claude_md, adrs, obligations) = spec_inputs();
        let clause = SPEC_CLAUSES.first().expect("the table is not empty");
        let edited: Vec<AdrFile> = adrs
            .into_iter()
            .map(|adr| {
                if adr.name == RECOVERY_SPEC_ADR {
                    AdrFile {
                        contents: adr.contents.replace(
                            &format!("- `{}`", clause.id),
                            &format!("```text\n`{}`\n```", clause.id),
                        ),
                        ..adr
                    }
                } else {
                    adr
                }
            })
            .collect();
        let violations = check_recovery_spec(Some(&claude_md), &edited, Some(&obligations));
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == clause.id),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_claude_md_is_left_to_the_rule_that_already_reports_it() {
        let (_, adrs, obligations) = spec_inputs();
        let violations = check_recovery_spec(None, &adrs, Some(&obligations));
        assert!(
            violations
                .iter()
                .all(|violation| violation.subject == RECOVERY_SPEC_ADR
                    || violation.detail.contains("obligation")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clause_id_in_a_doc_comment_alone_does_not_count_as_a_declaration() {
        // The scan matches the table's own `id: "..."` field. A clause that only appears in
        // prose above the table is a clause the crate documents and does not declare.
        let (claude_md, adrs, _) = spec_inputs();
        let mut prose = String::new();
        for clause in SPEC_CLAUSES {
            let _ = writeln!(prose, "//! `{}` is a guarantee.", clause.id);
        }
        let violations = check_recovery_spec(Some(&claude_md), &adrs, Some(&prose));
        assert_eq!(violations.len(), SPEC_CLAUSES.len(), "{violations:?}");
    }

    #[test]
    fn documentation_that_does_not_exist_fires_every_rule_that_can_fire_on_it() {
        // `adr-structure` and `missing-docs` are absent on purpose: both iterate a
        // collection, and both collections are empty here. Their own tests cover them.
        let violations = check_documentation(&DocsInputs::default(), RULES);
        assert_eq!(
            rule_ids(&violations),
            [
                "adr-index",
                "adr-numbering",
                "claude-md",
                "deferred-questions",
                "diagrams",
                "recovery-spec",
                "settled-decisions"
            ]
        );
    }
}
