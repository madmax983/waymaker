//! The book, and the hardware compatibility matrix.
//!
//! Issue [#42](https://github.com/madmax983/waymaker/issues/42) asks for an mdBook and a
//! matrix of the parts Waymaker has been tested on. Both are prose, and prose is the one
//! artifact in this repository that rots without anything going red — so both are held to
//! the tables that already own the facts they state.
//!
//! Two rules live here.
//!
//! `book` holds the book's *shape*: the chapters issue #42 lists are the chapters the book
//! has, each is linked from `SUMMARY.md`, and every code sample is an
//! `{{#include}}` of an anchor in a file `cargo test` runs. A chapter may not carry a Rust
//! fence of its own, which is the rule that makes "tested, not merely quoted" a build
//! failure: a quoted sample is a sample nothing runs.
//!
//! `hardware-matrix` holds the matrix's *contents*: every part is a row, a board's
//! power-cut column is read from [`crate::docs::HARDWARE_TARGETS`] rather than typed, and
//! the write-amplification column is what [`crate::wear`] measured on this run.
//!
//! # What neither can see
//!
//! Prose. Both match ids and rendered cells against tables; the sentences around them are
//! reviewed by people, which is the limit `CLAUDE.md`'s own "what is not checked" section
//! states for every rule in this workspace.

use core::fmt::Write as _;

use crate::Violation;
use crate::docs::{Attestation, HARDWARE_TARGETS};
use crate::wear::{PARTS, PartWear};

/// Where the book lives, relative to the workspace root.
pub const BOOK_DIR: &str = "docs/book";

/// The book's manifest.
pub const BOOK_MANIFEST: &str = "docs/book/book.toml";

/// The book's Markdown, relative to the workspace root.
pub const BOOK_SOURCE_DIR: &str = "docs/book/src";

/// The table of contents, relative to [`BOOK_SOURCE_DIR`].
pub const BOOK_SUMMARY: &str = "SUMMARY.md";

/// One chapter of the book.
///
/// The rows are issue #42's own bullets. A chapter is a row here, a file under
/// [`BOOK_SOURCE_DIR`], and a link in [`BOOK_SUMMARY`]; a chapter missing from any of the
/// three fails the `book` rule, in both directions, so a chapter cannot be added without
/// being reachable and cannot be deleted without being removed from the list of things the
/// book claims to cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chapter {
    /// Stable id, cited when a change touches this chapter.
    pub id: &'static str,
    /// The file, relative to [`BOOK_SOURCE_DIR`].
    pub file: &'static str,
    /// The title, as [`BOOK_SUMMARY`] must link it.
    pub title: &'static str,
    /// Which of issue #42's bullets this chapter is.
    pub covers: &'static str,
}

/// Every chapter issue #42 asks for, in reading order.
pub const BOOK_CHAPTERS: &[Chapter] = &[
    Chapter {
        id: "design-centre",
        file: "design-centre.md",
        title: "The design centre",
        covers: "the invariant, stated before the API",
    },
    Chapter {
        id: "determinism",
        file: "determinism.md",
        title: "The determinism contract",
        covers: "what a workflow may not read directly, and how those values arrive instead",
    },
    Chapter {
        id: "effect-protocol",
        file: "effect-protocol.md",
        title: "The durable effect protocol",
        covers: "design document §07's seven steps as reference material",
    },
    Chapter {
        id: "failure-semantics",
        file: "failure-semantics.md",
        title: "Failure semantics",
        covers: "design document §14's ten rows as reference material",
    },
    Chapter {
        id: "wire-format",
        file: "wire-format.md",
        title: "The wire format",
        covers: "the byte-level format",
    },
    Chapter {
        id: "not-promised",
        file: "not-promised.md",
        title: "What is not promised",
        covers: "the honest chapter on the non-goals",
    },
    Chapter {
        id: "porting",
        file: "porting.md",
        title: "Porting to a new part",
        covers: "implementing StableStorage and PersistentClock",
    },
    Chapter {
        id: "hardware-matrix",
        file: "hardware-matrix.md",
        title: "The hardware compatibility matrix",
        covers: "the matrix of tested parts",
    },
];

/// The files a chapter may take a sample from, by workspace-relative path.
///
/// One entry, and the entry is an integration test. That is the whole of issue #42's "its
/// code samples are tested, not merely quoted": the bytes the book shows are the bytes of a
/// file the `test` stage compiles and runs, so a sample that stopped compiling is a red
/// pipeline rather than a wrong page.
pub const BOOK_SAMPLE_FILES: &[&str] = &["crates/waymaker-drive/tests/book.rs"];

/// The documents a chapter may include whole, by workspace-relative path.
///
/// The frozen format is stated byte by byte in one place and the book shows that place
/// rather than a copy of it. A second copy is the failure this list exists to prevent: it
/// would pass every rule that reads the original and say something else.
pub const BOOK_DOCUMENT_INCLUDES: &[&str] = &["docs/format/wire-format-v1.md"];

/// The chapter that must carry the frozen format, and the document it must include.
pub const WIRE_FORMAT_CHAPTER: (&str, &str) = ("wire-format", "docs/format/wire-format-v1.md");

/// Something Waymaker does not promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonGoal {
    /// Stable id, cited when a change touches this promise.
    pub id: &'static str,
    /// The headline, as the `not-promised` chapter must state it.
    pub headline: &'static str,
    /// Where the engine already records it.
    pub recorded: &'static str,
}

/// The promises Waymaker declines to make.
///
/// Issue #42 names four; the fifth is [ADR 0037]'s, and it is here for the same reason the
/// other four are: a reader who finds out about it from a fleet has found out too late.
///
/// [ADR 0037]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md
pub const NON_GOALS: &[NonGoal] = &[
    NonGoal {
        id: "at-least-once-effects",
        headline: "no exactly-once physical effects",
        recorded: "`Activities::perform`'s own documentation, and ADR 0026",
    },
    NonGoal {
        id: "no-snapshotted-futures",
        headline: "no persisted stacks or suspended futures",
        recorded: "§02 decision 6, `no-snapshotted-futures`",
    },
    NonGoal {
        id: "no-distributed-consensus",
        headline: "no distributed consensus",
        recorded: "design document §03: one device decides, and nothing votes",
    },
    NonGoal {
        id: "boot-timer-is-not-power-loss-durable",
        headline: "no AfterBoot timer surviving power loss",
        recorded: "§02 decision 8, and ADR 0028",
    },
    NonGoal {
        id: "no-downgrade",
        headline: "no downgrade past a record kind a device has already written",
        recorded: "ADR 0037",
    },
];

/// Whether a part has a clock that outlives the supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcSupport {
    /// A battery- or supercapacitor-backed RTC, which is what an `AtPersistentTime`
    /// deadline needs.
    Backed,
    /// None at all. A modelled part has no clock, and a firmware on one may arm no
    /// persistent deadline.
    Absent,
    /// Not known, because no board has been attached.
    Unknown,
}

impl RtcSupport {
    /// The cell as the matrix must render it.
    #[must_use]
    pub const fn render(self) -> &'static str {
        match self {
            Self::Backed => "Backed RTC",
            Self::Absent => "None",
            Self::Unknown => "Not known",
        }
    }
}

/// What a matrix row is a row about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatrixPart {
    /// A board, named by its [`crate::docs::HARDWARE_TARGETS`] id. Its geometry and its
    /// write amplification are the board's to report and no host can supply them.
    Board(&'static str),
    /// A part [`crate::wear`] models, named by its [`PARTS`] row. Its geometry is that
    /// row's and its write amplification is measured on every run of the gate.
    Model(&'static str),
}

/// The program unit every crash sweep in this workspace runs at.
///
/// `waymaker-fault`'s and `waymaker-rig`'s sweeps all lay out a part with a four-byte
/// program unit. So a modelled part with another unit has a *measured* wear figure and no
/// crash sweep of its own, and [`render_power_cut`] says which — because "swept on the host
/// model" printed against a row nothing has ever swept is the flattering kind of wrong.
pub const SWEPT_PROGRAM_BYTES: u32 = 4;

/// One part the matrix covers.
///
/// Every column but [`rtc`](Self::rtc) is derived rather than declared: the geometry comes
/// from [`PARTS`], the power-cut standing from [`HARDWARE_TARGETS`] or from
/// [`SWEPT_PROGRAM_BYTES`], and the write amplification from the measurement this run took.
/// A matrix whose cells were typed is a matrix that would still say `Passed` after the row
/// it describes stopped being true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatrixRow {
    /// Stable id, cited when a change touches this part, and the first cell of its row.
    pub id: &'static str,
    /// What the part is, in words, as the row's second cell.
    pub label: &'static str,
    /// What the row is about.
    pub part: MatrixPart,
    /// Whether the part has a clock that outlives the supply. The one declared column,
    /// because it is a fact about hardware and no table here holds it.
    pub rtc: RtcSupport,
}

/// Every part the matrix covers: the three boards two rungs owe, and the three parts the
/// wear figure is measured on.
pub const HARDWARE_MATRIX: &[MatrixRow] = &[
    MatrixRow {
        id: "cortex-m0plus",
        label: "A Cortex-M0+ board",
        part: MatrixPart::Board("cortex-m0plus"),
        rtc: RtcSupport::Unknown,
    },
    MatrixRow {
        id: "cortex-m4",
        label: "A Cortex-M4 board",
        part: MatrixPart::Board("cortex-m4"),
        rtc: RtcSupport::Unknown,
    },
    MatrixRow {
        id: "rtc-power-loss",
        label: "A board with a backed RTC",
        part: MatrixPart::Board("rtc-power-loss"),
        rtc: RtcSupport::Backed,
    },
    MatrixRow {
        id: "byte-programmable",
        label: "Modelled NOR, byte programs",
        part: MatrixPart::Model("byte-programmable"),
        rtc: RtcSupport::Absent,
    },
    MatrixRow {
        id: "word-programmable",
        label: "Modelled NOR, word programs",
        part: MatrixPart::Model("word-programmable"),
        rtc: RtcSupport::Absent,
    },
    MatrixRow {
        id: "page-programmable",
        label: "Modelled NOR, 16-byte pages",
        part: MatrixPart::Model("page-programmable"),
        rtc: RtcSupport::Absent,
    },
];

/// The matrix table's header, pinned because every cell below it is derived.
pub const MATRIX_TABLE_HEADER: &[&str] = &[
    "Id",
    "Part",
    "Erase/program/read",
    "Power cut",
    "Clock",
    "Written B per effect",
];

/// The cell a row with no host-side answer renders.
pub const NOT_MEASURED: &str = "Not measured";

/// The cell a board that has passed its loops would render.
///
/// No row renders it today. It is a constant so that the ban on the *word* elsewhere in the
/// chapter can be lifted by the same change that earns it.
pub const PASSED: &str = "Passed";

/// Languages a chapter may quote verbatim.
///
/// The ban is on a chapter carrying *source*, and the way to express that is an allowlist
/// rather than a list of the languages that count as Rust: an info string this list does not
/// name — the empty one included — must hold include directives and nothing else. Codex's
/// review of this change reached a bare ` ``` ` fence carrying Rust past a version that
/// asked whether the info string said "rust".
pub const QUOTABLE_FENCE_LANGUAGES: &[&str] = &[
    "text", "sh", "console", "toml", "json", "yaml", "mermaid", "diff",
];

/// The only mdBook link directive a chapter may use.
pub const INCLUDE_DIRECTIVE: &str = "{{#include";

/// Anchors that show a fixture rather than the test named after them.
///
/// The rule is that an anchor must *contain* the `#[test] fn` of its own name, so that the
/// bytes on the page are the bytes that run. Three anchors show a type a test uses instead —
/// a workflow, a storage adapter, a clock driver — and each is a line written on purpose
/// here, because review of this change shrank an anchor to two comment lines advertising an
/// API that does not exist and watched the gate stay green.
pub const BOOK_FIXTURE_ANCHORS: &[&str] = &[
    "a_workflow_is_a_value_with_a_method",
    "a_storage_adapter_is_four_operations_and_a_barrier",
    "a_persistent_clock_is_a_reading_and_a_bit",
];

/// What a Rust item declaration starts with, for a fixture anchor that must declare one.
const ITEM_KEYWORDS: &[&str] = &[
    "struct ", "enum ", "trait ", "impl ", "fn ", "const ", "type ",
];

/// Everything the two rules read, already collected off disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookInputs {
    /// Contents of [`BOOK_MANIFEST`], when the repository has one.
    pub manifest: Option<String>,
    /// Every file under [`BOOK_SOURCE_DIR`], by name relative to it, with its text.
    ///
    /// Every file rather than every `.md` file: review of this change added a chapter named
    /// `rogue.MD`, linked it from the summary, and watched a case-sensitive collector leave
    /// it covered by nothing.
    pub pages: Vec<(String, String)>,
    /// Every file of [`BOOK_SAMPLE_FILES`] that exists, by workspace-relative path.
    pub samples: Vec<(String, String)>,
    /// Every path of [`BOOK_DOCUMENT_INCLUDES`] that exists.
    pub documents: Vec<String>,
    /// What [`crate::wear::measure`] answered on this run.
    ///
    /// An `Err` is a violation rather than a skipped column: a measurement that did not
    /// happen is not a measurement that passed.
    pub wear: Result<Vec<PartWear>, String>,
}

impl BookInputs {
    /// A repository with no book at all, and no measurement.
    #[must_use]
    pub fn absent() -> Self {
        Self {
            manifest: None,
            pages: Vec::new(),
            samples: Vec::new(),
            documents: Vec::new(),
            wear: Err("no measurement was taken".to_owned()),
        }
    }

    /// The page named `file`, when the book has it.
    fn page(&self, file: &str) -> Option<&str> {
        self.pages
            .iter()
            .find(|(name, _)| name == file)
            .map(|(_, contents)| contents.as_str())
    }
}

/// The geometry cell a row renders: erase, program and read sizes, or [`NOT_MEASURED`].
#[must_use]
pub fn render_geometry(row: &MatrixRow) -> String {
    match row.part {
        MatrixPart::Board(_) => NOT_MEASURED.to_owned(),
        MatrixPart::Model(name) => PARTS.iter().find(|(part, ..)| *part == name).map_or_else(
            || NOT_MEASURED.to_owned(),
            |(_, _, erase, program, read)| format!("{erase} / {program} / {read}"),
        ),
    }
}

/// The power-cut cell a row renders.
///
/// A board's is read from [`HARDWARE_TARGETS`], so a row cannot claim a pass the decision
/// record does not carry — and flipping that table needs an accepted ADR, which is
/// `hardware-attestation`'s. A model's is read from its program unit against
/// [`SWEPT_PROGRAM_BYTES`], because a part nothing has swept has not been swept.
#[must_use]
pub fn render_power_cut(row: &MatrixRow) -> &'static str {
    match row.part {
        MatrixPart::Board(id) => HARDWARE_TARGETS
            .iter()
            .find(|target| target.id == id)
            .map_or(NOT_MEASURED, |target| match target.attestation {
                Attestation::NotRun => "Not run",
                Attestation::Passed => PASSED,
            }),
        MatrixPart::Model(name) => PARTS.iter().find(|(part, ..)| *part == name).map_or(
            NOT_MEASURED,
            |(_, _, _, program, _)| {
                if *program == SWEPT_PROGRAM_BYTES {
                    "Swept on the host model"
                } else {
                    "Not swept at this program unit"
                }
            },
        ),
    }
}

/// The write-amplification cell a row renders, or why it could not be rendered.
///
/// A board has no host-side answer and renders [`NOT_MEASURED`]. A modelled part with no
/// figure is an `Err`: folding "the measurement produced nothing" into the same cell a board
/// gets would publish a degenerate run as a blank and leave the build green.
///
/// # Errors
///
/// When a modelled part is missing from the measurement, or measured a run with no effect in
/// it.
pub fn render_write_amplification(
    row: &MatrixRow,
    measured: &[PartWear],
) -> Result<String, String> {
    match row.part {
        MatrixPart::Board(_) => Ok(NOT_MEASURED.to_owned()),
        MatrixPart::Model(name) => measured
            .iter()
            .find(|part| part.part == name)
            .ok_or_else(|| format!("the measurement has no row for `{name}`"))?
            .engine
            .programmed_bytes_per_effect()
            .map(|figure| figure.to_string())
            .ok_or_else(|| format!("`{name}` measured a run with no completed effect in it")),
    }
}

/// One matrix row, as every cell of it must appear.
///
/// # Errors
///
/// When a cell cannot be derived, which is a failure rather than a blank column.
pub fn derived_cells(row: &MatrixRow, measured: &[PartWear]) -> Result<Vec<String>, String> {
    Ok(vec![
        format!("`{}`", row.id),
        row.label.to_owned(),
        render_geometry(row),
        render_power_cut(row).to_owned(),
        row.rtc.render().to_owned(),
        render_write_amplification(row, measured)?,
    ])
}

/// A `{{#include}}` a chapter carries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Include {
    /// The argument as written, for a violation message.
    raw: String,
    /// The target, resolved against [`BOOK_SOURCE_DIR`] into a workspace-relative path.
    /// `None` when the relative path climbs above the workspace root.
    path: Option<String>,
    /// What of the target is shown.
    part: IncludePart,
}

/// Which part of an included file a chapter shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IncludePart {
    /// The whole file. Only a document may be included this way.
    Whole,
    /// One named anchor. Only a sample file may be included this way.
    Anchor(String),
    /// Anything else mdBook accepts — a line range, or a range of anchors.
    Unsupported,
}

/// One anchor of a sample file: its name, its text, and the lines it spans.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor {
    name: String,
    body: String,
    /// Index of the `ANCHOR:` line.
    start: usize,
    /// Index of the `ANCHOR_END:` line.
    end: usize,
}

/// `contents` with every HTML comment removed.
///
/// Comments render as nothing, so text inside one satisfies a reader of the source and no
/// reader of the page. Review of this change hid an honest matrix in comments under a
/// fabricated table and watched the gate stay green.
fn without_html_comments(contents: &str) -> String {
    let mut kept = String::new();
    let mut rest = contents;
    while let Some((before, after)) = rest.split_once("<!--") {
        kept.push_str(before);
        kept.push('\n');
        let Some((_, resumed)) = after.split_once("-->") else {
            return kept;
        };
        rest = resumed;
    }
    kept.push_str(rest);
    kept
}

/// The fence marker and its info string, for a line that opens or closes a fenced block.
fn fence_of(line: &str) -> Option<(char, usize, &str)> {
    let marker = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = line.chars().take_while(|c| *c == marker).count();
    if length < 3 {
        return None;
    }
    Some((marker, length, line.get(length..).unwrap_or_default()))
}

/// Every fenced block a page carries, as its info string and its body.
fn fenced_blocks(page: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for line in page.lines() {
        if let Some((marker, length, info)) = fence_of(line.trim()) {
            match open {
                // A closing fence carries no info string, which tells the two apart.
                Some((open_marker, open_length))
                    if marker == open_marker && length >= open_length && info.trim().is_empty() =>
                {
                    open = None;
                    continue;
                }
                Some(_) => {}
                None => {
                    found.push((info.trim().to_owned(), String::new()));
                    open = Some((marker, length));
                    continue;
                }
            }
        }
        if open.is_some() {
            if let Some((_, body)) = found.last_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    found
}

/// Every line of `page` that is outside a fenced block, with its index.
fn unfenced_lines(page: &str) -> Vec<(usize, &str)> {
    let mut kept = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for (at, line) in page.lines().enumerate() {
        if let Some((marker, length, info)) = fence_of(line.trim()) {
            match open {
                Some((open_marker, open_length))
                    if marker == open_marker && length >= open_length && info.trim().is_empty() =>
                {
                    open = None;
                }
                Some(_) => {}
                None => open = Some((marker, length)),
            }
            continue;
        }
        if open.is_none() {
            kept.push((at, line));
        }
    }
    kept
}

/// Whether a fence's info string names a language a chapter may quote verbatim.
fn is_quotable(info: &str) -> bool {
    let language = info
        .trim()
        .split([',', ' ', '\t', '{'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    QUOTABLE_FENCE_LANGUAGES.contains(&language.as_str())
}

/// Whether a line is exactly one `{{#include}}` directive and nothing else.
///
/// `starts_with` and `ends_with` are not enough: `{{#include a}} let x = 1; {{#include b}}`
/// satisfies both, renders the source between them, and is the mutation review of this
/// change ran.
fn is_one_include(line: &str) -> bool {
    line.starts_with(INCLUDE_DIRECTIVE) && line.find("}}").is_some_and(|at| at + 2 == line.len())
}

/// Whether a fence body is nothing but include directives and blank lines.
fn is_only_includes(body: &str) -> bool {
    let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
    lines.clone().count() > 0 && lines.all(is_one_include)
}

/// `relative`, resolved against [`BOOK_SOURCE_DIR`], as a workspace-relative path.
///
/// Returns `None` for a path that climbs above the workspace root, which is a path no rule
/// below can say anything useful about.
fn resolve(relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = BOOK_SOURCE_DIR.split('/').collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Every `{{#include}}` a page carries, in the order they appear.
fn includes(page: &str) -> Vec<Include> {
    let mut found = Vec::new();
    let mut rest = page;
    while let Some((_, after)) = rest.split_once(INCLUDE_DIRECTIVE) {
        let Some((argument, resumed)) = after.split_once("}}") else {
            break;
        };
        rest = resumed;
        let argument = argument.trim();
        let mut fields = argument.split(':');
        let path = fields.next().unwrap_or_default().trim();
        let remainder: Vec<&str> = fields.collect();
        let part = match remainder.as_slice() {
            [] => IncludePart::Whole,
            // A single non-numeric field is an anchor. A number is a line, and a line
            // range is a citation that goes wrong the first time somebody inserts a line
            // above it.
            [anchor] if !anchor.is_empty() && !anchor.bytes().all(|byte| byte.is_ascii_digit()) => {
                IncludePart::Anchor((*anchor).to_owned())
            }
            _ => IncludePart::Unsupported,
        };
        found.push(Include {
            raw: argument.to_owned(),
            path: resolve(path),
            part,
        });
    }
    found
}

/// Every mdBook directive a page carries that is not an `{{#include}}`.
///
/// `{{#playground}}` renders an arbitrary source file as a runnable Rust block with no fence
/// at all, and `{{#rustdoc_include}}` takes the line ranges the include rule refuses. Both
/// were reached past an earlier version of this module.
fn foreign_directives(page: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = page;
    while let Some((_, after)) = rest.split_once("{{#") {
        let name: String = after
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .collect();
        rest = after;
        if format!("{{{{#{name}") != INCLUDE_DIRECTIVE {
            found.push(name);
        }
    }
    found
}

/// Every anchor a sample file declares, with its text and the lines it spans.
///
/// A stack rather than one slot, because mdBook allows anchors to overlap and a single slot
/// drops the outer one.
fn anchors(sample: &str) -> Vec<Anchor> {
    let mut found: Vec<Anchor> = Vec::new();
    let mut open: Vec<(String, usize, Vec<&str>)> = Vec::new();
    for (at, line) in sample.lines().enumerate() {
        let trimmed = line.trim();
        let marker = trimmed
            .strip_prefix("//")
            .map(str::trim)
            .and_then(|rest| rest.strip_prefix("ANCHOR"));
        match marker.map(|rest| rest.split_once(':')) {
            Some(Some((tag, name))) if tag.trim() == "_END" => {
                let name = name.trim();
                if let Some(index) = open.iter().rposition(|(open_name, ..)| open_name == name) {
                    let (name, start, body) = open.remove(index);
                    found.push(Anchor {
                        name,
                        body: body.join("\n"),
                        start,
                        end: at,
                    });
                }
                continue;
            }
            Some(Some((tag, name))) if tag.trim().is_empty() => {
                open.push((name.trim().to_owned(), at, Vec::new()));
                continue;
            }
            _ => {}
        }
        for (_, _, body) in &mut open {
            body.push(line);
        }
    }
    found
}

/// Where `sample` declares `#[test] fn name(`, or why it does not.
///
/// The whole contiguous attribute run before the function is read, in both directions:
/// attribute order is free, and review of this change put `#[ignore]` *above* the anchor
/// marker, where a reader of the book never sees it and the test never runs.
fn declares_test(sample: &str, name: &str) -> Result<usize, String> {
    let opening = format!("fn {name}(");
    let mut run: Vec<&str> = Vec::new();
    for (at, line) in sample.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with(&opening) {
            if !run.contains(&"#[test]") {
                run.clear();
                continue;
            }
            if let Some(refused) = run.iter().find(|attribute| {
                attribute.starts_with("#[ignore")
                    || attribute.starts_with("#[cfg(")
                    || attribute.starts_with("#[cfg_attr(")
            }) {
                return Err(format!(
                    "carries `{refused}`, so the test does not run and the sample the book \
                     shows is compiled or executed by nothing"
                ));
            }
            return Ok(at);
        }
        if trimmed.starts_with("#[") {
            run.push(trimmed);
        } else if !(trimmed.is_empty() || trimmed.starts_with("//")) {
            run.clear();
        }
    }
    Err(format!("declares no `#[test] fn {name}`"))
}

/// Every link target a summary names that is not an absolute URL.
fn summary_links(summary: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = summary;
    while let Some((_, after)) = rest.split_once("](") {
        let Some((target, resumed)) = after.split_once(')') else {
            break;
        };
        rest = resumed;
        let target = target.trim();
        if !target.contains("://") && !target.is_empty() {
            found.push(target.to_owned());
        }
    }
    found
}

/// Whether a line of `contents` carries every one of `cells`.
fn a_line_carries(contents: &str, cells: &[&str]) -> bool {
    contents
        .lines()
        .any(|line| cells.iter().all(|cell| line.contains(cell)))
}

/// Every Markdown table row of `chapter`, as its trimmed cells.
fn table_rows(chapter: &str) -> Vec<Vec<String>> {
    chapter
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

/// Whether a table row is the `| --- | --- |` rule under a header.
fn is_separator(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let body = cell.trim_matches(':');
            !body.is_empty() && body.bytes().all(|byte| byte == b'-')
        })
}

/// Rule: the book is the book issue #42 asks for, and its samples are tested.
#[must_use]
pub fn check_book(
    inputs: &BookInputs,
    claude_md: Option<&str>,
    readme: Option<&str>,
) -> Vec<Violation> {
    const RULE: &str = "book";
    let mut violations = Vec::new();

    match inputs.manifest.as_deref() {
        None => violations.push(Violation::new(
            RULE,
            BOOK_MANIFEST,
            format!("{BOOK_MANIFEST} is missing, so there is no book for the pipeline to build"),
        )),
        Some(manifest) => {
            for required in ["[book]", "title"] {
                if !manifest.contains(required) {
                    violations.push(Violation::new(
                        RULE,
                        BOOK_MANIFEST,
                        format!("declares no `{required}`, which mdBook needs to render a book"),
                    ));
                }
            }
        }
    }

    violations.extend(check_summary(inputs, RULE));
    violations.extend(check_chapters(inputs, RULE));
    violations.extend(check_samples(inputs, RULE));
    violations.extend(check_chapter_contents(inputs, RULE));

    let target = format!("{BOOK_DIR}/src/{BOOK_SUMMARY}");
    for (subject, contents) in [("CLAUDE.md", claude_md), ("README.md", readme)] {
        if !contents.is_some_and(|text| text.contains(&target)) {
            violations.push(Violation::new(
                RULE,
                subject,
                format!("does not link {target}; a book nothing links is a book nobody finds"),
            ));
        }
    }

    violations
}

/// The table of contents names every chapter and invents none.
fn check_summary(inputs: &BookInputs, rule: &'static str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Some(summary) = inputs.page(BOOK_SUMMARY) else {
        return vec![Violation::new(
            rule,
            BOOK_SUMMARY,
            format!("{BOOK_SOURCE_DIR}/{BOOK_SUMMARY} is missing, so mdBook has no book to render"),
        )];
    };

    let linked = summary_links(summary);
    for chapter in BOOK_CHAPTERS {
        if !linked.iter().any(|target| target == chapter.file) {
            violations.push(Violation::new(
                rule,
                chapter.id,
                format!(
                    "{BOOK_SUMMARY} does not link `{}`; a chapter no table of contents reaches \
                     is a chapter mdBook renders and nobody opens ({})",
                    chapter.file, chapter.covers
                ),
            ));
        } else if !a_line_carries(summary, &[chapter.title, chapter.file]) {
            violations.push(Violation::new(
                rule,
                chapter.id,
                format!(
                    "{BOOK_SUMMARY} links `{}` under another title; the table says `{}`",
                    chapter.file, chapter.title
                ),
            ));
        }
    }
    for target in &linked {
        if !BOOK_CHAPTERS.iter().any(|chapter| chapter.file == *target) {
            violations.push(Violation::new(
                rule,
                target.clone(),
                format!(
                    "is linked from {BOOK_SUMMARY} and is not a row of `book::BOOK_CHAPTERS`, \
                     so no rule covers what it says"
                ),
            ));
        }
    }
    violations
}

/// Every chapter is a file, and no file under the book's source is anything else.
fn check_chapters(inputs: &BookInputs, rule: &'static str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for chapter in BOOK_CHAPTERS {
        match inputs.page(chapter.file) {
            None => violations.push(Violation::new(
                rule,
                chapter.id,
                format!(
                    "{BOOK_SOURCE_DIR}/{} is missing; issue #42 asks the book to cover {}",
                    chapter.file, chapter.covers
                ),
            )),
            Some(page) if page.trim().is_empty() => violations.push(Violation::new(
                rule,
                chapter.id,
                format!("{BOOK_SOURCE_DIR}/{} is empty", chapter.file),
            )),
            Some(_) => {}
        }
    }
    for (name, _) in &inputs.pages {
        if name != BOOK_SUMMARY && !BOOK_CHAPTERS.iter().any(|chapter| chapter.file == *name) {
            violations.push(Violation::new(
                rule,
                name.clone(),
                format!(
                    "is a file of {BOOK_SOURCE_DIR} that `book::BOOK_CHAPTERS` does not declare, \
                     so no rule covers what it says"
                ),
            ));
        }
    }
    violations
}

/// Every sample the book shows is an anchor of a file the `test` stage runs, and every
/// anchor those files declare is shown.
fn check_samples(inputs: &BookInputs, rule: &'static str) -> Vec<Violation> {
    let mut violations = Vec::new();

    for path in BOOK_SAMPLE_FILES {
        if !inputs.samples.iter().any(|(name, _)| name == path) {
            violations.push(Violation::new(
                rule,
                *path,
                "is named by `book::BOOK_SAMPLE_FILES` and could not be read, so every sample \
                 the book takes from it is unchecked",
            ));
        }
    }
    for path in BOOK_DOCUMENT_INCLUDES {
        if !inputs.documents.iter().any(|name| name == path) {
            violations.push(Violation::new(
                rule,
                *path,
                "is named by `book::BOOK_DOCUMENT_INCLUDES` and could not be read, so the \
                 chapter that includes it renders an error message instead",
            ));
        }
    }

    let mut shown: Vec<(String, String)> = Vec::new();
    violations.extend(check_chapter_includes(inputs, rule, &mut shown));

    for (path, sample) in &inputs.samples {
        for anchor in anchors(sample) {
            if !shown
                .iter()
                .any(|(file, shown)| file == path && *shown == anchor.name)
            {
                violations.push(Violation::new(
                    rule,
                    path.clone(),
                    format!(
                        "declares the anchor `{}` that no chapter shows; a sample nobody quotes \
                         is a test wearing documentation's name",
                        anchor.name
                    ),
                ));
            }
        }
    }

    violations
}

/// Every directive every chapter carries, and what it names.
///
/// `shown` collects the `(file, anchor)` pairs the book displays, so the caller can report
/// an anchor nobody quotes.
fn check_chapter_includes(
    inputs: &BookInputs,
    rule: &'static str,
    shown: &mut Vec<(String, String)>,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for chapter in BOOK_CHAPTERS {
        let Some(page) = inputs.page(chapter.file) else {
            continue;
        };
        for directive in foreign_directives(page) {
            violations.push(Violation::new(
                rule,
                chapter.id,
                format!(
                    "carries `{{{{#{directive}}}}}`; the only directive a chapter may use is \
                     `{INCLUDE_DIRECTIVE}}}}}`. `{{{{#playground}}}}` renders an arbitrary source \
                     file as a Rust block with no fence at all, and `{{{{#rustdoc_include}}}}` \
                     takes the line ranges this rule refuses"
                ),
            ));
        }
        for include in includes(page) {
            let Some(path) = include.path.clone() else {
                violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "includes `{}`, which climbs above the workspace",
                        include.raw
                    ),
                ));
                continue;
            };
            let is_sample = BOOK_SAMPLE_FILES.contains(&path.as_str());
            let is_document = BOOK_DOCUMENT_INCLUDES.contains(&path.as_str());
            match &include.part {
                IncludePart::Unsupported => violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "includes `{}` by line number; a line range is a citation that stops \
                         being the right lines the first time somebody inserts one above it",
                        include.raw
                    ),
                )),
                IncludePart::Whole if !is_document => violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "includes the whole of `{path}`, which `book::BOOK_DOCUMENT_INCLUDES` \
                         does not name"
                    ),
                )),
                IncludePart::Whole => {}
                IncludePart::Anchor(anchor) if !is_sample => violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "takes the sample `{anchor}` from `{path}`, which \
                         `book::BOOK_SAMPLE_FILES` does not name; a sample from a file no stage \
                         runs is a sample nothing runs"
                    ),
                )),
                IncludePart::Anchor(anchor) => {
                    shown.push((path.clone(), anchor.clone()));
                    violations.extend(check_anchor(inputs, rule, chapter.id, &path, anchor));
                }
            }
        }
    }

    violations
}

/// One anchor exists, is the name of a test that runs, and is the text of that test.
fn check_anchor(
    inputs: &BookInputs,
    rule: &'static str,
    chapter: &'static str,
    path: &str,
    anchor: &str,
) -> Vec<Violation> {
    let Some((_, sample)) = inputs.samples.iter().find(|(name, _)| name == path) else {
        // The unreadable file is already reported once, by name.
        return Vec::new();
    };
    let mut violations = Vec::new();
    let Some(declared) = anchors(sample)
        .into_iter()
        .find(|found| found.name == anchor)
    else {
        violations.push(Violation::new(
            rule,
            chapter,
            format!(
                "shows `{anchor}` from `{path}`, which declares no such anchor; mdBook renders \
                 a missing anchor as an error message inside the page rather than failing"
            ),
        ));
        return violations;
    };

    let at = match declares_test(sample, anchor) {
        Ok(at) => Some(at),
        Err(why) => {
            violations.push(Violation::new(
                rule,
                chapter,
                format!(
                    "shows `{anchor}` from `{path}`, which {why}; issue #42 asks for samples \
                     that are tested rather than quoted, and the tie is the name"
                ),
            ));
            None
        }
    };

    if BOOK_FIXTURE_ANCHORS.contains(&anchor) {
        if !declared.body.lines().map(str::trim_start).any(|line| {
            ITEM_KEYWORDS
                .iter()
                .any(|keyword| line.starts_with(keyword))
        }) {
            violations.push(Violation::new(
                rule,
                chapter,
                format!(
                    "shows `{anchor}`, which `book::BOOK_FIXTURE_ANCHORS` allows to be a fixture \
                     rather than its own test, and which declares no item at all. An anchor \
                     shrunk to commentary publishes prose as a tested sample"
                ),
            ));
        }
    } else if let Some(at) = at {
        if at < declared.start || at > declared.end {
            violations.push(Violation::new(
                rule,
                chapter,
                format!(
                    "shows `{anchor}` from `{path}`, whose `#[test] fn {anchor}` is outside the \
                     anchor, so the bytes on the page are not the bytes that run. Either move \
                     the anchor around the test, or name it in `book::BOOK_FIXTURE_ANCHORS`"
                ),
            ));
        }
    }

    violations
}

/// No chapter carries a sample of its own: not in a fence this rule cannot read the
/// language of, and not as an indented block with no fence at all.
fn check_chapter_fences(inputs: &BookInputs, rule: &'static str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for chapter in BOOK_CHAPTERS {
        let Some(page) = inputs.page(chapter.file) else {
            continue;
        };
        for (info, body) in fenced_blocks(page) {
            if !is_quotable(&info) && !is_only_includes(&body) {
                let named = if info.is_empty() {
                    "an unlabelled".to_owned()
                } else {
                    format!("a ```{info}")
                };
                violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "carries {named} fence of its own; a quoted sample compiles nowhere and \
                         runs nowhere, and issue #42 asks for samples that are tested. Put it in \
                         a `book::BOOK_SAMPLE_FILES` file and show it with `{INCLUDE_DIRECTIVE}}}}}`, \
                         or label the fence with a language `book::QUOTABLE_FENCE_LANGUAGES` names"
                    ),
                ));
            }
        }
        for (at, line) in unfenced_lines(page) {
            let trimmed = line.trim();
            if trimmed.contains(INCLUDE_DIRECTIVE) && !is_one_include(trimmed) {
                violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "line {} carries an `{INCLUDE_DIRECTIVE}}}}}` and something else. \
                         `starts_with` and `ends_with` both hold for a directive with source \
                         between two of them, and everything between renders",
                        at + 1
                    ),
                ));
            }
            if line.starts_with("    ") || line.starts_with('\t') {
                violations.push(Violation::new(
                    rule,
                    chapter.id,
                    format!(
                        "indents line {} by four spaces, which Markdown renders as a code block \
                         with no fence and no language — a quoted sample this rule cannot see \
                         the language of",
                        at + 1
                    ),
                ));
            }
        }
    }

    violations
}

/// The three chapters whose contents another table owns.
fn check_chapter_contents(inputs: &BookInputs, rule: &'static str) -> Vec<Violation> {
    let mut violations = check_chapter_fences(inputs, rule);

    let (wire_chapter, wire_document) = WIRE_FORMAT_CHAPTER;
    if let Some(page) = BOOK_CHAPTERS
        .iter()
        .find(|chapter| chapter.id == wire_chapter)
        .and_then(|chapter| inputs.page(chapter.file))
    {
        let includes_it = includes(page).iter().any(|include| {
            include.path.as_deref() == Some(wire_document) && include.part == IncludePart::Whole
        });
        if !includes_it {
            violations.push(Violation::new(
                rule,
                wire_chapter,
                format!(
                    "does not include `{wire_document}`; a second copy of the frozen format \
                     passes every rule that reads the first and says something else"
                ),
            ));
        }
        if !table_rows(page).is_empty() {
            violations.push(Violation::new(
                rule,
                wire_chapter,
                format!(
                    "states a table of its own beside the include of `{wire_document}`; the \
                     format's tables are that document's, and a second copy of one is the thing \
                     the include exists to prevent"
                ),
            ));
        }
    }

    if let Some(page) = BOOK_CHAPTERS
        .iter()
        .find(|chapter| chapter.id == "failure-semantics")
        .and_then(|chapter| inputs.page(chapter.file))
    {
        for row in crate::docs::FAILURE_ROWS {
            if !a_line_carries(page, &[row.id, row.failure_point]) {
                violations.push(Violation::new(
                    rule,
                    "failure-semantics",
                    format!(
                        "states no row for `{}` beside `{}`; design document §14's table is ten \
                         rows and a book that shows nine is a book that promises less than the \
                         engine does",
                        row.id, row.failure_point
                    ),
                ));
            }
        }
    }

    if let Some(page) = BOOK_CHAPTERS
        .iter()
        .find(|chapter| chapter.id == "not-promised")
        .and_then(|chapter| inputs.page(chapter.file))
    {
        for goal in NON_GOALS {
            if !a_line_carries(page, &[goal.id, goal.headline]) {
                violations.push(Violation::new(
                    rule,
                    "not-promised",
                    format!(
                        "states no line carrying `{}` and `{}`; a non-goal a reader finds out \
                         about from a fleet is a non-goal they found out about too late",
                        goal.id, goal.headline
                    ),
                ));
            }
        }
    }

    violations
}

/// Rule: the hardware compatibility matrix covers every part, and claims nothing.
#[must_use]
pub fn check_hardware_matrix(inputs: &BookInputs) -> Vec<Violation> {
    const RULE: &str = "hardware-matrix";
    let mut violations = check_matrix_covers_every_part(RULE);

    let measured = match &inputs.wear {
        Ok(measured) => measured.as_slice(),
        Err(error) => {
            violations.push(Violation::new(
                RULE,
                "write amplification",
                format!(
                    "could not be measured ({error}); a measurement that did not happen is not a \
                     measurement that passed, and a matrix with a blank column is a matrix that \
                     published nothing"
                ),
            ));
            return violations;
        }
    };

    let chapter = BOOK_CHAPTERS
        .iter()
        .find(|chapter| chapter.id == "hardware-matrix")
        .and_then(|chapter| inputs.page(chapter.file));
    let Some(chapter) = chapter else {
        violations.push(Violation::new(
            RULE,
            "hardware-matrix",
            "the matrix chapter could not be read, so the matrix is published nowhere",
        ));
        return violations;
    };
    // Comments render as nothing, so honest cells hidden in one satisfy a reader of the
    // source and no reader of the page.
    let chapter = without_html_comments(chapter);

    violations.extend(check_matrix_table(RULE, &chapter, measured));

    if !HARDWARE_MATRIX
        .iter()
        .any(|row| render_power_cut(row) == PASSED)
        && chapter.contains(PASSED)
    {
        violations.push(Violation::new(
            RULE,
            "hardware-matrix",
            format!(
                "says `{PASSED}` while no row of `book::HARDWARE_MATRIX` renders it. Every board \
                 is `Not run` in `docs::HARDWARE_TARGETS`, and a chapter that says otherwise in \
                 prose is a chapter that says otherwise"
            ),
        ));
    }

    violations
}

/// The matrix table and the two tables it draws its parts from name the same set.
fn check_matrix_covers_every_part(rule: &'static str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for target in HARDWARE_TARGETS {
        if !HARDWARE_MATRIX
            .iter()
            .any(|row| row.part == MatrixPart::Board(target.id))
        {
            violations.push(Violation::new(
                rule,
                target.id,
                "is a board `docs::HARDWARE_TARGETS` names and the matrix has no row for it",
            ));
        }
    }
    for (part, ..) in PARTS {
        if !HARDWARE_MATRIX
            .iter()
            .any(|row| row.part == MatrixPart::Model(part))
        {
            violations.push(Violation::new(
                rule,
                *part,
                "is a part `wear::PARTS` measures and the matrix has no row for it",
            ));
        }
    }
    for row in HARDWARE_MATRIX {
        match row.part {
            MatrixPart::Board(id) if !HARDWARE_TARGETS.iter().any(|target| target.id == id) => {
                violations.push(Violation::new(
                    rule,
                    row.id,
                    format!("names the board `{id}`, which `docs::HARDWARE_TARGETS` does not"),
                ));
            }
            MatrixPart::Model(name) if !PARTS.iter().any(|(part, ..)| *part == name) => {
                violations.push(Violation::new(
                    rule,
                    row.id,
                    format!("names the part `{name}`, which `wear::PARTS` does not measure"),
                ));
            }
            _ => {}
        }
    }
    violations
}

/// The chapter's one table is the header, its rule, and every derived row in order.
///
/// Whole rows compared cell by cell rather than a search for each cell somewhere in the
/// page. Review of this change fabricated a table of two boards that do not exist, both
/// `Passed`, hid the honest rows in HTML comments, and watched a substring version stay
/// green — and separately took a wear figure from `63.37` to `163.37`, which no `contains`
/// can see.
fn check_matrix_table(rule: &'static str, chapter: &str, measured: &[PartWear]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut expected: Vec<Vec<String>> = vec![
        MATRIX_TABLE_HEADER
            .iter()
            .map(|cell| (*cell).to_owned())
            .collect(),
    ];
    for row in HARDWARE_MATRIX {
        match derived_cells(row, measured) {
            Ok(cells) => expected.push(cells),
            Err(why) => violations.push(Violation::new(
                rule,
                row.id,
                format!("has no cell to publish: {why}"),
            )),
        }
    }
    if !violations.is_empty() {
        return violations;
    }

    let found: Vec<Vec<String>> = table_rows(chapter)
        .into_iter()
        .filter(|row| !is_separator(row))
        .collect();
    if found == expected {
        return violations;
    }

    for (at, row) in expected.iter().enumerate() {
        match found.get(at) {
            Some(actual) if actual == row => {}
            Some(actual) => violations.push(Violation::new(
                rule,
                row.first().map_or("hardware-matrix", String::as_str),
                format!(
                    "row {} of the matrix chapter is {actual:?} and every derived cell says \
                     {row:?}. Each cell is read from a table or measured on this run, so a row \
                     that states another is a row that is wrong",
                    at + 1
                ),
            )),
            None => violations.push(Violation::new(
                rule,
                row.first().map_or("hardware-matrix", String::as_str),
                format!("row {} of the matrix chapter is missing: {row:?}", at + 1),
            )),
        }
    }
    for (at, row) in found.iter().enumerate().skip(expected.len()) {
        violations.push(Violation::new(
            rule,
            row.first().map_or("hardware-matrix", String::as_str),
            format!(
                "row {} of the matrix chapter is {row:?}, which `book::HARDWARE_MATRIX` does not \
                 declare, so nothing derives its cells",
                at + 1
            ),
        ));
    }
    violations
}

/// Where mdBook writes the rendered book, relative to the workspace root.
///
/// Under `target/` rather than beside the sources, so that the rendered HTML is already
/// ignored by the same line that ignores every other build product.
pub const BOOK_BUILD_DIR: &str = "target/book";

/// The renderer.
pub const MDBOOK_BINARY: &str = "mdbook";

/// Why the book could not be rendered.
///
/// Distinct from a [`Violation`], for the reason `CheckError` is: a violation means the book
/// is wrong, this means the render did not happen.
#[derive(Debug)]
pub struct BookError {
    message: String,
}

impl BookError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl core::fmt::Display for BookError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BookError {}

/// The longest identifier in an anchor's body, as a token to look for in the rendered page.
///
/// Identifier-shaped so that no HTML escaping is involved: `<`, `>` and `&` are the three
/// characters a renderer rewrites, and a token made of letters, digits and underscores has
/// none of them.
fn anchor_witness(body: &str) -> Option<String> {
    body.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| token.len() >= 4)
        .max_by_key(|token| token.len())
        .map(str::to_owned)
}

/// Whether mdBook reported an error while still exiting zero.
///
/// It does exactly that for an `{{#include}}` whose file is missing: the page renders with
/// the unresolved directive in it and the process succeeds. Without this the `book` stage
/// would be a stage that passes on a book with a hole in it.
fn mdbook_reported_an_error(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim_start().starts_with("ERROR"))
}

/// Everything wrong with what the renderer produced, in the order it was looked for.
///
/// Pure, so that the checks can be tested against rendered output no renderer produced.
///
/// # What it cannot see
///
/// Whether the *right* sample landed. A missing anchor renders as nothing at all — mdBook
/// reports no error and exits zero — so this looks for one identifier out of each anchor's
/// body in the page that shows it. A body whose longest identifier also appears in the
/// chapter's prose would satisfy it without the sample being there; the tie that matters is
/// `check_book`'s, which resolves every anchor against the file it is taken from.
#[must_use]
pub fn verify_render(inputs: &BookInputs, rendered: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for chapter in BOOK_CHAPTERS {
        let page = chapter.file.trim_end_matches(".md");
        let Some((_, html)) = rendered
            .iter()
            .find(|(name, _)| name.trim_end_matches(".html") == page)
        else {
            problems.push(format!(
                "{BOOK_BUILD_DIR}/{page}.html was not written, so the `{}` chapter is in no \
                 rendered book",
                chapter.id
            ));
            continue;
        };
        if html.trim().is_empty() {
            problems.push(format!("{BOOK_BUILD_DIR}/{page}.html is empty"));
            continue;
        }

        let Some(source) = inputs.page(chapter.file) else {
            continue;
        };
        for include in includes(source) {
            let IncludePart::Anchor(anchor) = &include.part else {
                continue;
            };
            let sample = include
                .path
                .as_deref()
                .and_then(|path| inputs.samples.iter().find(|(name, _)| name == path));
            let witness = sample.and_then(|(_, sample)| {
                anchors(sample)
                    .into_iter()
                    .find(|found| found.name == *anchor)
                    .and_then(|found| anchor_witness(&found.body))
            });
            // A missing anchor is the case this whole check exists for, so it may not be
            // the case it skips: mdBook renders one as nothing at all and exits zero, and
            // an earlier version of this function looked the witness up *in the anchor* and
            // so had nothing to look for exactly when there was nothing on the page.
            let Some(witness) = witness else {
                problems.push(format!(
                    "{BOOK_BUILD_DIR}/{page}.html shows `{anchor}`, and no witness for it could \
                     be read out of the sample file; mdBook renders an anchor it cannot find as \
                     nothing at all, reports no error, and exits zero"
                ));
                continue;
            };
            if !html.contains(&witness) {
                problems.push(format!(
                    "{BOOK_BUILD_DIR}/{page}.html shows nothing of the sample `{anchor}`; mdBook \
                     renders an anchor it cannot find as nothing at all, reports no error, and \
                     exits zero"
                ));
            }
        }
    }
    problems
}

/// Renders the book with mdBook, and fails closed on everything mdBook does not.
///
/// # Errors
///
/// [`BookError`] when the renderer is not installed, when it fails, when it reports an error
/// while exiting zero, or when what it produced does not carry every chapter and every
/// sample.
pub fn render(root: &std::path::Path) -> Result<String, BookError> {
    let output = std::process::Command::new(MDBOOK_BINARY)
        .arg("build")
        .arg(root.join(BOOK_DIR))
        .output()
        .map_err(|error| {
            BookError::new(format!(
                "could not run `{MDBOOK_BINARY}` ({error}); the book stage renders the book with \
                 it, and a stage that skipped a missing tool would be a stage that passes \
                 without building anything. Install it with `cargo install mdbook --locked`"
            ))
        })?;

    let reported = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(BookError::new(format!(
            "`{MDBOOK_BINARY} build` failed:\n{reported}"
        )));
    }
    if mdbook_reported_an_error(&reported) {
        return Err(BookError::new(format!(
            "`{MDBOOK_BINARY} build` reported an error and exited zero, which is what it does \
             for an unresolvable include:\n{reported}"
        )));
    }

    let build_dir = root.join(BOOK_BUILD_DIR);
    let mut pages = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&build_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|kind| kind == "html") {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let contents = std::fs::read_to_string(&path).unwrap_or_default();
                pages.push((name, contents));
            }
        }
    }

    // The same collector the gate uses, so the two readers cannot disagree about which
    // files the book has.
    let inputs = collect(root);

    let problems = verify_render(&inputs, &pages);
    if !problems.is_empty() {
        let mut report = String::from("the rendered book is incomplete:\n");
        for problem in &problems {
            let _ = writeln!(report, "  {problem}");
        }
        return Err(BookError::new(report));
    }

    Ok(format!(
        "book: ok ({} chapters rendered to {BOOK_BUILD_DIR})\n",
        BOOK_CHAPTERS.len()
    ))
}

/// Everything the two rules read, collected off disk.
///
/// Lossy rather than fallible on a file that is not UTF-8: a binary under the book's source
/// is a file `check_chapters` reports by name, and failing the whole gate with an encoding
/// error would report nothing else.
#[must_use]
pub fn collect(root: &std::path::Path) -> BookInputs {
    let source_dir = root.join(BOOK_SOURCE_DIR);
    let pages = book_files(&source_dir)
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&source_dir)
                .unwrap_or(&path)
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let bytes = std::fs::read(&path).unwrap_or_default();
            (name, String::from_utf8_lossy(&bytes).into_owned())
        })
        .collect();

    BookInputs {
        manifest: std::fs::read_to_string(root.join(BOOK_MANIFEST)).ok(),
        pages,
        samples: BOOK_SAMPLE_FILES
            .iter()
            .filter_map(|path| {
                std::fs::read_to_string(root.join(path))
                    .ok()
                    .map(|contents| ((*path).to_owned(), contents))
            })
            .collect(),
        documents: BOOK_DOCUMENT_INCLUDES
            .iter()
            .filter(|path| root.join(path).is_file())
            .map(|path| (*path).to_owned())
            .collect(),
        wear: crate::wear::measure().map_err(|error| error.to_string()),
    }
}

/// Every file under `directory`, in a stable order.
///
/// Every file rather than every `.md` file, because a chapter named `rogue.MD` is a chapter
/// mdBook renders and a case-sensitive filter leaves covered by nothing. Bounded in depth
/// rather than guarded by a visited set: a symlink cycle under the book's source would
/// otherwise walk for ever, and no book needs eight levels of directory.
fn book_files(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    const MAX_DEPTH: usize = 8;
    let mut found = Vec::new();
    let mut pending = vec![(directory.to_path_buf(), 0_usize)];
    while let Some((next, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Fixtures describing a book that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use core::fmt::Write as _;

    use super::{
        BOOK_CHAPTERS, BOOK_DIR, BOOK_SAMPLE_FILES, BOOK_SUMMARY, BookInputs, HARDWARE_MATRIX,
        MATRIX_TABLE_HEADER, NON_GOALS, derived_cells,
    };
    use crate::docs::FAILURE_ROWS;
    use crate::wear::PartWear;

    /// The write amplification, measured exactly as the gate measures it.
    ///
    /// # Panics
    ///
    /// If a modelled part cannot be measured, which is the failure `hardware-matrix`
    /// reports on a real run and which a fixture has no way to work around.
    #[must_use]
    pub fn measured() -> Vec<PartWear> {
        crate::wear::measure().expect("the modelled parts should be measurable")
    }

    /// A sample file whose anchors are named after the tests it declares.
    #[must_use]
    pub fn clean_sample_file() -> String {
        let mut file = String::from("//! Samples.\n\n");
        for anchor in ["a_first_sample", "a_second_sample"] {
            let _ = writeln!(file, "// ANCHOR: {anchor}");
            let _ = write!(file, "#[test]\nfn {anchor}() {{\n    assert!(true);\n}}\n");
            let _ = write!(file, "// ANCHOR_END: {anchor}\n\n");
        }
        file
    }

    /// The failure chapter, with every row of design document §14's table.
    #[must_use]
    pub fn clean_failure_chapter() -> String {
        let mut chapter =
            String::from("# Failure semantics\n\n| Id | Failure point |\n| --- | --- |\n");
        for row in FAILURE_ROWS {
            let _ = writeln!(chapter, "| `{}` | {} |", row.id, row.failure_point);
        }
        chapter
    }

    /// The non-goals chapter, with every promise Waymaker declines to make.
    #[must_use]
    pub fn clean_non_goals_chapter() -> String {
        let mut chapter = String::from("# What is not promised\n\n");
        for goal in NON_GOALS {
            let _ = writeln!(chapter, "- `{}` — {}", goal.id, goal.headline);
        }
        chapter
    }

    /// The matrix chapter, rendered from the same derivations the rule reads.
    #[must_use]
    pub fn clean_matrix_chapter(measured: &[PartWear]) -> String {
        let mut chapter = String::from("# The hardware compatibility matrix\n\n");
        let _ = writeln!(chapter, "| {} |", MATRIX_TABLE_HEADER.join(" | "));
        let _ = writeln!(chapter, "|{}", " --- |".repeat(MATRIX_TABLE_HEADER.len()));
        for row in HARDWARE_MATRIX {
            let Ok(cells) = derived_cells(row, measured) else {
                continue;
            };
            let _ = writeln!(chapter, "| {} |", cells.join(" | "));
        }
        chapter
    }

    /// A chapter that shows one sample.
    #[must_use]
    pub fn clean_sample_chapter(title: &str, anchor: &str) -> String {
        let mut chapter = format!("# {title}\n\n```rust,ignore\n");
        let _ = writeln!(
            chapter,
            "{{{{#include ../../../{}:{anchor}}}}}",
            BOOK_SAMPLE_FILES[0]
        );
        chapter.push_str("```\n");
        chapter
    }

    /// A book every rule in this module accepts.
    #[must_use]
    pub fn clean_book() -> BookInputs {
        let mut summary = String::from("# Summary\n\n");
        for chapter in BOOK_CHAPTERS {
            let _ = writeln!(summary, "- [{}]({})", chapter.title, chapter.file);
        }

        let measured = measured();
        let mut pages = vec![(BOOK_SUMMARY.to_owned(), summary)];
        for chapter in BOOK_CHAPTERS {
            let body = match chapter.id {
                "design-centre" => clean_sample_chapter("The design centre", "a_first_sample"),
                "determinism" => {
                    clean_sample_chapter("The determinism contract", "a_second_sample")
                }
                "failure-semantics" => clean_failure_chapter(),
                "wire-format" => String::from(
                    "# The wire format\n\n{{#include ../../format/wire-format-v1.md}}\n",
                ),
                "not-promised" => clean_non_goals_chapter(),
                "hardware-matrix" => clean_matrix_chapter(&measured),
                other => format!("# {other}\n\nProse.\n"),
            };
            pages.push((chapter.file.to_owned(), body));
        }

        BookInputs {
            manifest: Some("[book]\ntitle = \"Waymaker\"\nsrc = \"src\"\n".to_owned()),
            pages,
            samples: vec![(BOOK_SAMPLE_FILES[0].to_owned(), clean_sample_file())],
            documents: vec!["docs/format/wire-format-v1.md".to_owned()],
            wear: Ok(measured),
        }
    }

    /// A line pointing a reader at the book, for the two documents that must carry one.
    #[must_use]
    pub fn book_link() -> String {
        format!("The book: [{BOOK_DIR}]({BOOK_DIR}/src/{BOOK_SUMMARY})\n")
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;

    use super::tests_support::{book_link, clean_book};
    use super::{
        BOOK_CHAPTERS, BOOK_FIXTURE_ANCHORS, BOOK_MANIFEST, BOOK_SOURCE_DIR, BOOK_SUMMARY,
        BookInputs, HARDWARE_MATRIX, MatrixPart, NON_GOALS, NOT_MEASURED, check_book,
        check_hardware_matrix, render_geometry, render_power_cut, render_write_amplification,
    };
    use crate::Violation;
    use crate::docs::{FAILURE_ROWS, HARDWARE_TARGETS};
    use crate::wear::PARTS;

    /// The rule ids these tests are about.
    const BOOK: &str = "book";
    const MATRIX: &str = "hardware-matrix";

    fn fired(violations: &[Violation], rule: &str) -> bool {
        violations.iter().any(|violation| violation.rule == rule)
    }

    fn render(violations: &[Violation]) -> String {
        let mut report = String::new();
        for violation in violations {
            let _ = writeln!(report, "  {violation}");
        }
        report
    }

    fn good_book() -> BookInputs {
        clean_book()
    }

    fn claude_md() -> String {
        book_link()
    }

    fn readme() -> String {
        book_link()
    }

    fn check(inputs: &BookInputs) -> Vec<Violation> {
        check_book(inputs, Some(&claude_md()), Some(&readme()))
    }

    #[test]
    fn a_book_that_agrees_with_every_table_is_accepted() {
        let inputs = good_book();
        let violations = check(&inputs);
        assert!(
            violations.is_empty(),
            "the fixture book should pass:\n{}",
            render(&violations)
        );
        let matrix = check_hardware_matrix(&inputs);
        assert!(
            matrix.is_empty(),
            "the fixture matrix should pass:\n{}",
            render(&matrix)
        );
    }

    #[test]
    fn a_missing_book_is_reported_rather_than_skipped() {
        let violations = check_book(&BookInputs::absent(), None, None);
        assert!(
            fired(&violations, BOOK),
            "a book that is not there is not a book that passed"
        );
        assert!(fired(&check_hardware_matrix(&BookInputs::absent()), MATRIX));
    }

    #[test]
    fn a_chapter_the_summary_does_not_link_is_unreachable() {
        let mut inputs = good_book();
        let dropped = BOOK_CHAPTERS[2];
        let summary = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_SUMMARY)
            .expect("the fixture has a summary");
        summary.1 = summary
            .1
            .lines()
            .filter(|line| !line.contains(dropped.file))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            fired(&check(&inputs), BOOK),
            "a chapter no table of contents links is a chapter nobody reads"
        );
    }

    #[test]
    fn a_summary_link_to_a_chapter_the_table_does_not_declare_is_reported() {
        let mut inputs = good_book();
        let summary = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_SUMMARY)
            .expect("the fixture has a summary");
        summary.1.push_str("- [Improvised](improvised.md)\n");
        inputs
            .pages
            .push(("improvised.md".to_owned(), "# Improvised\n".to_owned()));
        assert!(
            fired(&check(&inputs), BOOK),
            "a chapter the table does not declare is a chapter no rule covers"
        );
    }

    #[test]
    fn a_missing_chapter_file_is_reported() {
        let mut inputs = good_book();
        let dropped = BOOK_CHAPTERS[4].file;
        inputs.pages.retain(|(name, _)| name != dropped);
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_quoted_rust_sample_is_refused() {
        // The whole of issue #42's "tested, not merely quoted". A fence a chapter carries
        // itself is text: it compiles nowhere, runs nowhere, and goes on being wrong.
        for fence in ["```rust", "```rust,no_run", "``` rust", "```rs"] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
                .expect("the fixture has the first chapter");
            let _ = write!(chapter.1, "\n{fence}\nfn main() {{}}\n```\n");
            assert!(
                fired(&check(&inputs), BOOK),
                "a `{fence}` fence in a chapter is a sample nothing runs"
            );
        }
    }

    #[test]
    fn a_rust_fence_carrying_nothing_but_an_include_is_allowed() {
        // A Rust fence is how mdBook renders an included sample as code, so the ban is on
        // a fence that carries source rather than on the fence. The fixture's chapters
        // already show their samples that way, in every info-string spelling.
        for info in ["rust,ignore", "rust", "rs"] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
                .expect("the fixture has the first chapter");
            chapter.1 = chapter.1.replace("```rust,ignore", &format!("```{info}"));
            let violations = check(&inputs);
            assert!(
                !fired(&violations, BOOK),
                "```{info}: {}",
                render(&violations)
            );
        }
    }

    #[test]
    fn a_rust_fence_carrying_a_line_of_source_beside_an_include_is_refused() {
        // The hole a laxer rule would leave: one directive, and a hand-written line under
        // it that nothing compiles.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1 = chapter.1.replace("```\n", "let x = 1;\n```\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn an_empty_rust_fence_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1.push_str("\n```rust\n```\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_fence_that_is_not_rust_is_allowed() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1.push_str("\n```sh\ncargo test\n```\n");
        assert!(
            !fired(&check(&inputs), BOOK),
            "a shell block is not a Rust sample"
        );
    }

    #[test]
    fn an_include_of_a_file_no_stage_runs_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1 = chapter.1.replace(
            "crates/waymaker-drive/tests/book.rs",
            "crates/waymaker-drive/src/ota.rs",
        );
        assert!(
            fired(&check(&inputs), BOOK),
            "a sample taken from a file no test runs is a sample nothing runs"
        );
    }

    #[test]
    fn an_include_of_an_anchor_the_sample_file_does_not_declare_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1 = chapter.1.replace("a_first_sample", "a_sample_nobody_wrote");
        assert!(
            fired(&check(&inputs), BOOK),
            "mdBook renders a missing anchor as an error message inside the page rather than failing"
        );
    }

    #[test]
    fn an_anchor_that_is_not_a_test_is_refused() {
        // The tie is the whole claim. An anchor whose name is not a `#[test]` is a
        // fragment the book shows and no stage runs.
        let mut inputs = good_book();
        inputs.samples[0].1 = inputs.samples[0]
            .1
            .replace("#[test]\nfn a_first_sample", "fn a_first_sample");
        assert!(
            fired(&check(&inputs), BOOK),
            "an anchor with no test of its name is a sample nothing runs"
        );
    }

    #[test]
    fn a_sample_anchor_no_chapter_shows_is_refused() {
        // The other direction: a sample nobody quotes is a test pretending to be
        // documentation.
        let mut inputs = good_book();
        inputs.samples[0].1.push_str(
            "// ANCHOR: a_sample_nobody_shows\n#[test]\nfn a_sample_nobody_shows() {}\n// ANCHOR_END: a_sample_nobody_shows\n",
        );
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_line_ranged_include_is_refused() {
        // mdBook can include by line number. A line range is a citation that goes wrong
        // silently the first time somebody adds a line above it.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1 = chapter.1.replace(":a_first_sample", ":10:20");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_wire_format_chapter_that_restates_the_format_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "wire-format.md")
            .expect("the fixture has the wire-format chapter");
        chapter.1 = "# The wire format\n\nThe header is eight bytes.\n".to_owned();
        assert!(
            fired(&check(&inputs), BOOK),
            "a second copy of the frozen format passes every rule that reads the first"
        );
    }

    #[test]
    fn a_failure_row_the_chapter_omits_is_reported() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "failure-semantics.md")
            .expect("the fixture has the failure chapter");
        chapter.1 = chapter
            .1
            .lines()
            .filter(|line| !line.contains(FAILURE_ROWS[3].id))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_non_goal_the_chapter_omits_is_reported() {
        for goal in NON_GOALS {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == "not-promised.md")
                .expect("the fixture has the non-goals chapter");
            chapter.1 = chapter
                .1
                .lines()
                .filter(|line| !line.contains(goal.id))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                fired(&check(&inputs), BOOK),
                "dropping `{}` from the chapter must be reported",
                goal.id
            );
        }
    }

    #[test]
    fn a_repository_that_does_not_point_at_the_book_is_reported() {
        let inputs = good_book();
        assert!(
            fired(
                &check_book(&inputs, Some("no link here"), Some(&readme())),
                BOOK
            ),
            "a book nothing links is a book nobody finds"
        );
        assert!(fired(
            &check_book(&inputs, Some(&claude_md()), Some("no link here")),
            BOOK
        ));
    }

    #[test]
    fn a_manifest_without_a_title_is_reported() {
        let mut inputs = good_book();
        inputs.manifest = Some("[book]\nsrc = \"src\"\n".to_owned());
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn every_hardware_target_and_every_modelled_part_has_a_matrix_row() {
        for target in HARDWARE_TARGETS {
            assert!(
                HARDWARE_MATRIX
                    .iter()
                    .any(|row| row.part == MatrixPart::Board(target.id)),
                "the matrix has no row for `{}`",
                target.id
            );
        }
        for (part, ..) in PARTS {
            assert!(
                HARDWARE_MATRIX
                    .iter()
                    .any(|row| row.part == MatrixPart::Model(part)),
                "the matrix has no row for `{part}`"
            );
        }
        assert_eq!(HARDWARE_MATRIX.len(), HARDWARE_TARGETS.len() + PARTS.len());
    }

    #[test]
    fn a_matrix_row_the_chapter_omits_is_reported() {
        for row in HARDWARE_MATRIX {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == "hardware-matrix.md")
                .expect("the fixture has the matrix chapter");
            chapter.1 = chapter
                .1
                .lines()
                .filter(|line| !line.contains(row.id))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                fired(&check_hardware_matrix(&inputs), MATRIX),
                "dropping `{}` from the matrix must be reported",
                row.id
            );
        }
    }

    #[test]
    fn a_matrix_that_claims_a_board_passed_is_refused() {
        // The sharpest one. Every board row is `Not run`, and the chapter may not say
        // otherwise: flipping the claim needs an accepted ADR, which is
        // `hardware-attestation`'s, and this is what stops the *book* saying it anyway.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "hardware-matrix.md")
            .expect("the fixture has the matrix chapter");
        chapter.1 = chapter.1.replace("Not run", "Passed");
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_write_amplification_figure_that_drifted_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "hardware-matrix.md")
            .expect("the fixture has the matrix chapter");
        chapter.1 = chapter.1.replace("63.37", "12.00");
        assert!(
            fired(&check_hardware_matrix(&inputs), MATRIX),
            "a published figure that no longer matches the measurement is a published figure that is wrong"
        );
    }

    #[test]
    fn a_geometry_that_drifted_from_the_measured_part_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "hardware-matrix.md")
            .expect("the fixture has the matrix chapter");
        chapter.1 = chapter.1.replace("4096 / 16 / 1", "4096 / 32 / 1");
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_measurement_that_could_not_be_taken_is_a_failure_rather_than_a_blank_column() {
        let mut inputs = good_book();
        inputs.wear = Err("the part cannot be laid out".to_owned());
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_board_row_reports_no_geometry_and_no_wear() {
        for row in HARDWARE_MATRIX {
            let MatrixPart::Board(_) = row.part else {
                continue;
            };
            assert_eq!(render_geometry(row), NOT_MEASURED);
            assert_eq!(
                render_write_amplification(row, &[]),
                Ok(NOT_MEASURED.to_owned())
            );
            assert_eq!(render_power_cut(row), "Not run");
        }
    }

    #[test]
    fn a_renderer_that_reported_an_error_and_exited_zero_is_a_failure() {
        // mdBook does exactly this for an `{{#include}}` whose file is missing: the page
        // renders with the directive still in it and the process succeeds.
        assert!(super::mdbook_reported_an_error(
            " INFO Book building has started\nERROR Error updating \"{{#include ../x.rs}}\"\n"
        ));
        assert!(!super::mdbook_reported_an_error(
            " INFO Book building has started\n INFO HTML book written\n"
        ));
    }

    #[test]
    fn a_chapter_the_renderer_did_not_write_is_reported() {
        let inputs = clean_book();
        let problems = super::verify_render(&inputs, &[]);
        assert_eq!(problems.len(), BOOK_CHAPTERS.len(), "{problems:?}");
    }

    #[test]
    fn a_rendered_page_that_shows_none_of_its_sample_is_reported() {
        // The other half mdBook does not fail on: an anchor it cannot find renders as
        // nothing at all, with no error and a zero exit.
        let inputs = clean_book();
        let rendered: Vec<(String, String)> = BOOK_CHAPTERS
            .iter()
            .map(|chapter| {
                (
                    chapter.file.replace(".md", ".html"),
                    format!("<h1>{}</h1>", chapter.title),
                )
            })
            .collect();
        let problems = super::verify_render(&inputs, &rendered);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("a_first_sample")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_rendered_page_that_shows_its_sample_is_accepted() {
        let inputs = clean_book();
        let rendered: Vec<(String, String)> = BOOK_CHAPTERS
            .iter()
            .map(|chapter| {
                (
                    chapter.file.replace(".md", ".html"),
                    format!(
                        "<h1>{}</h1><pre>a_first_sample a_second_sample</pre>",
                        chapter.title
                    ),
                )
            })
            .collect();
        assert!(super::verify_render(&inputs, &rendered).is_empty());
    }

    #[test]
    fn an_anchor_witness_is_an_identifier_no_renderer_escapes() {
        let witness = super::anchor_witness("let x = &a_long_identifier < 3;")
            .expect("the body has an identifier");
        assert_eq!(witness, "a_long_identifier");
        assert!(super::anchor_witness("a + b").is_none());
    }

    #[test]
    fn an_unlabelled_fence_carrying_source_is_refused() {
        // Codex-style review of this change reached a bare fence past a version of this
        // rule that asked whether the info string said "rust". The ban is on a fence that
        // carries source, so the allowlist is of languages a chapter may quote.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1.push_str("\n```\nfn quoted() {}\n```\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_quotable_language_is_allowed_and_an_invented_one_is_not() {
        for (language, allowed) in [("sh", true), ("text", true), ("python", false)] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
                .expect("the fixture has the first chapter");
            let _ = write!(chapter.1, "\n```{language}\necho hello\n```\n");
            assert_eq!(
                !fired(&check(&inputs), BOOK),
                allowed,
                "a ```{language} fence"
            );
        }
    }

    #[test]
    fn source_beside_an_include_on_one_line_is_refused() {
        // `starts_with` and `ends_with` both hold for
        // `{{#include a}} let x = 1; {{#include b}}`, and everything between the two
        // renders. Review of this change ran exactly that.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1 = chapter.1.replace(
            "a_first_sample}}",
            "a_first_sample}} let stolen = 1; fn main() {}}",
        );
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_directive_that_is_not_an_include_is_refused() {
        // `{{#playground}}` renders an arbitrary source file as a Rust block with no fence
        // at all, and `{{#rustdoc_include}}` takes the line ranges the include rule
        // refuses. Both were reached past an earlier version of this module.
        for directive in [
            "{{#playground ../../../src/drive.rs}}",
            "{{#rustdoc_include ../x.rs:1:40}}",
        ] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
                .expect("the fixture has the first chapter");
            let _ = write!(chapter.1, "\n{directive}\n");
            assert!(
                fired(&check(&inputs), BOOK),
                "`{directive}` must be refused"
            );
        }
    }

    #[test]
    fn an_indented_code_block_is_refused() {
        // Four spaces is a code block with no fence and no language, which is a quoted
        // sample this rule could not otherwise see.
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        chapter.1.push_str("\n    let quoted = 1;\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_page_whose_extension_is_not_lowercase_is_still_covered() {
        // A `.MD` chapter is one mdBook renders and a case-sensitive collector leaves
        // covered by nothing.
        let mut inputs = good_book();
        inputs
            .pages
            .push(("rogue.MD".to_owned(), "# Production readiness\n".to_owned()));
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_test_that_does_not_run_is_not_a_test() {
        // CLAUDE.md already states this standard for `failure-matrix`. Attribute order is
        // free, and review of this change put `#[ignore]` *above* the anchor marker, where
        // a reader of the book never sees it.
        for attribute in [
            "#[ignore]",
            "#[ignore = \"why\"]",
            "#[cfg(feature = \"never\")]",
        ] {
            let mut inputs = good_book();
            inputs.samples[0].1 = inputs.samples[0].1.replace(
                "// ANCHOR: a_first_sample",
                &format!("{attribute}\n// ANCHOR: a_first_sample"),
            );
            assert!(
                fired(&check(&inputs), BOOK),
                "`{attribute}` above the anchor must be refused"
            );
        }
    }

    #[test]
    fn an_anchor_that_does_not_contain_its_own_test_is_refused() {
        // The load-bearing claim: the bytes on the page are the bytes that run. Review of
        // this change shrank an anchor to two comment lines advertising an API that does
        // not exist and watched a name-only tie stay green.
        let mut inputs = good_book();
        inputs.samples[0].1 = inputs.samples[0].1.replace(
            "// ANCHOR: a_first_sample\n#[test]",
            "// ANCHOR: a_first_sample\n// Waymaker guarantees exactly-once delivery.\n// ANCHOR_END: a_first_sample\n#[test]",
        );
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_fixture_anchor_shrunk_to_commentary_is_refused() {
        // The escape hatch for an anchor that shows a type rather than its own test is a
        // row somebody writes. It still may not be prose.
        let anchor = BOOK_FIXTURE_ANCHORS
            .first()
            .expect("the table names at least one fixture");
        let mut inputs = good_book();
        inputs.samples[0].1 = format!(
            "// ANCHOR: {anchor}\n// Just a sentence.\n// ANCHOR_END: {anchor}\n#[test]\nfn {anchor}() {{}}\n"
        );
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == BOOK_CHAPTERS[0].file)
            .expect("the fixture has the first chapter");
        *chapter = (
            chapter.0.clone(),
            super::tests_support::clean_sample_chapter("Fixture", anchor),
        );
        inputs
            .pages
            .retain(|(name, _)| name != BOOK_CHAPTERS[1].file);
        inputs.pages.push((
            BOOK_CHAPTERS[1].file.to_owned(),
            "# Prose only\n".to_owned(),
        ));
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn overlapping_anchors_are_both_seen() {
        // mdBook allows them, and a single open slot drops the outer one — which reports a
        // bogus "declares no such anchor" and blinds the orphan check at the same time.
        let found = super::anchors(
            "// ANCHOR: outer\nlet a = 1;\n// ANCHOR: inner\nlet b = 2;\n// ANCHOR_END: inner\nlet c = 3;\n// ANCHOR_END: outer\n",
        );
        let names: Vec<&str> = found.iter().map(|anchor| anchor.name.as_str()).collect();
        assert_eq!(names, ["inner", "outer"]);
    }

    #[test]
    fn a_repository_that_links_the_book_only_by_name_is_reported() {
        // `contains("docs/book")` is satisfied by "docs/book (deleted; see the archive)".
        let inputs = good_book();
        assert!(fired(
            &check_book(&inputs, Some("docs/book (deleted)"), Some(&readme())),
            BOOK
        ));
    }

    #[test]
    fn a_wire_format_chapter_that_states_a_table_of_its_own_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "wire-format.md")
            .expect("the fixture has the wire-format chapter");
        chapter
            .1
            .push_str("\n| Field | Bytes |\n| --- | --- |\n| header | 12 |\n");
        assert!(fired(&check(&inputs), BOOK));
    }

    #[test]
    fn a_fabricated_matrix_row_is_refused_however_it_is_spelled() {
        // Review of this change added rows whose first cell was bold, or bare, or a
        // duplicate of a declared id, and hid the honest rows in HTML comments. A
        // substring search saw none of it.
        for row in [
            "| **`nrf52840-dk`** | A board | 4096 / 4 / 1 | Passed | Backed RTC | 41.00 |",
            "| stm32f407 | A board | 4096 / 4 / 1 | Passed | Backed RTC | 39.00 |",
            "| `cortex-m0plus` | A Cortex-M0+ board (bench) | 4096 / 1 / 1 | Passed | Backed RTC | 63.37 |",
        ] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == "hardware-matrix.md")
                .expect("the fixture has the matrix chapter");
            let _ = writeln!(chapter.1, "{row}");
            assert!(
                fired(&check_hardware_matrix(&inputs), MATRIX),
                "the row `{row}` must be refused"
            );
        }
    }

    #[test]
    fn an_honest_row_hidden_in_a_comment_does_not_satisfy_the_matrix() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "hardware-matrix.md")
            .expect("the fixture has the matrix chapter");
        let honest = chapter.1.clone();
        chapter.1 = format!(
            "# The hardware compatibility matrix\n\n| Id | Part |\n| --- | --- |\n| nRF52840 | Passed |\n\n<!--\n{honest}\n-->\n"
        );
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_cell_that_merely_contains_the_derived_value_is_refused() {
        // `63.37` is a substring of `163.37`, and `Not run` of `Not run in the lab, Passed
        // on the bench`. Both were run against a version that asked only whether a line
        // contained each cell.
        for (from, to) in [
            ("| 63.37 |", "| 163.37 |"),
            ("| Not run |", "| Not run in the lab, Passed on the bench |"),
            ("| 4096 / 16 / 1 |", "| 14096 / 16 / 1 |"),
        ] {
            let mut inputs = good_book();
            let chapter = inputs
                .pages
                .iter_mut()
                .find(|(name, _)| name == "hardware-matrix.md")
                .expect("the fixture has the matrix chapter");
            assert!(
                chapter.1.contains(from),
                "the fixture should carry `{from}`"
            );
            chapter.1 = chapter.1.replace(from, to);
            assert!(
                fired(&check_hardware_matrix(&inputs), MATRIX),
                "`{from}` -> `{to}` must be refused"
            );
        }
    }

    #[test]
    fn prose_that_claims_a_board_passed_is_refused() {
        let mut inputs = good_book();
        let chapter = inputs
            .pages
            .iter_mut()
            .find(|(name, _)| name == "hardware-matrix.md")
            .expect("the fixture has the matrix chapter");
        chapter
            .1
            .push_str("\nAll three boards have Passed their power-cut loops.\n");
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_modelled_part_with_no_figure_is_a_failure_rather_than_a_blank_column() {
        // Folding "the measurement produced nothing" into the cell a board gets would
        // publish a degenerate run as a blank and leave the build green.
        let mut inputs = good_book();
        inputs.wear = Ok(Vec::new());
        assert!(fired(&check_hardware_matrix(&inputs), MATRIX));
    }

    #[test]
    fn a_rendered_page_missing_the_anchor_it_shows_is_reported() {
        // The case the whole `book` stage exists for: mdBook renders an anchor it cannot
        // find as nothing at all and exits zero. An earlier version looked the witness up
        // *inside* the anchor, so it had nothing to look for exactly when there was
        // nothing on the page.
        let mut inputs = clean_book();
        inputs.samples[0].1 = inputs.samples[0]
            .1
            .replace("a_first_sample", "a_renamed_sample");
        let rendered: Vec<(String, String)> = BOOK_CHAPTERS
            .iter()
            .map(|chapter| {
                (
                    chapter.file.replace(".md", ".html"),
                    format!("<h1>{}</h1><pre>a_second_sample</pre>", chapter.title),
                )
            })
            .collect();
        let problems = super::verify_render(&inputs, &rendered);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("a_first_sample")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_model_row_is_only_swept_at_the_program_unit_the_sweeps_use() {
        // Every crash sweep in this workspace lays the part out with a four-byte program
        // unit, so the other two modelled rows carry a measured figure and no sweep.
        for row in HARDWARE_MATRIX {
            let MatrixPart::Model(name) = row.part else {
                continue;
            };
            let program = PARTS
                .iter()
                .find(|(part, ..)| *part == name)
                .map(|(_, _, _, program, _)| *program)
                .expect("every model row names a measured part");
            let expected = if program == super::SWEPT_PROGRAM_BYTES {
                "Swept on the host model"
            } else {
                "Not swept at this program unit"
            };
            assert_eq!(render_power_cut(row), expected, "{name}");
        }
    }

    #[test]
    fn every_chapter_id_and_file_is_unique() {
        for (at, chapter) in BOOK_CHAPTERS.iter().enumerate() {
            assert!(
                !BOOK_CHAPTERS[..at]
                    .iter()
                    .any(|earlier| earlier.id == chapter.id || earlier.file == chapter.file),
                "`{}` is declared twice",
                chapter.id
            );
            assert!(
                std::path::Path::new(chapter.file)
                    .extension()
                    .is_some_and(|kind| kind == "md")
            );
            assert!(!chapter.covers.trim().is_empty());
        }
        assert!(BOOK_MANIFEST.starts_with(BOOK_SOURCE_DIR.trim_end_matches("/src")));
    }
}
