//! Every `(operation, width)` shape the writers above this contract issue.
//!
//! Issue [#130](https://github.com/madmax983/waymaker/issues/130) item 2 asks for two
//! things: a list of the shapes `waymaker-flash`'s and `waymaker-fault`'s writers really
//! issue, and proof that this suite issues each one somewhere. [`SHAPES`] is the list.
//! [`ShapeWitness`] is the proof: it wraps a [`StableStorage`] and records which shapes a
//! run against it actually used, so a row can be checked rather than trusted.
//!
//! The `storage-shapes` rule of `cargo xtask check-layering` compares [`SHAPES`] against
//! `xtask::docs::STORAGE_SHAPES`, against `CLAUDE.md`, and against
//! [ADR 0047](https://github.com/madmax983/waymaker/blob/main/docs/adr/0047-a-shape-catalogue-holds-the-suite-to-the-writers.md),
//! so a shape cannot be added to one of the four and forgotten in the others. What that rule
//! cannot see is inside this crate: `tests/shapes.rs::a_full_run_issues_every_declared_shape`
//! is what proves a real run issues every row.

use waymaker_flash::storage::{Geometry, StableStorage};

/// One shape a legal call to [`StableStorage::read`], [`StableStorage::program`] or
/// [`StableStorage::erase`] can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    /// Stable identifier, cited when a change touches this shape.
    pub id: &'static str,
    /// The shape, in one sentence.
    pub sentence: &'static str,
    /// Which writer or reader in `waymaker-flash` issues it.
    pub issued_by: &'static str,
}

/// Every shape a call above this contract can have.
///
/// A program and an erase each have two rows: one unit and more than one. A read has the
/// same two, and no third row for the caller-sized scan `recovery::Recovery`'s erased-tail
/// walk makes: its width is the caller's own page, rounded down to a whole number of read
/// units, so it lands in one of the same two rows depending on that page — one unit on a
/// page that holds no more, more than one on a wider page — and a caller-chosen chunk size
/// is not a shape of its own.
pub const SHAPES: &[Shape] = &[
    Shape {
        id: "program-single-unit",
        sentence: "A program of exactly one program unit.",
        issued_by: "`append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever the padded value — at the journal's own alignment, which may be coarser than the device program unit — comes to exactly one device program unit",
    },
    Shape {
        id: "program-multi-unit",
        sentence: "A program of more than one program unit in one call.",
        issued_by: "`append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever that padded value spans more than one device program unit",
    },
    Shape {
        id: "erase-single-block",
        sentence: "An erase of exactly one erase block.",
        issued_by: "`swap::Swap::prepare` and `Installed::reclaim`, on a device whose bank is one erase block",
    },
    Shape {
        id: "erase-multi-block",
        sentence: "An erase of more than one erase block in one call.",
        issued_by: "`swap::Swap::prepare` and `Installed::reclaim`, on a device with at least four erase blocks",
    },
    Shape {
        id: "read-single-unit",
        sentence: "A read of exactly one read unit.",
        issued_by: "`recovery::Recovery::stage`'s header read and its erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — come to exactly one read unit",
    },
    Shape {
        id: "read-multi-unit",
        sentence: "A read of more than one read unit in one call.",
        issued_by: "`recovery::Recovery::stage`'s whole-record read, always at least two read units by construction; and its header read and erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — span more than one read unit",
    },
];

/// The shape with this id, if there is one.
#[must_use]
pub fn shape(id: &str) -> Option<&'static Shape> {
    SHAPES.iter().find(|candidate| candidate.id == id)
}

/// The shape id of one successful call, or `None` for a call [`SHAPES`] does not name.
///
/// `None` for a zero-length call — [`crate::case::CaseId::ZeroLengthOperationsAreLegalAndChangeNothing`]
/// is the case for that, and zero units is neither one unit nor more than one — and for a
/// misaligned length, which never reaches a real writer because a legal call is always a
/// whole number of units.
const fn classify(operation: Operation, len: u32, unit: u32) -> Option<&'static str> {
    if unit == 0 || len == 0 || len % unit != 0 {
        return None;
    }
    let single = len == unit;
    match (operation, single) {
        (Operation::Program, true) => Some("program-single-unit"),
        (Operation::Program, false) => Some("program-multi-unit"),
        (Operation::Erase, true) => Some("erase-single-block"),
        (Operation::Erase, false) => Some("erase-multi-block"),
        (Operation::Read, true) => Some("read-single-unit"),
        (Operation::Read, false) => Some("read-multi-unit"),
    }
}

/// Which call [`classify`] is being asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Operation {
    /// [`StableStorage::read`].
    Read,
    /// [`StableStorage::program`].
    Program,
    /// [`StableStorage::erase`].
    Erase,
}

/// Wraps a [`StableStorage`] and records which [`SHAPES`] a run against it actually issues.
///
/// A hand-written table can drift from what the suite really does the moment a case
/// changes; this is the check that cannot. It credits a call only after the wrapped adapter
/// accepted it — see [`crate::suite`]'s own module documentation for why a shape is a claim
/// about a legal call and not about one attempted.
pub struct ShapeWitness<'storage, S> {
    storage: &'storage mut S,
    seen: [bool; SHAPES.len()],
}

impl<'storage, S> ShapeWitness<'storage, S> {
    /// Wraps `storage`. Nothing has been observed yet.
    #[must_use]
    pub const fn new(storage: &'storage mut S) -> Self {
        Self {
            storage,
            seen: [false; SHAPES.len()],
        }
    }

    /// Every shape this witness has not yet observed.
    pub fn unseen(&self) -> impl Iterator<Item = &'static Shape> + '_ {
        SHAPES
            .iter()
            .zip(self.seen)
            .filter_map(|(spec, seen)| (!seen).then_some(spec))
    }

    /// Marks `id` as observed, if [`SHAPES`] names it.
    fn record(&mut self, id: &str) {
        if let Some(index) = SHAPES.iter().position(|spec| spec.id == id)
            && let Some(slot) = self.seen.get_mut(index)
        {
            *slot = true;
        }
    }
}

impl<S: StableStorage> StableStorage for ShapeWitness<'_, S> {
    type Error = S::Error;

    fn geometry(&self) -> Geometry {
        self.storage.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let unit = self.storage.geometry().read_size();
        // `Ok` and skipped rather than clamped on overflow: a length this witness cannot
        // name is a length it declines to guess a shape for, not one it credits regardless.
        let len = u32::try_from(dst.len()).ok();
        self.storage.read(offset, dst)?;
        if let Some(id) = len.and_then(|len| classify(Operation::Read, len, unit)) {
            self.record(id);
        }
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let unit = self.storage.geometry().program_size();
        let len = u32::try_from(src.len()).ok();
        self.storage.program(offset, src)?;
        if let Some(id) = len.and_then(|len| classify(Operation::Program, len, unit)) {
            self.record(id);
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let unit = self.storage.geometry().erase_size();
        self.storage.erase(offset, len)?;
        if let Some(id) = classify(Operation::Erase, len, unit) {
            self.record(id);
        }
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.storage.barrier()
    }
}

#[cfg(test)]
mod tests {
    use super::{Operation, SHAPES, classify, shape};

    #[test]
    fn every_shape_classifies_from_its_own_row() {
        // Every id `classify` can return has to be a row `SHAPES` really declares, or a
        // witness could mark a shape as seen that the table never named.
        for operation in [Operation::Read, Operation::Program, Operation::Erase] {
            for single in [true, false] {
                let len = if single { 4 } else { 8 };
                let Some(id) = classify(operation, len, 4) else {
                    unreachable!("a whole number of a non-zero unit always classifies");
                };
                assert!(shape(id).is_some(), "{id} is not in SHAPES");
            }
        }
    }

    #[test]
    fn a_zero_length_call_has_no_shape() {
        assert_eq!(classify(Operation::Program, 0, 4), None);
    }

    #[test]
    fn a_misaligned_length_has_no_shape() {
        // Never reached by a real writer — every legal call is a whole number of units —
        // but a witness has to decline rather than guess if one ever is.
        assert_eq!(classify(Operation::Program, 6, 4), None);
    }

    #[test]
    fn a_zero_unit_has_no_shape() {
        assert_eq!(classify(Operation::Read, 4, 0), None);
    }

    // Whether a shape is *reachable* — really issued somewhere by a real run — is
    // `tests/shapes.rs::a_full_run_issues_every_declared_shape`'s claim, not this one's.
    #[test]
    fn every_shape_has_a_sentence_and_an_issuer() {
        for spec in SHAPES {
            assert!(!spec.sentence.is_empty(), "{} has no sentence", spec.id);
            assert!(!spec.issued_by.is_empty(), "{} names no issuer", spec.id);
        }
    }
}
