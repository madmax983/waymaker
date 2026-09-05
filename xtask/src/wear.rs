//! The write-amplification figure, measured.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27)'s second "done when" is that
//! "the measured write amplification per effect is published alongside the size report". Two
//! words in that sentence do the work.
//!
//! **Measured.** Not derived from `frame::encoded_len` and a bit of arithmetic — that would
//! be a second implementation of the writer, and it would agree with the writer right up
//! until the writer changed. This runs the real [`Journal`](waymaker_flash::append::Journal)
//! through the real [`Rig`] over `waymaker_fault::Device`, and reports what the device was
//! asked for.
//!
//! **Per effect.** The denominator is completed effects rather than records, because an
//! effect is the unit a workflow author reasons about and the unit §04's budgets are
//! eventually spent per. A run of *n* effects writes `2n + 2` records, so a per-record figure
//! would quietly flatter the engine by amortising the run's opening and closing records over
//! the effects.
//!
//! # Why several geometries
//!
//! Because the answer depends on the part more than on the engine. A record's frame is the
//! same everywhere; its commit seal is one program unit, so a part that programs sixty-four
//! bytes at a time pays sixty-four bytes to commit a twenty-four-byte frame. A single figure
//! would be a figure about whichever part happened to be in the fixture.
//!
//! # What this is not
//!
//! A hardware measurement. Every figure here is what the *model* was asked for, which is
//! exactly the write amplification a real part would see for the same calls — but the erase
//! counts a real fleet accumulates, and the wear those erases actually cause, are the boards'
//! to report. `docs::HARDWARE_TARGETS` is where that is written down as owed.

use waymaker_flash::storage::Geometry;
use waymaker_rig::log::Outcome;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Dispatcher, NeverCut, Rig};
use waymaker_rig::wear::{Metered, Wear};

/// The seed every published figure is measured at.
///
/// Fixed, because a published number that moved with the clock is a number nobody can
/// reproduce. The workload's payload lengths depend on it, so it is part of the measurement's
/// definition rather than an incidental.
pub const SEED: u64 = 0x0000_0000_0000_0027;

/// How many effects each measured run schedules.
pub const EFFECTS: u16 = 8;

/// The parts the figure is measured on.
///
/// Capacity, erase size, program size, read size. Six erase blocks each: two banks of two
/// blocks in the engine area, one for the rig's instrument, one spare. The program unit is
/// what varies, because it is what the answer depends on.
pub const PARTS: &[(&str, u32, u32, u32, u32)] = &[
    ("byte-programmable", 6 * 4096, 4096, 1, 1),
    ("word-programmable", 6 * 4096, 4096, 4, 1),
    ("page-programmable", 6 * 4096, 4096, 16, 1),
];

/// What one part cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartWear {
    /// The row's name, from [`PARTS`].
    pub part: &'static str,
    /// The program unit, which is what the figure turns on.
    pub program_size: u32,
    /// What the engine cost the media.
    pub engine: Wear,
    /// What the rig's own instrument cost it, reported so that the engine's figure can be
    /// seen not to include it.
    pub instrument: Wear,
}

/// Why a figure could not be measured.
///
/// Every one of these is a failure rather than a skipped row. A report that quietly dropped a
/// part it could not measure would publish an average over whichever parts happened to work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WearError {
    /// What went wrong, in one line.
    pub message: String,
}

impl std::fmt::Display for WearError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WearError {}

/// A dispatcher that does nothing, because the figure is about media rather than effects.
struct Inert;

impl Dispatcher for Inert {
    type Error = std::convert::Infallible;

    fn dispatch(&mut self, _effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Runs one clean iteration on one part and reports what it cost.
///
/// # Errors
///
/// [`WearError`] when the part cannot be laid out, the run cannot be performed, or the run's
/// own verifier does not accept what it wrote — the last of which is the one that matters: a
/// figure measured from a run that did not recover is a figure about a broken write.
pub fn measure_part(part: &'static str, geometry: Geometry) -> Result<PartWear, WearError> {
    let fail = |what: &str| WearError {
        message: format!("{part}: {what}"),
    };
    let rig = Rig::new::<waymaker_fault::FaultError>(geometry, Plan::new(SEED), EFFECTS)
        .map_err(|error| fail(&format!("cannot be laid out ({error:?})")))?;
    let mut device = waymaker_fault::Device::new(geometry);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let (engine, instrument) = {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .map_err(|error| fail(&format!("cannot be prepared ({error:?})")))?;
        rig.iterate(0, &mut metered, &mut Inert, &mut NeverCut, &mut page)
            .map_err(|error| fail(&format!("cannot be written ({error:?})")))?;
        (metered.wear(), metered.rig_wear())
    };
    match rig.verify(0, &mut device, &mut page) {
        Ok(Outcome::Passed) => {}
        Ok(Outcome::Breached(breach)) => {
            return Err(fail(&format!(
                "wrote a run its own oracle rejects: {breach}"
            )));
        }
        Err(error) => return Err(fail(&format!("cannot be verified ({error:?})"))),
    }
    if engine.effects() != u32::from(EFFECTS) {
        return Err(fail("did not complete every effect it scheduled"));
    }
    Ok(PartWear {
        part,
        program_size: geometry.program_size(),
        engine,
        instrument,
    })
}

/// Every row of [`PARTS`], measured.
///
/// # Errors
///
/// The first [`WearError`]. Fails closed: a part that could not be measured is not a part to
/// leave out of the table.
pub fn measure() -> Result<Vec<PartWear>, WearError> {
    PARTS
        .iter()
        .map(|(name, capacity, erase, program, read)| {
            let geometry =
                Geometry::new(*capacity, *erase, *program, *read).map_err(|error| WearError {
                    message: format!("{name}: not a geometry ({})", error.message()),
                })?;
            measure_part(name, geometry)
        })
        .collect()
}

/// A per-effect figure, or a dash where no effect ran.
fn per_effect(value: Option<u32>) -> String {
    value.map_or_else(|| "-".to_owned(), |figure| figure.to_string())
}

/// The table `cargo xtask size` prints under the section sizes.
#[must_use]
pub fn render(rows: &[PartWear]) -> String {
    let width = rows
        .iter()
        .map(|row| row.part.len())
        .max()
        .unwrap_or(0)
        .max("part".len());
    let mut table = vec![format!(
        "\nwrite amplification per effect, measured over a {EFFECTS}-effect run at seed {SEED:#018x}\n  {:<width$}  {:>7} {:>9} {:>9} {:>8} {:>8}  {:>9} {:>10}\n",
        "part",
        "program",
        "payload B",
        "written B",
        "programs",
        "barriers",
        "erases/run",
        "blocks/run",
    )];
    for row in rows {
        // The first five columns are per effect; the last two are run totals, and
        // deliberately so. §10 erases a whole bank once per run, so an erase count divided by
        // eight effects is zero — a figure that reads as "this engine does not erase", which
        // is the opposite of true and exactly the kind of number a report should not print.
        table.push(format!(
            "  {:<width$}  {:>7} {:>9} {:>9} {:>8} {:>8}  {:>9} {:>10}\n",
            row.part,
            row.program_size,
            per_effect(row.engine.payload_bytes().checked_div(row.engine.effects())),
            per_effect(row.engine.programmed_bytes_per_effect()),
            per_effect(row.engine.program_operations_per_effect()),
            per_effect(row.engine.barriers_per_effect()),
            row.engine.erase_operations(),
            row.engine.erase_blocks(),
        ));
    }
    table.push(
        "the figures are what the device was asked for, including calls that would have failed: design document \u{a7}12 says a failed program may still have changed media, and a wear figure that counted only successes would understate the runs that wore the part hardest.\n"
            .to_owned(),
    );
    if let Some(row) = rows.first() {
        table.push(format!(
            "the rig's own instrument is excluded: it cost a further {} programs and {} barriers on the {} part, which is the measuring apparatus rather than the engine.\n",
            row.instrument.program_operations(),
            row.instrument.barriers(),
            row.part,
        ));
    }
    table.push(
        "the last two columns are run totals rather than per-effect figures: \u{a7}10 erases a whole bank once per run, so dividing by the effect count would print a zero that reads as \"this engine does not erase\". A fleet's real erase count over a part's life, and the wear those erases cause, are a hardware measurement and are owed by the boards named in docs::HARDWARE_TARGETS.\n"
            .to_owned(),
    );
    table.concat()
}

/// The rows as JSON, for the CI artifact.
#[must_use]
pub fn to_json(rows: &[PartWear]) -> String {
    let parts: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "part": row.part,
                "program_size": row.program_size,
                "effects": row.engine.effects(),
                "payload_bytes": row.engine.payload_bytes(),
                "programmed_bytes": row.engine.programmed_bytes(),
                "program_operations": row.engine.program_operations(),
                "barriers": row.engine.barriers(),
                "erase_operations": row.engine.erase_operations(),
                "erase_blocks": row.engine.erase_blocks(),
                "erased_bytes": row.engine.erased_bytes(),
                "programmed_bytes_per_effect": row.engine.programmed_bytes_per_effect(),
                "program_operations_per_effect": row.engine.program_operations_per_effect(),
                "barriers_per_effect": row.engine.barriers_per_effect(),
                "erase_operations_per_effect": row.engine.erase_operations_per_effect(),
                "instrument_program_operations": row.instrument.program_operations(),
                "instrument_barriers": row.instrument.barriers(),
            })
        })
        .collect();
    let document = serde_json::json!({
        "seed": SEED,
        "effects": EFFECTS,
        "parts": parts,
    });
    format!("{document:#}\n")
}

/// Where the JSON is written.
pub const REPORT_PATH: &str = "target/waymaker-wear.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_part_is_a_geometry_that_can_be_measured() {
        let rows = measure().expect("every part in the table is measurable");
        assert_eq!(rows.len(), PARTS.len());
    }

    #[test]
    fn a_wider_program_unit_costs_more_per_effect() {
        // The reason the table has rows rather than a number: a commit seal is one program
        // unit, so the same record costs more to commit on a coarser part.
        let rows = measure().expect("every part is measurable");
        let mut previous = 0;
        for row in &rows {
            let figure = row
                .engine
                .programmed_bytes_per_effect()
                .expect("a run with effects in it");
            assert!(
                figure >= previous,
                "{} programmed {figure} B per effect, less than a finer part",
                row.part
            );
            previous = figure;
        }
        assert!(previous > 0);
    }

    #[test]
    fn the_instrument_is_not_counted_in_the_engines_figure() {
        let rows = measure().expect("every part is measurable");
        for row in &rows {
            assert!(
                row.instrument.program_operations() > 0,
                "{}: the rig wrote no marks, so the split is untested",
                row.part
            );
        }
    }

    #[test]
    fn a_part_that_cannot_hold_two_banks_and_a_witness_is_an_error_rather_than_a_gap() {
        let Ok(tiny) = Geometry::new(256, 256, 4, 1) else {
            unreachable!("a legal geometry")
        };
        assert!(measure_part("tiny", tiny).is_err());
    }

    #[test]
    fn every_measured_run_erases() {
        // Issue #27 asks for erase counts. A workload that never erased would make that
        // column structurally zero, and a zero nobody could ever have moved is not a
        // measurement — it is a column.
        let rows = measure().expect("every part is measurable");
        for row in &rows {
            assert!(
                row.engine.erase_operations() > 0 && row.engine.erase_blocks() > 0,
                "{}: the run performed no erase, so the erase count measures nothing",
                row.part
            );
        }
    }

    #[test]
    fn the_erase_count_is_a_run_total_rather_than_a_per_effect_zero() {
        let rows = measure().expect("every part is measurable");
        let row = rows.first().expect("at least one part");
        assert_eq!(
            row.engine.erase_operations_per_effect(),
            Some(0),
            "the per-effect figure is the misleading one this table avoids printing"
        );
        let rendered = render(&rows);
        assert!(rendered.contains("erases/run"), "got {rendered}");
        assert!(
            rendered.contains(&format!("{:>9}", row.engine.erase_operations())),
            "the run total is not in the table"
        );
    }

    #[test]
    fn the_rendered_table_names_every_part_and_the_figure_it_is_per() {
        let rows = measure().expect("every part is measurable");
        let rendered = render(&rows);
        assert!(rendered.contains("write amplification per effect"));
        for (name, ..) in PARTS {
            assert!(rendered.contains(name), "{name} is not in the table");
        }
    }

    #[test]
    fn the_json_carries_the_seed_the_figure_was_measured_at() {
        let rows = measure().expect("every part is measurable");
        let json = to_json(&rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("the report is JSON");
        assert_eq!(parsed["seed"], serde_json::json!(SEED));
        assert_eq!(parsed["effects"], serde_json::json!(EFFECTS));
        assert_eq!(parsed["parts"].as_array().map(Vec::len), Some(PARTS.len()));
    }
}
