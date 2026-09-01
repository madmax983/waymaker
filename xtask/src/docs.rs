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
use crate::source::inner_attributes;

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

/// Crate-root attributes that turn an undocumented public item into a warning.
///
/// Normalised the same way `crate::source::inner_attributes` normalises what it reads, so
/// whitespace inside the attribute does not decide whether the rule passes.
pub const MISSING_DOCS_ATTRIBUTES: &[&str] = &[
    "#![warn(missing_docs)]",
    "#![deny(missing_docs)]",
    "#![forbid(missing_docs)]",
];

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
    "Write schedule seal",
    "Dispatch physical activity",
    "Write outcome frame",
    "Payload barrier",
    "Write outcome seal",
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

/// Every diagram issue #11 asks for.
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
    /// Every crate root in the workspace, in package-then-path order.
    pub crate_roots: Vec<CrateRoot>,
    /// Workspace members whose crate roots could not be located.
    ///
    /// Carried rather than dropped: a member that contributes no root would otherwise
    /// leave the `missing_docs` rule with nothing to say about it and the gate reporting
    /// success.
    pub members_without_roots: Vec<String>,
}

/// A labelled Mermaid block from a Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidBlock {
    /// The id from the `<!-- diagram: ... -->` label above the fence.
    pub id: String,
    /// The fence's contents, without the fence lines.
    pub body: String,
}

/// The number an ADR file name carries, or `None` if it is not a numbered ADR.
///
/// `0003-the-eight-settled-design-decisions.md` is 3. `README.md` is `None`, and so is
/// `003-short.md`: a four-digit prefix is what makes the files sort in decision order for
/// the rest of the record's life.
#[must_use]
// `.md`, lowercase, on purpose: the record's file names are a fixed shape, and treating
// `README.MD` as an ADR would be a bug rather than a courtesy.
#[allow(
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
/// The label rather than the nearest heading, because a heading is prose: rewording it is
/// a normal thing to do to a document and would silently detach the diagram from the rule
/// that checks it. Blank lines between the label and the fence are allowed; anything else
/// between them means the label belongs to something other than the fence, and the block
/// is not collected.
#[must_use]
pub fn mermaid_blocks(contents: &str) -> Vec<MermaidBlock> {
    let mut blocks = Vec::new();
    let mut pending_id: Option<String> = None;
    let mut collecting: Option<(String, Vec<&str>)> = None;

    for line in contents.lines() {
        let trimmed = line.trim();

        if let Some((id, body)) = collecting.as_mut() {
            if trimmed == "```" {
                blocks.push(MermaidBlock {
                    id: std::mem::take(id),
                    body: body.join("\n"),
                });
                collecting = None;
            } else {
                body.push(line);
            }
            continue;
        }

        if let Some(id) = diagram_label(trimmed) {
            pending_id = Some(id.to_owned());
            continue;
        }

        if trimmed == "```mermaid" {
            if let Some(id) = pending_id.take() {
                collecting = Some((id, Vec::new()));
            }
            continue;
        }

        if !trimmed.is_empty() {
            pending_id = None;
        }
    }

    blocks
}

/// The id in `<!-- diagram: some-id -->`, if `line` is such a label.
fn diagram_label(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    Some(inner.strip_prefix("diagram:")?.trim())
}

/// Rule: `CLAUDE.md` exists, quotes the layering table, and names every gate rule.
///
/// The "must not own" cells are compared against [`LAYERS`] verbatim. A contributor
/// reading `CLAUDE.md` is reading the same string the gate reads, or the gate says so.
#[must_use]
pub fn check_claude_md(contents: Option<&str>, rules: &[&str]) -> Vec<Violation> {
    let Some(contents) = contents else {
        return vec![Violation::new(
            "claude-md",
            CLAUDE_MD_PATH,
            "the repository has no CLAUDE.md; the invariants, the layering rules and the \
             must-not-own table live there",
        )];
    };

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
        if !contents.contains(rule) {
            violations.push(Violation::new(
                "claude-md",
                *rule,
                "CLAUDE.md does not name this gate rule, so a contributor cannot tell in \
                 advance what the gate will reject",
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

/// Rule: the ADR record is numbered without gaps or duplicates, and has its template.
#[must_use]
pub fn check_adr_numbering(adrs: &[AdrFile]) -> Vec<Violation> {
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

    if !by_number.contains_key(&0) {
        violations.push(Violation::new(
            "adr-numbering",
            ADR_TEMPLATE,
            "the record has no template; a new ADR is then written from whichever one the \
             author happened to open",
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
pub fn check_adr_structure(adrs: &[AdrFile]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for adr in adrs {
        if !adr.contents.lines().any(|line| line.starts_with("# ")) {
            violations.push(Violation::new(
                "adr-structure",
                adr.name.clone(),
                "has no `# ` title line",
            ));
        }

        for field in ADR_REQUIRED_FIELDS {
            if !adr
                .contents
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

        if let Some(status) = adr_status(&adr.contents) {
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
            if !adr.contents.lines().any(|line| line.trim_end() == *heading) {
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
pub fn check_adr_index(index: Option<&str>, adrs: &[AdrFile]) -> Vec<Violation> {
    let Some(index) = index else {
        return vec![Violation::new(
            "adr-index",
            format!("{ADR_DIR}/{ADR_INDEX}"),
            "the decision record has no index",
        )];
    };

    let mut violations: Vec<Violation> = adrs
        .iter()
        .filter(|adr| !index.contains(&adr.name))
        .map(|adr| {
            Violation::new(
                "adr-index",
                adr.name.clone(),
                "exists in docs/adr but is not linked from the index",
            )
        })
        .collect();

    // The reverse direction: a link to an ADR that was renamed or never landed reads as a
    // decision the project took, and clicking it is the only way to find out otherwise.
    for linked in linked_markdown_files(index) {
        if adr_number(&linked).is_some() && !adrs.iter().any(|adr| adr.name == linked) {
            violations.push(Violation::new(
                "adr-index",
                linked,
                "is linked from the index but does not exist in docs/adr",
            ));
        }
    }

    violations
}

/// Every `*.md` target of a Markdown link in `contents`, without any directory part.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "ADR file names are lowercase by policy"
)]
fn linked_markdown_files(contents: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = contents;
    while let Some(open) = rest.find("](") {
        let after = rest.get(open + 2..).unwrap_or_default();
        let Some(close) = after.find(')') else {
            break;
        };
        let target = after.get(..close).unwrap_or_default();
        if let Some(name) = target.rsplit('/').next()
            && name.ends_with(".md")
        {
            found.push(name.to_owned());
        }
        rest = after.get(close..).unwrap_or_default();
    }
    found
}

/// Rule: the ADR recording design document §02 records all eight decisions.
#[must_use]
pub fn check_settled_decisions(adrs: &[AdrFile]) -> Vec<Violation> {
    let Some(adr) = adrs.iter().find(|adr| adr.name == SETTLED_DECISIONS_ADR) else {
        return vec![Violation::new(
            "settled-decisions",
            SETTLED_DECISIONS_ADR,
            "the eight decisions design document §02 settles have no ADR",
        )];
    };

    SETTLED_DECISIONS
        .iter()
        .flat_map(|decision| {
            let mut missing = Vec::new();
            if !adr.contents.contains(decision.id) {
                missing.push(Violation::new(
                    "settled-decisions",
                    decision.id,
                    format!("is not recorded in {SETTLED_DECISIONS_ADR}"),
                ));
            }
            if !adr.contents.contains(decision.headline) {
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

/// Rule: every diagram issue #11 asks for exists, is Mermaid, and shows every step.
#[must_use]
pub fn check_diagrams(architecture: Option<&str>) -> Vec<Violation> {
    let Some(architecture) = architecture else {
        return vec![Violation::new(
            "diagrams",
            ARCHITECTURE_PATH,
            "the repository has no architecture document, so it has no diagrams",
        )];
    };

    let blocks = mermaid_blocks(architecture);
    let mut violations = Vec::new();

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
            if !block.body.contains(label) {
                violations.push(Violation::new(
                    "diagrams",
                    spec.id,
                    format!(
                        "does not show `{label}`, which design document {} lists",
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
/// A fourth layer added to the table therefore fails this rule until the diagram draws it,
/// which is the whole reason the diagram is checked rather than admired.
fn check_dependency_diagram(blocks: &[MermaidBlock]) -> Vec<Violation> {
    let Some(block) = blocks
        .iter()
        .find(|block| block.id == CRATE_DEPENDENCY_DIAGRAM)
    else {
        // Already reported as a missing diagram.
        return Vec::new();
    };

    let mut violations = Vec::new();
    for spec in LAYERS {
        if !block.body.contains(spec.name) {
            violations.push(Violation::new(
                "diagrams",
                CRATE_DEPENDENCY_DIAGRAM,
                format!("does not draw `{}`, which policy::LAYERS lists", spec.name),
            ));
        }
        for dependency in spec.may_depend_on {
            let edge = format!("{} --> {dependency}", spec.name);
            if !block.body.contains(&edge) {
                violations.push(Violation::new(
                    "diagrams",
                    CRATE_DEPENDENCY_DIAGRAM,
                    format!(
                        "does not draw the edge `{edge}`, which policy::LAYERS permits and \
                         the layering gate enforces"
                    ),
                ));
            }
        }
    }
    violations
}

/// Rule: every crate root in the workspace warns on an undocumented public item.
///
/// Issue #11: "Doc comments required on all public APIs (`#[warn(missing_docs)]` in each
/// crate)." The attribute is one line, and deleting it is invisible: nothing fails, the
/// warnings simply stop.
#[must_use]
pub fn check_missing_docs(inputs: &DocsInputs) -> Vec<Violation> {
    let mut violations: Vec<Violation> = inputs
        .members_without_roots
        .iter()
        .map(|package| {
            Violation::new(
                "missing-docs",
                package.clone(),
                "is a workspace member with no crate root the gate could find, so the \
                 missing_docs rule did not run on it",
            )
        })
        .collect();

    for root in &inputs.crate_roots {
        let attributes = inner_attributes(&root.contents);
        if !attributes
            .iter()
            .any(|line| MISSING_DOCS_ATTRIBUTES.contains(&line.as_str()))
        {
            violations.push(Violation::new(
                "missing-docs",
                root.package.clone(),
                format!(
                    "{} is missing `#![warn(missing_docs)]`; without it an undocumented \
                     public item costs nothing",
                    root.path
                ),
            ));
        }
        if attributes
            .iter()
            .any(|line| line.starts_with("#![allow(") && line.contains("missing_docs"))
        {
            violations.push(Violation::new(
                "missing-docs",
                root.package.clone(),
                format!(
                    "{} allows `missing_docs`, which turns the warning off again",
                    root.path
                ),
            ));
        }
    }

    violations
}

/// Runs every documentation rule.
#[must_use]
pub fn check_documentation(inputs: &DocsInputs, rules: &[&str]) -> Vec<Violation> {
    let mut violations = check_claude_md(inputs.claude_md.as_deref(), rules);
    violations.extend(check_adr_numbering(&inputs.adrs));
    violations.extend(check_adr_structure(&inputs.adrs));
    violations.extend(check_adr_index(inputs.adr_index.as_deref(), &inputs.adrs));
    violations.extend(check_settled_decisions(&inputs.adrs));
    violations.extend(check_diagrams(inputs.architecture.as_deref()));
    violations.extend(check_missing_docs(inputs));
    violations
}

/// Fixtures describing documentation that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use std::fmt::Write as _;

    use super::{
        AdrFile, CrateRoot, DocsInputs, EFFECT_PROTOCOL_STEPS, SETTLED_DECISIONS,
        SETTLED_DECISIONS_ADR, TWO_BANK_SWAP_STEPS,
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
    #[must_use]
    pub fn clean_architecture() -> String {
        let mut body = String::from("# Architecture\n");
        line(
            &mut body,
            format_args!("\n<!-- diagram: crate-dependency-flow -->\n"),
        );
        line(&mut body, format_args!("```mermaid\ngraph LR"));
        for spec in LAYERS {
            line(&mut body, format_args!("  {}[{}]", spec.name, spec.name));
            for dependency in spec.may_depend_on {
                line(&mut body, format_args!("  {} --> {dependency}", spec.name));
            }
        }
        line(&mut body, format_args!("```\n"));

        for (id, steps) in [
            ("durable-effect-protocol", EFFECT_PROTOCOL_STEPS),
            ("two-bank-swap", TWO_BANK_SWAP_STEPS),
        ] {
            line(&mut body, format_args!("<!-- diagram: {id} -->\n"));
            line(&mut body, format_args!("```mermaid\nflowchart TD"));
            for (index, step) in steps.iter().enumerate() {
                line(&mut body, format_args!("  s{index}[\"{step}\"]"));
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
                format_args!("| {} | {} |", spec.name, spec.must_not_own),
            );
        }
        for decision in SETTLED_DECISIONS {
            line(&mut body, format_args!("- {}", decision.id));
        }
        for rule in rules {
            line(&mut body, format_args!("- `{rule}`"));
        }
        body
    }

    /// Documentation inputs that every rule passes.
    #[must_use]
    pub fn clean_inputs(rules: &[&str]) -> DocsInputs {
        DocsInputs {
            claude_md: Some(clean_claude_md(rules)),
            architecture: Some(clean_architecture()),
            adr_index: Some(format!(
                "# Decisions\n\n- [template](0000-template.md)\n- [one](0001-one.md)\n\
                 - [two](0002-two.md)\n- [three]({SETTLED_DECISIONS_ADR})\n"
            )),
            adrs: vec![
                AdrFile {
                    name: "0000-template.md".to_owned(),
                    contents: "# ADR NNNN: title\n\n- Status: proposed | accepted\n\
                               - Date: YYYY-MM-DD\n\n## Context\n\n## Decision\n\n\
                               ## Consequences\n"
                        .to_owned(),
                },
                AdrFile {
                    name: "0001-one.md".to_owned(),
                    contents: clean_adr("one"),
                },
                AdrFile {
                    name: "0002-two.md".to_owned(),
                    contents: clean_adr("two"),
                },
                AdrFile {
                    name: SETTLED_DECISIONS_ADR.to_owned(),
                    contents: clean_settled_decisions_adr(),
                },
            ],
            crate_roots: vec![CrateRoot {
                package: "waymaker-core".to_owned(),
                path: "crates/waymaker-core/src/lib.rs".to_owned(),
                contents: "//! Docs.\n#![no_std]\n#![warn(missing_docs)]\n".to_owned(),
            }],
            members_without_roots: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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
        let violations = check_claude_md(Some(&clean_claude_md(&[])), RULES);
        assert_eq!(violations.len(), RULES.len(), "{violations:?}");
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
    fn a_missing_index_is_reported() {
        let violations = check_adr_index(None, &[]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "adr-index");
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
    fn the_effect_protocol_has_seven_steps_and_the_swap_has_seven() {
        assert_eq!(EFFECT_PROTOCOL_STEPS.len(), 7);
        assert_eq!(TWO_BANK_SWAP_STEPS.len(), 7);
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
    fn a_missing_architecture_document_is_reported_once() {
        let violations = check_diagrams(None);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "diagrams");
    }

    #[test]
    fn a_crate_root_without_the_missing_docs_attribute_is_reported() {
        let mut inputs = clean_inputs(RULES);
        inputs.crate_roots[0].contents = "//! Docs.\n#![no_std]\n".to_owned();
        let violations = check_missing_docs(&inputs);
        assert!(
            violations.iter().any(|v| v.detail.contains("missing_docs")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_crate_root_that_allows_missing_docs_is_reported() {
        let mut inputs = clean_inputs(RULES);
        inputs.crate_roots[0].contents =
            "#![warn(missing_docs)]\n#![allow(missing_docs)]\n".to_owned();
        let violations = check_missing_docs(&inputs);
        assert!(
            violations.iter().any(|v| v.detail.contains("allows")),
            "{violations:?}"
        );
    }

    #[test]
    fn deny_and_forbid_satisfy_the_missing_docs_rule() {
        for attribute in ["#![deny(missing_docs)]", "#![forbid(missing_docs)]"] {
            let mut inputs = clean_inputs(RULES);
            inputs.crate_roots[0].contents = format!("//! Docs.\n{attribute}\n");
            assert!(check_missing_docs(&inputs).is_empty(), "{attribute}");
        }
    }

    #[test]
    fn a_workspace_member_with_no_crate_root_is_reported() {
        let mut inputs = clean_inputs(RULES);
        inputs.members_without_roots.push("ghost".to_owned());
        let violations = check_missing_docs(&inputs);
        assert!(
            violations.iter().any(|v| v.subject == "ghost"),
            "{violations:?}"
        );
    }

    #[test]
    fn every_documentation_rule_fires_on_documentation_that_has_none() {
        let violations = check_documentation(&DocsInputs::default(), RULES);
        assert_eq!(
            rule_ids(&violations),
            [
                "adr-index",
                "adr-numbering",
                "claude-md",
                "diagrams",
                "settled-decisions"
            ]
        );
    }
}
