//! The rig's log line.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27)'s third "done when" is that
//! "any recovery violation is reproducible from the rig's log". That is a claim about an
//! encoding rather than about a message: a line that says "prefix safety failed at 03:14" is
//! a line nobody can reproduce anything from.
//!
//! # What a line carries, and why each field is in it
//!
//! * **the seed and the iteration** — the whole of the cut point and the whole of the run,
//!   because [`Cut`] and [`Workload`](crate::workload::Workload) are pure functions of them.
//!   Two numbers rather than a transcript of every record.
//! * **the geometry** — a violation reproduced on a different part is a different violation.
//!   Four `u32`s: the capacity and the three units.
//! * **the effect count** — the run's shape.
//! * **the outcome** — a [`Breach`] code and its one detail field, so the host reproduces the
//!   failure it was told about rather than whichever one it happens to find.
//! * **the wear** — issue #27's fourth work item, per iteration, so the published figure is a
//!   sum of lines rather than a number somebody typed.
//!
//! # Why there are two encodings
//!
//! [`Entry::encode`] is the bytes; [`Entry::render`] is a line of ASCII with the bytes in hex
//! at the end of it. A rig's transport is a serial port, and a board that could only emit
//! binary would be a board whose log a human cannot skim. The human part is a prefix and the
//! machine part is a suffix, so [`Entry::parse`] reads the suffix and the two can never
//! disagree — a renderer that lied in its prefix would still hand back the truth.
//!
//! # Why the line is sealed
//!
//! Because a serial link drops bytes, and a log line that arrived damaged must not be read as
//! a *different* run's violation. The check is the same one the firmware seals frames with.

use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::{Geometry, GeometryError};

use crate::audit::Breach;
use crate::plan::Cut;
use crate::wear::Wear;

/// How many bytes an encoded entry occupies.
pub const ENTRY_BYTES: usize = 76;

/// The magic an entry opens with.
const ENTRY_MAGIC: u16 = 0x4752;

/// How many bytes of an entry the check covers.
const ENTRY_BODY_BYTES: usize = ENTRY_BYTES - 4;

/// The text every rendered line opens with.
const LINE_PREFIX: &str = "waymaker-rig ";

/// Why a log entry could not be read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LogError {
    /// The buffer is shorter than the operation needs.
    ShortBuffer,
    /// These bytes are not an entry: damaged, truncated, or never one.
    NotAnEntry,
    /// The entry was written at a format version this build does not know.
    UnknownVersion {
        /// The version the entry declared.
        version: u8,
    },
    /// The entry's four units are not a geometry.
    Geometry(GeometryError),
}

impl LogError {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ShortBuffer => "the buffer is shorter than the operation needs",
            Self::NotAnEntry => "these bytes are not a log entry",
            Self::UnknownVersion { .. } => "the entry was written at an unknown format version",
            Self::Geometry(error) => error.message(),
        }
    }
}

impl core::fmt::Display for LogError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for LogError {}

/// How an iteration ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Recovery satisfied every guarantee the witness demanded of it.
    #[default]
    Passed,
    /// It did not.
    Breached(Breach),
}

impl Outcome {
    /// The code a line carries. Zero for a pass.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Breached(breach) => breach.code(),
        }
    }

    /// The detail the breach's own field carried, packed into one word.
    #[must_use]
    pub fn detail(self) -> u32 {
        match self {
            Self::Passed | Self::Breached(Breach::WitnessUnreadable) => 0,
            Self::Breached(
                Breach::RecordDiffers { index }
                | Breach::LostAcknowledgedRecord { index }
                | Breach::DispatchedEffectWithoutSchedule { index }
                | Breach::DispatchMarkIsNotASchedule { index },
            ) => u32::from(index),
            Self::Breached(Breach::RecoveredPastWhatWasAttempted {
                recovered,
                attempted,
            }) => (u32::from(recovered) << 16) | u32::from(attempted),
            // Unreachable past `u32::MAX`: there are two banks. Saturating rather than
            // truncating, because a count that wrapped would read as a *legal* authority.
            Self::Breached(Breach::Authority { banks }) => u32::try_from(banks).unwrap_or(u32::MAX),
        }
    }

    /// A short static name, for the human half of a line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Breached(breach) => breach.name(),
        }
    }

    /// The outcome `code` and `detail` describe.
    fn from_parts(code: u8, detail: u32) -> Option<Self> {
        // The low half of the detail word, taken by masking rather than by a cast: this is a
        // decoder, and a decoder reading a field the encoder never wrote is exactly the bug a
        // truncating cast hides.
        let index = u16::try_from(detail & 0xFFFF).ok()?;
        match code {
            0 => Some(Self::Passed),
            1 => Some(Self::Breached(Breach::RecordDiffers { index })),
            2 => Some(Self::Breached(Breach::LostAcknowledgedRecord { index })),
            3 => Some(Self::Breached(Breach::RecoveredPastWhatWasAttempted {
                recovered: (detail >> 16) as u16,
                attempted: index,
            })),
            4 => Some(Self::Breached(Breach::DispatchedEffectWithoutSchedule {
                index,
            })),
            5 => Some(Self::Breached(Breach::DispatchMarkIsNotASchedule { index })),
            6 => Some(Self::Breached(Breach::Authority {
                banks: usize::try_from(detail).ok()?,
            })),
            7 => Some(Self::Breached(Breach::WitnessUnreadable)),
            _ => None,
        }
    }
}

/// A cursor over a fixed buffer, writing little-endian words.
struct Writer<'a> {
    bytes: &'a mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn put(&mut self, source: &[u8]) -> Option<()> {
        let end = self.at.checked_add(source.len())?;
        let slot = self.bytes.get_mut(self.at..end)?;
        slot.copy_from_slice(source);
        self.at = end;
        Some(())
    }

    fn u8(&mut self, value: u8) -> Option<()> {
        self.put(&[value])
    }

    fn u16(&mut self, value: u16) -> Option<()> {
        self.put(&value.to_le_bytes())
    }

    fn u32(&mut self, value: u32) -> Option<()> {
        self.put(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> Option<()> {
        self.put(&value.to_le_bytes())
    }

    /// The wear block, whose layout is [`Wear`]'s own rather than this module's.
    fn wear(&mut self, wear: Wear) -> Option<()> {
        let end = self.at.checked_add(Wear::ENCODED_BYTES)?;
        let slot = self.bytes.get_mut(self.at..end)?;
        wear.encode(slot)?;
        self.at = end;
        Some(())
    }
}

/// A cursor over a fixed buffer, reading little-endian words.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(len)?;
        let slot = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slot)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(<[u8; 2]>::try_from(self.take(2)?).ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(<[u8; 4]>::try_from(self.take(4)?).ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(<[u8; 8]>::try_from(self.take(8)?).ok()?))
    }

    /// The wear block, read back by [`Wear`] rather than field by field here.
    fn wear(&mut self) -> Option<Wear> {
        Wear::decode(self.take(Wear::ENCODED_BYTES)?)
    }
}

/// One iteration of the rig, as the log records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    seed: u64,
    iteration: u32,
    capacity: u32,
    erase_size: u32,
    program_size: u32,
    read_size: u32,
    effects: u16,
    outcome: Outcome,
    wear: Wear,
}

impl Entry {
    /// The format version this build writes and reads.
    pub const FORMAT_VERSION: u8 = 1;

    /// The longest line [`render`](Self::render) produces.
    pub const LINE_BYTES: usize = LINE_PREFIX.len() + 64 + 2 * ENTRY_BYTES;

    /// An entry for an iteration that has not been judged yet.
    #[must_use]
    pub const fn new(seed: u64, iteration: u32, geometry: Geometry, effects: u16) -> Self {
        Self {
            seed,
            iteration,
            capacity: geometry.capacity(),
            erase_size: geometry.erase_size(),
            program_size: geometry.program_size(),
            read_size: geometry.read_size(),
            effects,
            outcome: Outcome::Passed,
            wear: Wear::NONE,
        }
    }

    /// This entry with `outcome` recorded.
    #[must_use]
    pub const fn with_outcome(self, outcome: Outcome) -> Self {
        Self { outcome, ..self }
    }

    /// This entry with `wear` recorded.
    #[must_use]
    pub const fn with_wear(self, wear: Wear) -> Self {
        Self { wear, ..self }
    }

    /// The seed the run was derived from.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Which iteration this was.
    #[must_use]
    pub const fn iteration(self) -> u32 {
        self.iteration
    }

    /// How many effects the run scheduled.
    #[must_use]
    pub const fn effects(self) -> u16 {
        self.effects
    }

    /// How the iteration ended.
    #[must_use]
    pub const fn outcome(self) -> Outcome {
        self.outcome
    }

    /// What the iteration cost the part.
    #[must_use]
    pub const fn wear(self) -> Wear {
        self.wear
    }

    /// The cut this iteration armed, derived rather than stored.
    #[must_use]
    pub const fn cut(self) -> Cut {
        Cut::for_iteration(self.seed, self.iteration)
    }

    /// The device the run happened on.
    ///
    /// # Errors
    ///
    /// [`LogError::Geometry`] if the four units in the line are not a geometry — which a line
    /// this build wrote cannot be, and a line from another build can.
    pub const fn geometry(self) -> Result<Geometry, LogError> {
        match Geometry::new(
            self.capacity,
            self.erase_size,
            self.program_size,
            self.read_size,
        ) {
            Ok(geometry) => Ok(geometry),
            Err(error) => Err(LogError::Geometry(error)),
        }
    }

    /// Writes this entry into `out`.
    ///
    /// # Errors
    ///
    /// [`LogError::ShortBuffer`] if `out` is shorter than [`ENTRY_BYTES`].
    pub fn encode(self, out: &mut [u8]) -> Result<usize, LogError> {
        self.encode_with::<Catalogued>(out)
    }

    /// Writes this entry into `out`, sealed with `C`.
    ///
    /// # Errors
    ///
    /// [`LogError::ShortBuffer`] if `out` is shorter than [`ENTRY_BYTES`].
    pub fn encode_with<C: IntegrityCheck>(self, out: &mut [u8]) -> Result<usize, LogError> {
        let Some(slot) = out.get_mut(..ENTRY_BYTES) else {
            return Err(LogError::ShortBuffer);
        };
        let mut writer = Writer { bytes: slot, at: 0 };
        let written = (|| {
            writer.u16(ENTRY_MAGIC)?;
            writer.u8(Self::FORMAT_VERSION)?;
            writer.u8(self.outcome.code())?;
            writer.u64(self.seed)?;
            writer.u32(self.iteration)?;
            writer.u16(self.effects)?;
            writer.u16(0)?;
            writer.u32(self.capacity)?;
            writer.u32(self.erase_size)?;
            writer.u32(self.program_size)?;
            writer.u32(self.read_size)?;
            writer.u32(self.outcome.detail())?;
            writer.wear(self.wear)?;
            Some(writer.at)
        })();
        if written != Some(ENTRY_BODY_BYTES) {
            return Err(LogError::ShortBuffer);
        }
        let (body, seal) = slot.split_at_mut(ENTRY_BODY_BYTES);
        seal.copy_from_slice(&C::frame_check(body).to_le_bytes());
        Ok(ENTRY_BYTES)
    }

    /// The format version `bytes` declares, without reading the rest.
    ///
    /// # Errors
    ///
    /// [`LogError::ShortBuffer`] and [`LogError::NotAnEntry`], as [`decode`](Self::decode).
    pub fn decode_version(bytes: &[u8]) -> Result<u8, LogError> {
        let mut reader = Reader { bytes, at: 0 };
        let (Some(magic), Some(version)) = (reader.u16(), reader.u8()) else {
            return Err(LogError::ShortBuffer);
        };
        if magic != ENTRY_MAGIC {
            return Err(LogError::NotAnEntry);
        }
        Ok(version)
    }

    /// Reads an entry from `bytes`.
    ///
    /// # Errors
    ///
    /// [`LogError::ShortBuffer`] when `bytes` is shorter than [`ENTRY_BYTES`],
    /// [`LogError::NotAnEntry`] when the magic, the reserved field, the outcome code or the
    /// check does not hold, and [`LogError::UnknownVersion`] for a format this build does not
    /// read.
    pub fn decode(bytes: &[u8]) -> Result<Self, LogError> {
        Self::decode_with::<Catalogued>(bytes)
    }

    /// Reads an entry from `bytes`, verified with `C`.
    ///
    /// # Errors
    ///
    /// As [`decode`](Self::decode).
    pub fn decode_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<Self, LogError> {
        let Some(slot) = bytes.get(..ENTRY_BYTES) else {
            return Err(LogError::ShortBuffer);
        };
        let (body, seal) = slot.split_at(ENTRY_BODY_BYTES);
        let Ok(seal) = <[u8; 4]>::try_from(seal) else {
            return Err(LogError::NotAnEntry);
        };
        if C::frame_check(body) != u32::from_le_bytes(seal) {
            return Err(LogError::NotAnEntry);
        }

        let mut reader = Reader { bytes: body, at: 0 };
        let (
            Some(magic),
            Some(version),
            Some(code),
            Some(seed),
            Some(iteration),
            Some(effects),
            Some(reserved),
        ) = (
            reader.u16(),
            reader.u8(),
            reader.u8(),
            reader.u64(),
            reader.u32(),
            reader.u16(),
            reader.u16(),
        )
        else {
            return Err(LogError::NotAnEntry);
        };
        if magic != ENTRY_MAGIC || reserved != 0 {
            return Err(LogError::NotAnEntry);
        }
        if version != Self::FORMAT_VERSION {
            return Err(LogError::UnknownVersion { version });
        }
        let (Some(capacity), Some(erase_size), Some(program_size), Some(read_size), Some(detail)) = (
            reader.u32(),
            reader.u32(),
            reader.u32(),
            reader.u32(),
            reader.u32(),
        ) else {
            return Err(LogError::NotAnEntry);
        };
        let Some(wear) = reader.wear() else {
            return Err(LogError::NotAnEntry);
        };
        let Some(outcome) = Outcome::from_parts(code, detail) else {
            return Err(LogError::NotAnEntry);
        };

        Ok(Self {
            seed,
            iteration,
            capacity,
            erase_size,
            program_size,
            read_size,
            effects,
            outcome,
            wear,
        })
    }

    /// Renders a line: a human-readable prefix, then the entry in hex.
    ///
    /// # Errors
    ///
    /// [`LogError::ShortBuffer`] if `out` is shorter than [`LINE_BYTES`](Self::LINE_BYTES).
    pub fn render(self, out: &mut [u8]) -> Result<&[u8], LogError> {
        let cut = self.cut();
        let mut at = 0_usize;
        let mut put = |text: &str| -> Option<()> {
            let end = at.checked_add(text.len())?;
            let slot = out.get_mut(at..end)?;
            slot.copy_from_slice(text.as_bytes());
            at = end;
            Some(())
        };
        let laid_out = (|| {
            put(LINE_PREFIX)?;
            put(cut.phase().name())?;
            put(" ")?;
            put(cut.cause().name())?;
            put(" ")?;
            put(self.outcome.name())?;
            put(" ")
        })();
        if laid_out.is_none() {
            return Err(LogError::ShortBuffer);
        }

        let mut encoded = [0_u8; ENTRY_BYTES];
        self.encode(&mut encoded)?;
        let end = at
            .checked_add(2 * ENTRY_BYTES)
            .ok_or(LogError::ShortBuffer)?;
        let Some(hex) = out.get_mut(at..end) else {
            return Err(LogError::ShortBuffer);
        };
        for (pair, byte) in hex.chunks_exact_mut(2).zip(encoded) {
            let Some((high, low)) = pair.split_first_mut() else {
                return Err(LogError::ShortBuffer);
            };
            *high = nibble(byte >> 4);
            let Some(low) = low.first_mut() else {
                return Err(LogError::ShortBuffer);
            };
            *low = nibble(byte & 0x0F);
        }
        out.get(..end).ok_or(LogError::ShortBuffer)
    }

    /// Reads back a line [`render`](Self::render) produced.
    ///
    /// The hex suffix is the whole of what is read; the human-readable prefix is skipped, so
    /// a renderer that got its own prose wrong still hands back the truth.
    ///
    /// # Errors
    ///
    /// [`LogError::NotAnEntry`] for a line that is not one, and whatever
    /// [`decode`](Self::decode) refuses the bytes with.
    pub fn parse(line: &[u8]) -> Result<Self, LogError> {
        let Some(rest) = line.strip_prefix(LINE_PREFIX.as_bytes()) else {
            return Err(LogError::NotAnEntry);
        };
        let tail = rest
            .iter()
            .rposition(|byte| *byte == b' ')
            .map_or(rest, |position| {
                rest.get(position.saturating_add(1)..).unwrap_or_default()
            });
        if tail.len() != 2 * ENTRY_BYTES {
            return Err(LogError::NotAnEntry);
        }
        let mut bytes = [0_u8; ENTRY_BYTES];
        for (slot, pair) in bytes.iter_mut().zip(tail.chunks_exact(2)) {
            let (Some(high), Some(low)) = (pair.first(), pair.get(1)) else {
                return Err(LogError::NotAnEntry);
            };
            let (Some(high), Some(low)) = (unnibble(*high), unnibble(*low)) else {
                return Err(LogError::NotAnEntry);
            };
            *slot = (high << 4) | low;
        }
        Self::decode(&bytes)
    }
}

/// The lower-case hex digit for a nibble.
const fn nibble(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        10..=15 => b'a' + (value - 10),
        _ => b'0',
    }
}

/// The nibble a lower-case hex digit names.
const fn unnibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(10 + digit - b'a'),
        _ => None,
    }
}

// The entry layout is arithmetic the codec trusts, so it is checked where a mistake is a
// compile error rather than a test run.
const _: () = assert!(ENTRY_BYTES == ENTRY_BODY_BYTES + 4);
const _: () = assert!(ENTRY_MAGIC != 0x0000 && ENTRY_MAGIC != 0xFFFF);
