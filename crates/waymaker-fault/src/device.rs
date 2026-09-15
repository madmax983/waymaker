//! Media that behaves like media.
//!
//! A [`Device`] is a byte array with three properties real NOR flash has and a `Vec<u8>`
//! does not: it starts erased rather than zeroed, programming can only clear bits, and an
//! operation the geometry forbids never reaches it. Everything a crash injector does is
//! layered on top of this; getting these three wrong would make every fault test above it
//! agree with a model rather than with hardware.

use core::fmt;

use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// The byte an erased cell reads as.
///
/// Public because a test that asserts what a stale tail looks like should not have to
/// write `0xFF` and hope.
pub const ERASED: u8 = 0xFF;

/// Whether every byte of `bytes` is [`ERASED`], a word at a time.
///
/// [`Device::apply_erase`] and [`Device::erase_would_change`] are on the crash injector's
/// hottest path — [`crate::Session::erase`] calls the first once per erase in every writer
/// sequence the injector drives, at every length an interrupted erase can land at, so this
/// runs over spans up to a whole erase block, over and over, for every crash point swept.
/// A byte-at-a-time `iter().any(|&b| b != ERASED)` there pays a bounds check and a compare
/// per byte for an answer usable in `usize`-sized chunks; re-erasing an already-erased
/// block — the bank-prepare shape `apply_erase`'s own doc comment names as "not exotic" —
/// is the case that scan cannot short-circuit out of; it has to read every byte to answer
/// "no". Same technique `waymaker_conformance::suite::slice_is_erased` uses for the same
/// reason, reimplemented rather than shared: that function is private to a crate two layers
/// away from this one. Checked against the byte-at-a-time definition at every length and
/// single-byte-mutation position around a word boundary in this module's tests, so a
/// remainder handled short does not pass silently.
fn slice_is_erased(bytes: &[u8]) -> bool {
    const WORD: usize = size_of::<usize>();
    let mut words = bytes.chunks_exact(WORD);
    let words_erased = words.by_ref().all(|word| {
        matches!(<[u8; WORD]>::try_from(word), Ok(word) if usize::from_ne_bytes(word) == usize::MAX)
    });
    words_erased && words.remainder().iter().all(|&byte| byte == ERASED)
}

/// What the model does when a program asks for a bit the media has already cleared.
///
/// Real NOR silently drops it, which is why firmware bugs of this shape survive testing on
/// a `Vec<u8>` model that assigns instead of masking. Both behaviours are here on purpose:
/// [`Nor`](OneWayBits::Absorbed) is what the hardware does and is the default, and
/// [`Rejected`](OneWayBits::Rejected) is a strictness knob for a test that wants the bug
/// reported rather than absorbed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OneWayBits {
    /// Clear the bits that can be cleared and silently ignore the rest, as hardware does.
    #[default]
    Absorbed,
    /// Refuse the whole program, touching no media.
    Rejected,
}

/// Anything a modelled device can refuse to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultError {
    /// The operation is not one this geometry permits.
    Geometry(GeometryError),
    /// A program asked for a bit that only an erase can restore.
    ///
    /// Only ever returned under [`OneWayBits::Rejected`].
    BitSetWithoutErase,
    /// The power went away. Nothing after this observation happened, or ever will.
    PowerLoss,
    /// The core was reset while the supply held. Nothing after this observation happened.
    ///
    /// The supply holds, so two things differ from [`PowerLoss`](Self::PowerLoss). The flash
    /// controller finishes the program unit the core stopped believing in, so media holds a
    /// whole number of units. And the call never returns, so the writer is never told the
    /// operation completed — where a power cut at [`Progress::Whole`] returns `Ok(())`
    /// first.
    ///
    /// A real core returns from neither. This is an error because the model has no way to
    /// stop the thread.
    ///
    /// [`Progress::Whole`]: crate::Progress::Whole
    WatchdogReset,
    /// The injected failure of a `program` or an `erase`: the call returns an error, media
    /// may already have changed, and the caller carries on.
    ///
    /// Named for the *failure* rather than for the injection, because
    /// [`PowerLoss`](Self::PowerLoss) is injected too: what tells them apart is that this
    /// one leaves the device alive.
    InjectedFailure,
}

impl FaultError {
    /// A short static description of this failure.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Geometry(error) => error.message(),
            Self::BitSetWithoutErase => "a program would set a bit that only an erase restores",
            Self::PowerLoss => "power was lost; nothing after this point happened",
            Self::WatchdogReset => "the core was reset; nothing after this point happened",
            Self::InjectedFailure => "the injected failure of a program or an erase",
        }
    }
}

impl From<GeometryError> for FaultError {
    fn from(error: GeometryError) -> Self {
        Self::Geometry(error)
    }
}

impl fmt::Display for FaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for FaultError {}

/// An in-memory device with a geometry.
///
/// # Invariants
///
/// The image is always exactly `geometry.capacity()` bytes. Every byte is `ERASED` until
/// something programs it, and no operation this type performs can set a bit that was
/// cleared without an intervening erase.
#[derive(Clone, Debug)]
pub struct Device {
    geometry: Geometry,
    media: Vec<u8>,
    bits: OneWayBits,
}

impl Device {
    /// An erased device with the hardware bit rule.
    #[must_use]
    pub fn new(geometry: Geometry) -> Self {
        Self::with_bit_rule(geometry, OneWayBits::Absorbed)
    }

    /// An erased device that reports one-way bit violations the way `rule` says.
    #[must_use]
    pub fn with_bit_rule(geometry: Geometry, rule: OneWayBits) -> Self {
        Self {
            geometry,
            media: vec![ERASED; geometry.capacity() as usize],
            bits: rule,
        }
    }

    /// A device holding `image`, as a reset would find it.
    ///
    /// `None` if `image` is not exactly `geometry.capacity()` bytes: a device that is not
    /// the size it says it is would let a caller read past the end of the one it modelled.
    /// This is how a [`crate::Run`]'s image is handed back to code that expects a
    /// [`StableStorage`] — a recovery path expressed against the contract rather than
    /// against a slice, which is the shape bank selection arrives in.
    #[must_use]
    pub fn restored(geometry: Geometry, image: Vec<u8>) -> Option<Self> {
        (image.len() == geometry.capacity() as usize).then_some(Self {
            geometry,
            media: image,
            bits: OneWayBits::Absorbed,
        })
    }

    /// The bytes as they stand, which is what a reader after a reset would see.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.media
    }

    /// The bytes, taken.
    #[must_use]
    pub fn into_image(self) -> Vec<u8> {
        self.media
    }

    /// Programs `src` at `offset` with no geometry check, as a torn write does.
    ///
    /// Crate-internal because it is not an operation a device offers: a torn write is a
    /// prefix of a *validated* operation, so [`crate::Session`] validates the whole write
    /// first and only then applies the part of it that survived. Exposing this would let a
    /// caller program across an erase-block boundary at an offset the geometry forbids,
    /// which is the one thing [`Geometry`] exists to stop.
    /// Returns whether any cell actually changed, which is not the same as whether bytes
    /// were offered: AND-masking `0xFF` over erased media is the identity, and a record
    /// whose only write was that has nothing on media for recovery to find.
    pub(crate) fn apply_program(&mut self, offset: u32, src: &[u8]) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return false;
        };
        let mut changed = false;
        for (cell, wanted) in target.iter_mut().zip(src) {
            let after = *cell & *wanted;
            changed |= after != *cell;
            *cell = after;
        }
        changed
    }

    /// Erases `offset..offset + len` with no geometry check, as an interrupted erase does.
    ///
    /// Returns whether any cell actually changed. Erasing an already-erased block — the
    /// bank-prepare shape, which is not exotic — changes nothing, and a record whose only
    /// mutation was that has nothing on media either.
    pub(crate) fn apply_erase(&mut self, offset: u32, len: u32) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return false;
        };
        let changed = !slice_is_erased(target);
        target.fill(ERASED);
        changed
    }

    /// Whether programming `src` at `offset` would ask for a bit only an erase can restore.
    pub(crate) fn would_set_a_bit(&self, offset: u32, src: &[u8]) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return false;
        };
        target
            .iter()
            .zip(src)
            .any(|(cell, wanted)| wanted & !cell != 0)
    }

    /// What programming `src` at `offset` would leave behind, cell by cell.
    ///
    /// The preimage masked by the bytes, because programming ANDs: over media already
    /// holding `0x3C`, programming `0xF0` leaves `0x30` and not `0xF0`. Storing the
    /// *argument* as a record's expected contents would call an untouched region torn.
    pub(crate) fn postimage_of(&self, offset: u32, src: &[u8]) -> Vec<u8> {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return Vec::new();
        };
        target
            .iter()
            .zip(src)
            .map(|(cell, wanted)| cell & wanted)
            .collect()
    }

    /// Whether the media at `offset` differs from `expected`, byte for byte.
    ///
    /// Equality rather than "would programming these bytes change anything". The second
    /// question cannot tell a withheld byte from one that arrived and then had *more* bits
    /// cleared: programming only clears, so replaying `0xF0` over a cell now holding `0x00`
    /// changes nothing and answers "already there" about a byte that is not.
    pub(crate) fn differs_from(&self, offset: u32, expected: &[u8]) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(expected.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return !expected.is_empty();
        };
        target != expected
    }

    /// Whether erasing `offset..offset + len` would change any cell.
    pub(crate) fn erase_would_change(&self, offset: u32, len: u32) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return false;
        };
        !slice_is_erased(target)
    }

    /// The bit rule this device was built with.
    pub(crate) const fn bit_rule(&self) -> OneWayBits {
        self.bits
    }
}

impl StableStorage for Device {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        let source = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(dst.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
            .ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(source);
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_program(offset, len)?;
        if self.bits == OneWayBits::Rejected && self.would_set_a_bit(offset, src) {
            return Err(FaultError::BitSetWithoutErase);
        }
        let _changed = self.apply_program(offset, src);
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        let _changed = self.apply_erase(offset, len);
        Ok(())
    }

    /// Always succeeds.
    ///
    /// A [`Device`] has no write-behind to flush and no reordering to settle: it *is* the
    /// durable state. Durability is modelled one level up, by [`crate::Session`], because
    /// what a barrier means is a property of when the power went away rather than of the
    /// bytes.
    fn barrier(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ERASED, slice_is_erased};

    /// `slice_is_erased` has to agree with "every byte is [`ERASED`]", at every length
    /// around a word boundary and with the one non-erased byte at every position — inside a
    /// whole word-sized chunk and inside whatever a word-at-a-time walk would leave as a
    /// remainder. Pinned before `apply_erase` and `erase_would_change` stop scanning a byte
    /// at a time, mirroring `waymaker_conformance::suite`'s own sweep of the same shape.
    #[test]
    fn slice_is_erased_agrees_with_the_byte_at_a_time_definition_at_every_length_and_position() {
        const MAX_LEN: usize = 32;
        let word = size_of::<usize>();
        let widest = word * 3 + 1;
        assert!(widest <= MAX_LEN, "word size outgrew this fixture");
        for len in 0..=widest {
            let all_erased = [ERASED; MAX_LEN];
            assert!(
                slice_is_erased(&all_erased[..len]),
                "length {len} of all erased bytes"
            );
            for spoiled in 0..len {
                let mut bytes = all_erased;
                bytes[spoiled] = 0x00;
                assert!(
                    !slice_is_erased(&bytes[..len]),
                    "length {len} spoiled at {spoiled}"
                );
            }
        }
    }
}
