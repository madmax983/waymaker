//! Section sizes, read straight out of the linked ELF.
//!
//! The size budgets are measured from section headers, and this module is what reads them.
//! It parses rather than shells out to `llvm-size` or `arm-none-eabi-size` for two
//! reasons. The first is that a gate whose measurement depends on a binary that may or may
//! not be installed is a gate that reports "tool missing" on the day it matters. The
//! second is the one the rest of this crate is built around: a parser is a pure function
//! over bytes, so the awkward cases — a truncated table, a name offset pointing outside
//! the string table, an extended section count — can be tested against images that no
//! linker would produce, rather than hoped about.
//!
//! Only the section header table is read. Symbols are not, which is what makes this work
//! against the `strip = "symbols"` release profile the budgets are measured with:
//! stripping removes the symbol table and leaves the section headers alone.

use core::fmt;

/// `SHF_WRITE`: the section is writable at run time, so it lives in RAM.
pub const SHF_WRITE: u64 = 0x1;

/// `SHF_ALLOC`: the section occupies memory in the running image.
pub const SHF_ALLOC: u64 = 0x2;

/// `SHF_EXECINSTR`: the section holds executable instructions.
pub const SHF_EXECINSTR: u64 = 0x4;

/// `SHT_NOBITS`: the section occupies no space in the file, only in memory.
pub const SHT_NOBITS: u32 = 8;

/// `SHN_UNDEF`: the symbol is defined in no section of this image.
pub const SHN_UNDEF: u16 = 0;

/// `SHT_SYMTAB`: the section is a symbol table.
pub const SHT_SYMTAB: u32 = 2;

/// `SHT_STRTAB`: the section is a string table.
pub const SHT_STRTAB: u32 = 3;

/// The first section index that is reserved rather than naming a section.
///
/// `SHN_ABS`, `SHN_COMMON` and `SHN_XINDEX` live at or above it, and a symbol carrying one
/// of them is in no section of this image. Public because a caller bucketing symbols by
/// section has to be able to say so: `sections.get(index)` happens to answer `None` for
/// every one of them today, and stops doing so in an image with 0xff00 sections or more,
/// which [`sections`] can read.
///
/// A file with this many sections or more stores the real count and the real string-table
/// index in the otherwise unused first section header.
pub const SHN_LORESERVE: u16 = 0xff00;

/// One section of a linked image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The section name, for example `.text`.
    pub name: String,
    /// `sh_size`: the section's size in bytes.
    pub size: u64,
    /// `sh_type`.
    pub kind: u32,
    /// `sh_flags`.
    pub flags: u64,
}

impl Section {
    /// Whether the section occupies memory in the running image.
    #[must_use]
    pub const fn allocated(&self) -> bool {
        self.flags & SHF_ALLOC != 0
    }

    /// Whether the section is writable, and therefore lives in RAM.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.flags & SHF_WRITE != 0
    }

    /// Whether the section's bytes are stored in the image rather than only reserved.
    ///
    /// This is the flash question: `.text`, `.rodata` and `.data` all cost storage,
    /// `.bss` does not, and the difference is `SHT_NOBITS` rather than the section name.
    /// Naming is a convention a linker script can change; the type is not.
    #[must_use]
    pub const fn occupies_storage(&self) -> bool {
        self.allocated() && self.kind != SHT_NOBITS
    }
}

/// The image could not be read, so its sizes are unknown rather than zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfError {
    message: String,
}

impl ElfError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ElfError {}

/// Which byte order the header fields are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// Where a field sits, and how wide it is, for one ELF class.
///
/// One table per class, read by both the parser and the synthetic-image builder in
/// [`tests_support`]. Shared rather than written twice: two copies of the same offsets can
/// be wrong in the same direction, and the tests would then agree with the parser about a
/// layout no linker uses.
#[derive(Debug, Clone, Copy)]
struct Layout {
    /// The size of the file header, and so the offset the section names can start at.
    header_size: usize,
    /// The size of one section header entry. An image declaring less than this is
    /// malformed: its entries overlap.
    entry_size: usize,
    section_header_offset: usize,
    section_entry_size: usize,
    section_count: usize,
    string_table_index: usize,
    /// Offsets within one section header entry.
    sh_name: usize,
    sh_type: usize,
    sh_flags: usize,
    sh_offset: usize,
    sh_size: usize,
    sh_link: usize,
    sh_entsize: usize,
    /// Offsets within one symbol table entry.
    st_name: usize,
    st_value: usize,
    st_shndx: usize,
    st_size: usize,
    /// The size of one symbol table entry.
    symbol_entry_size: usize,
    /// Whether the class's address-width fields are 64 bits wide: `sh_flags`,
    /// `sh_offset`, `sh_size`, `sh_entsize`, and a symbol's `st_value` and `st_size`.
    wide: bool,
}

const ELF32: Layout = Layout {
    header_size: 0x34,
    entry_size: 0x28,
    section_header_offset: 0x20,
    section_entry_size: 0x2e,
    section_count: 0x30,
    string_table_index: 0x32,
    sh_name: 0x00,
    sh_type: 0x04,
    sh_flags: 0x08,
    sh_offset: 0x10,
    sh_size: 0x14,
    sh_link: 0x18,
    sh_entsize: 0x24,
    st_name: 0x00,
    st_value: 0x04,
    st_shndx: 0x0e,
    st_size: 0x08,
    symbol_entry_size: 0x10,
    wide: false,
};

const ELF64: Layout = Layout {
    header_size: 0x40,
    entry_size: 0x40,
    section_header_offset: 0x28,
    section_entry_size: 0x3a,
    section_count: 0x3c,
    string_table_index: 0x3e,
    sh_name: 0x00,
    sh_type: 0x04,
    sh_flags: 0x08,
    sh_offset: 0x18,
    sh_size: 0x20,
    sh_link: 0x28,
    sh_entsize: 0x38,
    st_name: 0x00,
    st_value: 0x08,
    st_shndx: 0x06,
    st_size: 0x10,
    symbol_entry_size: 0x18,
    wide: true,
};

/// `EM_ARM`: the machine the resource budgets are stated for.
pub const EM_ARM: u16 = 0x28;

/// Where `e_machine` sits, which is the same in both classes.
const E_MACHINE: usize = 0x12;

/// A raw section header, before its name has been resolved.
#[derive(Debug, Clone, Copy)]
struct RawSection {
    name_offset: u32,
    kind: u32,
    flags: u64,
    /// `sh_offset`: where the section's bytes start in the file.
    offset: u64,
    size: u64,
    /// `sh_link`. On the first header it carries the extended string-table index; on a
    /// symbol table it names the string table its names live in.
    link: u32,
    /// `sh_entsize`: the width of one entry, for a section that holds a table.
    entry_size: u64,
}

/// Every section of the image at `bytes`, in section header order.
///
/// # Errors
///
/// Returns [`ElfError`] if the bytes are not an ELF image this can read, if the section
/// header table is truncated or absent, or if a section name points outside the string
/// table. Every one of those fails closed: an image whose sections cannot be read is an
/// image whose size is unknown, and reporting zero for it would pass every budget.
pub fn sections(bytes: &[u8]) -> Result<Vec<Section>, ElfError> {
    let (layout, endian) = identify(bytes)?;
    let table = locate_table(bytes, layout, endian)?;

    let mut raw = Vec::with_capacity(table.count);
    for index in 0..table.count {
        raw.push(read_section(
            bytes,
            table.offset,
            table.entry_size,
            index,
            layout,
            endian,
        )?);
    }

    let strings = string_table(bytes, &raw, &table, layout, endian)?;

    raw.into_iter()
        .map(|section| {
            Ok(Section {
                name: read_name(strings, section.name_offset, "section name")?,
                size: section.size,
                kind: section.kind,
                flags: section.flags,
            })
        })
        .collect()
}

/// One symbol of a linked image, as its symbol table records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The symbol name, still mangled.
    pub name: String,
    /// `st_value`: where the symbol sits, which is what identifies it.
    ///
    /// Two symbols can share a name after mangling collides, and any number share a size.
    /// A caller pairing this table against another reader's output has to pair on
    /// something, and for a defined symbol this is the thing that is unique.
    pub address: u64,
    /// `st_size`: how many bytes the symbol occupies.
    pub size: u64,
    /// `st_shndx`: which section the bytes are in, indexed as [`sections`] returns them.
    pub section_index: u16,
}

/// Every symbol of every symbol table in the image at `bytes`, in table order.
///
/// An image with no symbol table has no symbols, which is not an error here: whether a
/// missing table is fatal is a question for the gate that asked, and this reads the format.
///
/// `st_shndx` is reported as written. A reserved index — [`SHN_UNDEF`], `SHN_ABS`,
/// `SHN_COMMON`, or `SHN_XINDEX`, which defers to a `SHT_SYMTAB_SHNDX` section this does
/// not read — names no section of this image, and a caller bucketing symbols by section is
/// expected to say so with [`SHN_LORESERVE`]. Not resolved rather than refused, because a
/// symbol in no section occupies no bytes of one: the size gate's use of this
/// under-attributes such a symbol, which charges its bytes to the layers and errs toward
/// failing the budget.
///
/// # Errors
///
/// Returns [`ElfError`] if the bytes are not an ELF image this can read, if a symbol table
/// is truncated, if its entries are not the width this class declares, or if a symbol name
/// points outside its string table. Every one fails closed, for [`sections`]' reason: a
/// table that cannot be read is a table whose sizes are unknown, and reading the entries
/// that happen to fit would under-report every total taken from it.
pub fn symbols(bytes: &[u8]) -> Result<Vec<Symbol>, ElfError> {
    let (layout, endian) = identify(bytes)?;
    let table = locate_table(bytes, layout, endian)?;

    let mut raw = Vec::with_capacity(table.count);
    for index in 0..table.count {
        raw.push(read_section(
            bytes,
            table.offset,
            table.entry_size,
            index,
            layout,
            endian,
        )?);
    }

    let mut found = Vec::new();
    for section in raw.iter().filter(|section| section.kind == SHT_SYMTAB) {
        // A declared width other than this class's is a table whose fields sit somewhere
        // else, so reading it at these offsets would answer with well-formed nonsense.
        // Zero means "not a table of fixed-width entries", which a symbol table is.
        if section.entry_size != layout.symbol_entry_size as u64 {
            return Err(ElfError::new(format!(
                "a symbol table declares {} byte entries, but this ELF class writes {} byte ones",
                section.entry_size, layout.symbol_entry_size
            )));
        }
        let names = raw
            .get(section.link as usize)
            .ok_or_else(|| ElfError::new("a symbol table names no string table"))?;
        // A `sh_link` that names any other section reads as a string table of whatever
        // bytes are there, and every symbol comes back with a garbage name — which the
        // attribution above this then reports as zero rather than as an error.
        if names.kind != SHT_STRTAB {
            return Err(ElfError::new(format!(
                "a symbol table's `sh_link` names section type {} rather than a string table",
                names.kind
            )));
        }
        let strings = table_bytes(bytes, names, "the symbol name string table")?;
        let entries = table_bytes(bytes, section, "a symbol table")?;
        if entries.len() % layout.symbol_entry_size != 0 {
            return Err(ElfError::new(format!(
                "a symbol table is {} bytes, which is not a whole number of {} byte entries",
                entries.len(),
                layout.symbol_entry_size
            )));
        }
        for entry in entries.chunks_exact(layout.symbol_entry_size) {
            found.push(Symbol {
                name: read_name(
                    strings,
                    read_u32(entry, layout.st_name, endian)?,
                    "symbol name",
                )?,
                address: read_address(entry, layout.st_value, endian, layout.wide)?,
                size: read_address(entry, layout.st_size, endian, layout.wide)?,
                section_index: read_u16(entry, layout.st_shndx, endian)?,
            });
        }
    }

    Ok(found)
}

/// The bytes one section holds, or an error naming it.
fn table_bytes<'a>(
    bytes: &'a [u8],
    section: &RawSection,
    what: &str,
) -> Result<&'a [u8], ElfError> {
    let offset = usize::try_from(section.offset)
        .map_err(|_| ElfError::new(format!("{what} starts beyond addressable memory")))?;
    let len = usize::try_from(section.size)
        .map_err(|_| ElfError::new(format!("{what} is larger than memory")))?;
    bytes
        .get(offset..offset.saturating_add(len))
        .ok_or_else(|| ElfError::new(format!("{what} is truncated")))
}

/// The machine the image is for, from `e_machine`.
///
/// Read separately from [`sections`], which is deliberately machine-agnostic: the parser's
/// job is to read the format, and deciding which machine is the right one belongs to the
/// gate that asked. Without this the section sizes of a host executable are perfectly
/// readable and perfectly plausible.
///
/// # Errors
///
/// Returns [`ElfError`] if the bytes are not an ELF image.
pub fn machine(bytes: &[u8]) -> Result<u16, ElfError> {
    let (_, endian) = identify(bytes)?;
    read_u16(bytes, E_MACHINE, endian)
}

/// The class and byte order the header fields are written in.
fn identify(bytes: &[u8]) -> Result<(Layout, Endian), ElfError> {
    if bytes.get(..4) != Some(b"\x7fELF") {
        return Err(ElfError::new(
            "the file does not start with the ELF magic number, so it is not a linked image",
        ));
    }

    let layout = match bytes.get(4) {
        Some(1) => ELF32,
        Some(2) => ELF64,
        other => {
            return Err(ElfError::new(format!(
                "unknown ELF class {other:?}; only 32-bit and 64-bit images can be measured"
            )));
        }
    };
    let endian = match bytes.get(5) {
        Some(1) => Endian::Little,
        Some(2) => Endian::Big,
        other => {
            return Err(ElfError::new(format!(
                "unknown ELF byte order {other:?}; only little- and big-endian images can be measured"
            )));
        }
    };
    Ok((layout, endian))
}

/// Where the section header table is, how big it is, and which entry names the others.
#[derive(Debug, Clone, Copy)]
struct Table {
    offset: usize,
    entry_size: usize,
    count: usize,
    string_table_index: usize,
}

fn locate_table(bytes: &[u8], layout: Layout, endian: Endian) -> Result<Table, ElfError> {
    let offset = usize::try_from(read_address(
        bytes,
        layout.section_header_offset,
        endian,
        layout.wide,
    )?)
    .map_err(|_| ElfError::new("the section header table starts beyond addressable memory"))?;
    if offset == 0 {
        return Err(ElfError::new(
            "the image has no section header table, so its section sizes cannot be measured",
        ));
    }
    // The table cannot begin inside the file header it is described by. An image saying it
    // does is malformed, and reading it would interpret the header's own bytes as section
    // sizes — which is a measurement, just not of anything.
    if offset < layout.header_size {
        return Err(ElfError::new(format!(
            "the section header table starts at {offset}, inside the {} byte file header",
            layout.header_size
        )));
    }

    // Checked against the class's real entry size, not merely against zero. A smaller
    // `e_shentsize` makes the entries overlap, and — worse — makes the whole-table bounds
    // check below multiply by a number smaller than the entries it is meant to bound, so
    // a crafted image could pass it while its trailing fields fall off the end.
    let entry_size = usize::from(read_u16(bytes, layout.section_entry_size, endian)?);
    if entry_size < layout.entry_size {
        return Err(ElfError::new(format!(
            "the section header entries are {entry_size} bytes, but this ELF class has {} byte entries; the file is malformed or is not the class its header claims",
            layout.entry_size
        )));
    }

    let declared_count = read_u16(bytes, layout.section_count, endian)?;
    let declared_index = read_u16(bytes, layout.string_table_index, endian)?;

    // The first header is read on its own because, in an image with 0xff00 sections or
    // more, it is where the real count and the real string-table index live.
    let first = read_section(bytes, offset, entry_size, 0, layout, endian)?;
    let count = if declared_count == 0 {
        usize::try_from(first.size)
            .map_err(|_| ElfError::new("the extended section count does not fit in memory"))?
    } else {
        usize::from(declared_count)
    };
    let string_table_index = if declared_index >= SHN_LORESERVE {
        usize::try_from(first.link)
            .map_err(|_| ElfError::new("the extended string-table index does not fit in memory"))?
    } else {
        usize::from(declared_index)
    };

    // Checked as a whole rather than field by field: an entry whose trailing fields fall
    // off the end of the file is a truncated image even when every field this module
    // happens to read survives, and an image that is only accidentally readable is not one
    // to measure a budget from.
    let end = count
        .checked_mul(entry_size)
        .and_then(|len| len.checked_add(offset))
        .ok_or_else(|| ElfError::new("the section header table overflows the address space"))?;
    if end > bytes.len() {
        return Err(ElfError::new(format!(
            "the section header table is truncated: {count} entries of {entry_size} bytes end at {end}, but the file is {} bytes",
            bytes.len()
        )));
    }

    // The first entry is the reserved null section, which every image has and which
    // describes nothing. A table holding only that one is what `llvm-objcopy
    // --strip-sections` leaves behind, and it parses perfectly: every size reads zero, and
    // zero passes every budget.
    if count <= 1 {
        return Err(ElfError::new(
            "the image has only the reserved null section header, so it carries no section sizes to measure; its section headers were probably stripped",
        ));
    }

    Ok(Table {
        offset,
        entry_size,
        count,
        string_table_index,
    })
}

/// The bytes of the section name string table.
fn string_table<'a>(
    bytes: &'a [u8],
    raw: &[RawSection],
    table: &Table,
    layout: Layout,
    endian: Endian,
) -> Result<&'a [u8], ElfError> {
    let header = raw
        .get(table.string_table_index)
        .ok_or_else(|| ElfError::new("the section name string table is not in the header table"))?;
    let at = section_field(
        table.offset,
        table.entry_size,
        table.string_table_index,
        layout.sh_offset,
    )?;
    let offset = usize::try_from(read_address(bytes, at, endian, layout.wide)?).map_err(|_| {
        ElfError::new("the section name string table starts beyond addressable memory")
    })?;
    let len = usize::try_from(header.size)
        .map_err(|_| ElfError::new("the section name string table is larger than memory"))?;
    bytes
        .get(offset..offset.saturating_add(len))
        .ok_or_else(|| ElfError::new("the section name string table is truncated"))
}

/// Where `field` of section `index` sits in the file.
fn section_field(
    table_offset: usize,
    entry_size: usize,
    index: usize,
    field: usize,
) -> Result<usize, ElfError> {
    index
        .checked_mul(entry_size)
        .and_then(|at| at.checked_add(table_offset))
        .and_then(|at| at.checked_add(field))
        .ok_or_else(|| ElfError::new("the section header table overflows the address space"))
}

fn read_section(
    bytes: &[u8],
    table_offset: usize,
    entry_size: usize,
    index: usize,
    layout: Layout,
    endian: Endian,
) -> Result<RawSection, ElfError> {
    Ok(RawSection {
        name_offset: read_u32(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_name)?,
            endian,
        )?,
        kind: read_u32(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_type)?,
            endian,
        )?,
        flags: read_address(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_flags)?,
            endian,
            layout.wide,
        )?,
        offset: read_address(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_offset)?,
            endian,
            layout.wide,
        )?,
        size: read_address(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_size)?,
            endian,
            layout.wide,
        )?,
        link: read_u32(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_link)?,
            endian,
        )?,
        entry_size: read_address(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_entsize)?,
            endian,
            layout.wide,
        )?,
    })
}

/// The NUL-terminated name at `offset` in the string table `strings`.
///
/// `what` names the thing being read, because the same table walk answers for a section
/// name and for a symbol name and a message that guessed wrong sends a reader to the wrong
/// half of the image.
fn read_name(strings: &[u8], offset: u32, what: &str) -> Result<String, ElfError> {
    let offset = usize::try_from(offset)
        .map_err(|_| ElfError::new(format!("a {what} offset does not fit in memory")))?;
    // `>=`, not `>`: `strings.get(len..)` is an empty slice rather than `None`, so an
    // offset one past the end would come back as a nameless section instead of an error,
    // and the per-section breakdown would silently under-report while `flash` stayed right.
    if offset >= strings.len() {
        return Err(ElfError::new(format!(
            "a {what} offset ({offset}) points outside the {} byte string table",
            strings.len()
        )));
    }
    let rest = strings
        .get(offset..)
        .ok_or_else(|| ElfError::new(format!("a {what} offset is out of range")))?;
    // A name with no terminator is a truncated table, not a name that runs to the end of
    // it. Accepting the tail would answer with one enormous name and no error, which is the
    // same failure the `>=` above refuses one line at a time.
    let end = rest.iter().position(|byte| *byte == 0).ok_or_else(|| {
        ElfError::new(format!(
            "a {what} at offset {offset} has no terminator before the end of the string table"
        ))
    })?;
    let name = rest
        .get(..end)
        .ok_or_else(|| ElfError::new(format!("a {what} is truncated")))?;
    String::from_utf8(name.to_vec())
        .map_err(|err| ElfError::new(format!("a {what} is not valid UTF-8: {err}")))
}

fn read_u16(bytes: &[u8], at: usize, endian: Endian) -> Result<u16, ElfError> {
    let raw: [u8; 2] = slice(bytes, at, 2)?
        .try_into()
        .map_err(|_| ElfError::new("a 16-bit header field is truncated"))?;
    Ok(match endian {
        Endian::Little => u16::from_le_bytes(raw),
        Endian::Big => u16::from_be_bytes(raw),
    })
}

fn read_u32(bytes: &[u8], at: usize, endian: Endian) -> Result<u32, ElfError> {
    let raw: [u8; 4] = slice(bytes, at, 4)?
        .try_into()
        .map_err(|_| ElfError::new("a 32-bit header field is truncated"))?;
    Ok(match endian {
        Endian::Little => u32::from_le_bytes(raw),
        Endian::Big => u32::from_be_bytes(raw),
    })
}

fn read_u64(bytes: &[u8], at: usize, endian: Endian) -> Result<u64, ElfError> {
    let raw: [u8; 8] = slice(bytes, at, 8)?
        .try_into()
        .map_err(|_| ElfError::new("a 64-bit header field is truncated"))?;
    Ok(match endian {
        Endian::Little => u64::from_le_bytes(raw),
        Endian::Big => u64::from_be_bytes(raw),
    })
}

/// A field that is 32 bits wide in an ELF32 image and 64 bits wide in an ELF64 one.
fn read_address(bytes: &[u8], at: usize, endian: Endian, wide: bool) -> Result<u64, ElfError> {
    if wide {
        read_u64(bytes, at, endian)
    } else {
        read_u32(bytes, at, endian).map(u64::from)
    }
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], ElfError> {
    let end = at
        .checked_add(len)
        .ok_or_else(|| ElfError::new("a header field starts beyond the address space"))?;
    bytes.get(at..end).ok_or_else(|| {
        ElfError::new(format!(
            "the section header table is truncated: {len} byte(s) wanted at offset {at}, but the file is {} bytes",
            bytes.len()
        ))
    })
}

/// Builders for ELF images that no linker would produce.
///
/// `#[cfg(test)]` like [`crate::pipeline::tests_support`], and for the same reason: it is
/// reached from another module's test code, which is a use the attribute permits, so
/// shipping ~350 lines of ELF forgery in the binary and in `cargo doc` buys nothing.
#[cfg(test)]
pub mod tests_support {
    use super::{Endian, SHT_NOBITS};

    /// Which width to write the header fields at.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Class {
        /// 32-bit headers, as `thumbv6m-none-eabi` produces.
        Elf32,
        /// 64-bit headers.
        Elf64,
    }

    impl Class {
        /// The parser's own table for this class.
        ///
        /// Read from `super` rather than restated. A builder with its own copy of the
        /// offsets can be wrong in the same direction as the parser, and then every test
        /// agrees with the parser about a layout no linker uses — which is precisely the
        /// failure a synthetic-image test exists to rule out.
        const fn layout(self) -> super::Layout {
            match self {
                Self::Elf32 => super::ELF32,
                Self::Elf64 => super::ELF64,
            }
        }

        const fn header_size(self) -> usize {
            self.layout().header_size
        }

        const fn entry_size(self) -> usize {
            self.layout().entry_size
        }
    }

    /// One section to write into a synthetic image.
    #[derive(Debug, Clone)]
    pub struct SectionSpec {
        name: String,
        size: u64,
        flags: u64,
        kind: u32,
        name_offset: Option<u32>,
        /// Bytes to lay down in the file, for a section a reader actually reads.
        contents: Option<Vec<u8>>,
        /// `sh_link`.
        link: u32,
        /// `sh_info`. For a symbol table, one past the index of the last local symbol.
        info: u32,
        /// `sh_entsize`.
        entry_size: u64,
    }

    impl SectionSpec {
        /// A section whose bytes are stored in the file.
        #[must_use]
        pub fn progbits(name: &str, size: u64, flags: u64) -> Self {
            Self {
                name: name.to_owned(),
                size,
                flags,
                kind: 1,
                name_offset: None,
                contents: None,
                link: 0,
                info: 0,
                entry_size: 0,
            }
        }

        /// A string table, which is what a section holding names has to be.
        fn strings(name: &str) -> Self {
            Self {
                kind: super::SHT_STRTAB,
                ..Self::progbits(name, 0, 0)
            }
        }

        /// A section carrying real bytes, whose size is those bytes.
        fn holding(name: &str, kind: u32, contents: Vec<u8>) -> Self {
            Self {
                size: contents.len() as u64,
                kind,
                contents: Some(contents),
                ..Self::progbits(name, 0, 0)
            }
        }

        /// A section that is reserved in memory but stored nowhere, such as `.bss`.
        #[must_use]
        pub fn nobits(name: &str, size: u64, flags: u64) -> Self {
            Self {
                kind: SHT_NOBITS,
                ..Self::progbits(name, size, flags)
            }
        }

        /// Writes a name offset that does not point at this section's name.
        #[must_use]
        pub const fn with_name_offset(mut self, offset: u32) -> Self {
            self.name_offset = Some(offset);
            self
        }
    }

    /// One symbol to write into a synthetic image's symbol table.
    #[derive(Debug, Clone)]
    pub struct SymbolSpec {
        name: String,
        address: u64,
        size: u64,
        section_index: u16,
        name_offset: Option<u32>,
    }

    impl SymbolSpec {
        /// A symbol of `size` bytes at `address`, in the section at `section_index`.
        #[must_use]
        pub fn new(name: &str, address: u64, size: u64, section_index: u16) -> Self {
            Self {
                name: name.to_owned(),
                address,
                size,
                section_index,
                name_offset: None,
            }
        }

        /// Writes a name offset that does not point at this symbol's name.
        #[must_use]
        pub const fn with_name_offset(mut self, offset: u32) -> Self {
            self.name_offset = Some(offset);
            self
        }
    }

    /// Assembles a minimal but well-formed ELF image around a list of sections.
    #[derive(Debug, Clone)]
    pub struct ElfBuilder {
        class: Class,
        endian: Endian,
        sections: Vec<SectionSpec>,
        symbols: Vec<SymbolSpec>,
        section_headers: bool,
        extended_counts: bool,
    }

    impl ElfBuilder {
        /// An image with a null section and a section name string table, and nothing else.
        #[must_use]
        pub const fn new(class: Class) -> Self {
            Self {
                class,
                endian: Endian::Little,
                sections: Vec::new(),
                symbols: Vec::new(),
                section_headers: true,
                extended_counts: false,
            }
        }

        /// Writes the header fields most significant byte first.
        #[must_use]
        pub const fn big_endian(mut self) -> Self {
            self.endian = Endian::Big;
            self
        }

        /// Writes `e_shoff = 0`, as a file with no section header table has.
        #[must_use]
        pub const fn without_section_headers(mut self) -> Self {
            self.section_headers = false;
            self
        }

        /// Moves the section count and string-table index into the first section header,
        /// as a file with `SHN_LORESERVE` sections or more must.
        #[must_use]
        pub const fn with_extended_counts(mut self) -> Self {
            self.extended_counts = true;
            self
        }

        /// Adds a section.
        #[must_use]
        pub fn with(mut self, section: SectionSpec) -> Self {
            self.sections.push(section);
            self
        }

        /// Adds a `.symtab` and the `.strtab` its names live in.
        #[must_use]
        pub fn with_symbols(mut self, symbols: Vec<SymbolSpec>) -> Self {
            self.symbols = symbols;
            self
        }

        /// Renders the image.
        #[must_use]
        pub fn build(&self) -> Vec<u8> {
            let sections = self.all_sections();
            let (strings, offsets) = Self::string_table(&sections);
            let string_table_index = sections.len() + 1;
            let strings_offset = self.class.header_size();

            // Every section that carries bytes is laid down after the section names, and
            // the header table goes last, so a reader that follows `sh_offset` finds them.
            let contents_offset = strings_offset + strings.len();
            let mut contents = Vec::new();
            let mut content_offsets = Vec::new();
            for section in &sections {
                content_offsets.push(section.contents.as_ref().map_or(0, |bytes| {
                    let at = contents_offset + contents.len();
                    contents.extend_from_slice(bytes);
                    u64::try_from(at).unwrap_or(0)
                }));
            }
            let table_offset = contents_offset + contents.len();

            let mut image =
                self.file_header(table_offset, strings.len(), string_table_index, &sections);
            image.extend_from_slice(&strings);
            image.extend_from_slice(&contents);
            image.extend_from_slice(&self.section_headers(
                &sections,
                &offsets,
                &content_offsets,
                string_table_index,
                strings_offset,
                strings.len(),
            ));
            image
        }

        /// The sections written into the image: the declared ones, plus the symbol table
        /// and its string table when there are symbols.
        fn all_sections(&self) -> Vec<SectionSpec> {
            let mut sections = self.sections.clone();
            if self.symbols.is_empty() {
                return sections;
            }
            // Header indices: 0 is the null section, so the declared sections are 1..=n,
            // the symbol table is n+1 and its string table n+2.
            let strtab_index = u32::try_from(sections.len() + 2).unwrap_or(0);
            let (symtab, strtab) = self.symbol_table();
            sections.push(SectionSpec {
                link: strtab_index,
                // Every symbol this builder writes is local, so `sh_info` is the whole
                // table. A zero here makes a real reader warn once per symbol.
                info: u32::try_from(self.symbols.len() + 1).unwrap_or(0),
                entry_size: self.class.layout().symbol_entry_size as u64,
                ..SectionSpec::holding(".symtab", super::SHT_SYMTAB, symtab)
            });
            sections.push(SectionSpec::holding(".strtab", super::SHT_STRTAB, strtab));
            sections
        }

        /// The symbol table and the string table its names live in.
        fn symbol_table(&self) -> (Vec<u8>, Vec<u8>) {
            let layout = self.class.layout();
            let mut strings = vec![0_u8];
            // A real table opens with an all-zero symbol at index 0.
            let mut entries = vec![0_u8; layout.symbol_entry_size];
            for symbol in &self.symbols {
                let name_offset = symbol
                    .name_offset
                    .unwrap_or_else(|| u32::try_from(strings.len()).unwrap_or(0));
                strings.extend_from_slice(symbol.name.as_bytes());
                strings.push(0);

                let mut entry = vec![0_u8; layout.symbol_entry_size];
                write_u32(&mut entry, layout.st_name, name_offset, self.endian);
                write_address(
                    &mut entry,
                    layout.st_value,
                    symbol.address,
                    self.endian,
                    self.class == Class::Elf64,
                );
                write_address(
                    &mut entry,
                    layout.st_size,
                    symbol.size,
                    self.endian,
                    self.class == Class::Elf64,
                );
                write_u16(
                    &mut entry,
                    layout.st_shndx,
                    symbol.section_index,
                    self.endian,
                );
                entries.extend_from_slice(&entry);
            }
            (entries, strings)
        }

        /// The section name string table, and each section's offset into it.
        fn string_table(sections: &[SectionSpec]) -> (Vec<u8>, Vec<u32>) {
            let mut strings = vec![0_u8];
            let mut offsets = Vec::new();
            let shstrtab = SectionSpec::strings(".shstrtab");
            for section in sections.iter().chain(core::iter::once(&shstrtab)) {
                offsets.push(u32::try_from(strings.len()).unwrap_or(0));
                strings.extend_from_slice(section.name.as_bytes());
                strings.push(0);
            }
            (strings, offsets)
        }

        /// The ELF file header.
        fn file_header(
            &self,
            table_offset: usize,
            _strings_len: usize,
            string_table_index: usize,
            sections: &[SectionSpec],
        ) -> Vec<u8> {
            let header_size = self.class.header_size();
            let wide = self.class == Class::Elf64;
            let count = sections.len() + 2;

            let mut image = vec![0_u8; header_size];
            write_bytes(&mut image, 0, b"\x7fELF");
            write_bytes(
                &mut image,
                4,
                &[
                    match self.class {
                        Class::Elf32 => 1,
                        Class::Elf64 => 2,
                    },
                    match self.endian {
                        Endian::Little => 1,
                        Endian::Big => 2,
                    },
                    1,
                ],
            );

            let layout = self.class.layout();
            let (shoff_at, shentsize_at, shnum_at, shstrndx_at) = (
                layout.section_header_offset,
                layout.section_entry_size,
                layout.section_count,
                layout.string_table_index,
            );
            let shoff = if self.section_headers {
                u64::try_from(table_offset).unwrap_or(0)
            } else {
                0
            };
            write_address(&mut image, shoff_at, shoff, self.endian, wide);
            write_u16(
                &mut image,
                shentsize_at,
                u16::try_from(self.class.entry_size()).unwrap_or(0),
                self.endian,
            );
            let (shnum, shstrndx) = if self.extended_counts {
                (0, 0xffff)
            } else {
                (
                    u16::try_from(count).unwrap_or(0),
                    u16::try_from(string_table_index).unwrap_or(0),
                )
            };
            write_u16(&mut image, shnum_at, shnum, self.endian);
            write_u16(&mut image, shstrndx_at, shstrndx, self.endian);
            image
        }

        /// The section header table.
        fn section_headers(
            &self,
            sections: &[SectionSpec],
            offsets: &[u32],
            content_offsets: &[u64],
            string_table_index: usize,
            strings_offset: usize,
            strings_len: usize,
        ) -> Vec<u8> {
            let count = sections.len() + 2;
            let mut headers = vec![0_u8; self.class.entry_size() * count];

            if self.extended_counts {
                // The null header carries the real count in `sh_size` and the real
                // string-table index in `sh_link`.
                self.write_header(
                    &mut headers,
                    0,
                    &SectionSpec {
                        size: u64::try_from(count).unwrap_or(0),
                        link: u32::try_from(string_table_index).unwrap_or(0),
                        ..SectionSpec::progbits("", 0, 0)
                    },
                    0,
                    0,
                );
            }
            for (index, section) in sections.iter().enumerate() {
                let name_offset = section
                    .name_offset
                    .or_else(|| offsets.get(index).copied())
                    .unwrap_or(0);
                self.write_header(
                    &mut headers,
                    index + 1,
                    section,
                    name_offset,
                    content_offsets.get(index).copied().unwrap_or(0),
                );
            }
            self.write_header(
                &mut headers,
                string_table_index,
                &SectionSpec {
                    size: u64::try_from(strings_len).unwrap_or(0),
                    ..SectionSpec::strings(".shstrtab")
                },
                offsets.last().copied().unwrap_or(0),
                u64::try_from(strings_offset).unwrap_or(0),
            );
            headers
        }

        fn write_header(
            &self,
            headers: &mut [u8],
            index: usize,
            section: &SectionSpec,
            name_offset: u32,
            offset: u64,
        ) {
            let wide = self.class == Class::Elf64;
            let at = index * self.class.entry_size();
            let layout = self.class.layout();
            write_u32(headers, at, name_offset, self.endian);
            write_u32(headers, at + 0x04, section.kind, self.endian);
            write_address(
                headers,
                at + layout.sh_flags,
                section.flags,
                self.endian,
                wide,
            );
            write_address(headers, at + layout.sh_offset, offset, self.endian, wide);
            write_address(
                headers,
                at + layout.sh_size,
                section.size,
                self.endian,
                wide,
            );
            write_u32(headers, at + layout.sh_link, section.link, self.endian);
            // `sh_info` sits one word after `sh_link` in both classes. Kept here rather
            // than in `Layout`, which is shared with the parser so that the two cannot
            // disagree about a field they both read — and the parser never reads this one.
            write_u32(
                headers,
                at + layout.sh_link + 0x04,
                section.info,
                self.endian,
            );
            write_address(
                headers,
                at + layout.sh_entsize,
                section.entry_size,
                self.endian,
                wide,
            );
        }
    }

    fn write_bytes(target: &mut [u8], at: usize, bytes: &[u8]) {
        if let Some(window) = target.get_mut(at..at + bytes.len()) {
            window.copy_from_slice(bytes);
        }
    }

    fn write_u16(target: &mut [u8], at: usize, value: u16, endian: Endian) {
        let raw = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        write_bytes(target, at, &raw);
    }

    fn write_u32(target: &mut [u8], at: usize, value: u32, endian: Endian) {
        let raw = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        write_bytes(target, at, &raw);
    }

    fn write_address(target: &mut [u8], at: usize, value: u64, endian: Endian, wide: bool) {
        if wide {
            let raw = match endian {
                Endian::Little => value.to_le_bytes(),
                Endian::Big => value.to_be_bytes(),
            };
            write_bytes(target, at, &raw);
        } else {
            write_u32(target, at, u32::try_from(value).unwrap_or(u32::MAX), endian);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::tests_support::{Class, ElfBuilder, SectionSpec, SymbolSpec};

    /// An image with one function symbol in `.text` and one in `.bss`.
    fn image_with_symbols(class: Class) -> Vec<u8> {
        ElfBuilder::new(class)
            .with(SectionSpec::progbits(
                ".text",
                0x40,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with(SectionSpec::nobits(".bss", 0x20, SHF_ALLOC | SHF_WRITE))
            .with_symbols(vec![
                SymbolSpec::new("_RNvCs1_4mine4work", 0x1000, 0x30, 1),
                // Deliberately the same size as `work` and at another address: a reader
                // pairing this table against another one by size alone cannot tell them
                // apart, and every symbol here has to be identifiable.
                SymbolSpec::new("_RNvCs1_4mine4more", 0x1030, 0x30, 1),
                SymbolSpec::new("_RNvCs1_4mine5state", 0x2000, 0x10, 2),
            ])
            .build()
    }

    #[test]
    fn a_symbol_table_reports_each_symbols_name_size_and_section() {
        let image = image_with_symbols(Class::Elf32);
        let symbols = symbols(&image).expect("a synthetic ELF is readable");
        let named: Vec<(&str, u64, u16)> = symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.size, symbol.section_index))
            .collect();
        assert!(
            named.contains(&("_RNvCs1_4mine4work", 0x30, 1)),
            "{named:?}"
        );
        assert!(
            named.contains(&("_RNvCs1_4mine5state", 0x10, 2)),
            "{named:?}"
        );
    }

    #[test]
    fn the_same_symbols_read_the_same_in_a_wide_big_endian_image() {
        let image = ElfBuilder::new(Class::Elf64)
            .big_endian()
            .with(SectionSpec::progbits(
                ".text",
                0x40,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with_symbols(vec![SymbolSpec::new("_RNvCs1_4mine4work", 0x1000, 0x30, 1)])
            .build();
        let symbols = symbols(&image).expect("a synthetic ELF is readable");
        // The table opens with the all-zero symbol every real one does, and the parser
        // reports the table as written rather than editing it.
        let named: Vec<&Symbol> = symbols
            .iter()
            .filter(|symbol| !symbol.name.is_empty())
            .collect();
        assert_eq!(named.len(), 1, "{symbols:?}");
        assert_eq!(named[0].name, "_RNvCs1_4mine4work");
        assert_eq!(named[0].size, 0x30);
        assert_eq!(named[0].section_index, 1);
    }

    /// `llvm-readobj` from the pinned toolchain's sysroot.
    ///
    /// `llvm-readelf` is the same binary under another name and is not installed by
    /// `llvm-tools-preview`; `--elf-output-style=GNU` is what the other name selects.
    fn llvm_readobj() -> std::path::PathBuf {
        let sysroot = std::process::Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .expect("rustc should run");
        let sysroot =
            std::path::PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim().to_owned());
        let host = std::process::Command::new("rustc")
            .arg("-vV")
            .output()
            .expect("rustc should run");
        let host = String::from_utf8_lossy(&host.stdout).into_owned();
        let host = host
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .expect("rustc reports its host triple")
            .trim()
            .to_owned();
        let path = sysroot
            .join("lib/rustlib")
            .join(host)
            .join("bin/llvm-readobj");
        assert!(
            path.is_file(),
            "llvm-readobj is missing from the toolchain sysroot; rust-toolchain.toml pins \
             llvm-tools-preview, so this is a broken toolchain rather than a skippable test"
        );
        path
    }

    #[test]
    fn both_classes_read_the_symbols_llvm_readobj_reads() {
        // The synthetic tests share `Layout` between the builder and the parser, on
        // purpose — two copies of the offsets can be wrong in the same direction. That
        // leaves one hole, and it is not hypothetical: reading `st_shndx` four bytes early
        // reads `st_info`, which for a table of functions is the constant 2, and every
        // symbol then resolves to section 2 with the same total size. A sum cannot see it.
        // So the second opinion is per symbol and includes the section index.
        //
        // It is here rather than beside the `llvm-nm` check in `tests/size_budgets.rs`
        // because the only image that gate ever links is ELF32: the whole 64-bit half of
        // the layout table would otherwise have no external reader at all, and a
        // copy-pasted offset in it would leave every test in the workspace green.
        let readobj = llvm_readobj();
        let directory = std::env::temp_dir().join(format!("waymaker-elf-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a temporary directory");

        for (class, name) in [(Class::Elf32, "elf32.o"), (Class::Elf64, "elf64.o")] {
            let image = image_with_symbols(class);
            let path = directory.join(name);
            std::fs::write(&path, &image).expect("the image should be writable");

            let output = std::process::Command::new(&readobj)
                .args(["--elf-output-style=GNU", "--symbols", "--wide"])
                .arg(&path)
                .output()
                .expect("llvm-readobj should run");
            let listing = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && output.stderr.is_empty(),
                "llvm-readobj rejected the synthetic {name}: {}\n{listing}",
                String::from_utf8_lossy(&output.stderr)
            );

            // `Num: Value Size Type Bind Vis Ndx Name`
            let second_opinion: Vec<(String, u64, u64, u16)> = listing
                .lines()
                .filter_map(|line| {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    let (address, size) = (fields.get(1)?, fields.get(2)?);
                    let (section, symbol) = (fields.get(6)?, fields.get(7)?);
                    Some((
                        (*symbol).to_owned(),
                        u64::from_str_radix(address, 16).ok()?,
                        size.parse().ok()?,
                        section.parse().ok()?,
                    ))
                })
                .collect();

            let ours: Vec<(String, u64, u64, u16)> = symbols(&image)
                .expect("a synthetic ELF is readable")
                .into_iter()
                .filter(|symbol| !symbol.name.is_empty())
                .map(|symbol| {
                    (
                        symbol.name,
                        symbol.address,
                        symbol.size,
                        symbol.section_index,
                    )
                })
                .collect();

            assert!(!ours.is_empty(), "the builder wrote no named symbols");
            assert_eq!(
                ours, second_opinion,
                "our reader and llvm-readobj disagree about {name}:\n{listing}"
            );
        }

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_image_with_no_symbol_table_reports_no_symbols() {
        // Not an error here: whether a missing table is fatal is the gate's decision, and
        // the parser's job is to read the format.
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        assert_eq!(symbols(&image), Ok(Vec::new()));
    }

    #[test]
    fn a_symbol_name_outside_the_string_table_is_an_error_rather_than_a_nameless_symbol() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .with_symbols(vec![
                SymbolSpec::new("work", 0x1000, 4, 1).with_name_offset(0xffff),
            ])
            .build();
        assert!(
            symbols(&image).is_err(),
            "a name outside the table is not a nameless symbol"
        );
    }

    #[test]
    fn a_truncated_symbol_table_is_an_error_rather_than_the_symbols_that_fit() {
        let mut image = image_with_symbols(Class::Elf32);
        // Declare a table longer than the file, which is what a truncated artifact looks
        // like. Reading the entries that fit would under-report every attribution.
        let sections = sections(&image).expect("readable");
        let index = sections
            .iter()
            .position(|section| section.kind == SHT_SYMTAB)
            .expect("the builder wrote a symbol table");
        let table = locate_table(&image, ELF32, Endian::Little).expect("readable");
        let at =
            section_field(table.offset, table.entry_size, index, ELF32.sh_size).expect("in range");
        image
            .get_mut(at..at + 4)
            .expect("in range")
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            symbols(&image).is_err(),
            "a truncated table is not a short one"
        );
    }

    #[test]
    fn a_symbol_table_whose_entries_are_the_wrong_width_is_an_error() {
        let mut image = image_with_symbols(Class::Elf32);
        let sections = sections(&image).expect("readable");
        let index = sections
            .iter()
            .position(|section| section.kind == SHT_SYMTAB)
            .expect("the builder wrote a symbol table");
        let table = locate_table(&image, ELF32, Endian::Little).expect("readable");
        let at = section_field(table.offset, table.entry_size, index, ELF32.sh_entsize)
            .expect("in range");
        image
            .get_mut(at..at + 4)
            .expect("in range")
            .copy_from_slice(&20_u32.to_le_bytes());
        assert!(
            symbols(&image).is_err(),
            "entries of an unexpected width are read at the wrong offsets, not read anyway"
        );
    }

    #[test]
    fn an_elf32_little_endian_image_reports_its_sections() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(
                ".text",
                0x40,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with(SectionSpec::progbits(".rodata", 0x10, SHF_ALLOC))
            .with(SectionSpec::nobits(".bss", 0x200, SHF_ALLOC | SHF_WRITE))
            .build();

        let sections = sections(&image).expect("a synthetic ELF is readable");
        let named: Vec<(&str, u64)> = sections
            .iter()
            .map(|section| (section.name.as_str(), section.size))
            .collect();
        assert!(named.contains(&(".text", 0x40)), "{named:?}");
        assert!(named.contains(&(".rodata", 0x10)), "{named:?}");
        assert!(named.contains(&(".bss", 0x200)), "{named:?}");
    }

    #[test]
    fn allocation_and_writability_come_from_the_section_flags() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC | SHF_EXECINSTR))
            .with(SectionSpec::nobits(".bss", 16, SHF_ALLOC | SHF_WRITE))
            .with(SectionSpec::progbits(".comment", 99, 0))
            .build();
        let sections = sections(&image).expect("readable");

        let text = find(&sections, ".text");
        assert!(text.allocated() && !text.writable() && text.occupies_storage());

        let bss = find(&sections, ".bss");
        assert!(bss.allocated() && bss.writable() && !bss.occupies_storage());

        let comment = find(&sections, ".comment");
        assert!(!comment.allocated() && !comment.occupies_storage());
    }

    #[test]
    fn an_elf64_image_is_read_with_the_wider_header() {
        let image = ElfBuilder::new(Class::Elf64)
            .with(SectionSpec::progbits(".text", 0x1234_5678, SHF_ALLOC))
            .build();
        let sections = sections(&image).expect("readable");
        assert_eq!(find(&sections, ".text").size, 0x1234_5678);
    }

    #[test]
    fn a_big_endian_image_is_read_with_the_other_byte_order() {
        let image = ElfBuilder::new(Class::Elf32)
            .big_endian()
            .with(SectionSpec::progbits(".text", 0x0102_0304, SHF_ALLOC))
            .build();
        let sections = sections(&image).expect("readable");
        assert_eq!(find(&sections, ".text").size, 0x0102_0304);
    }

    #[test]
    fn a_file_that_is_not_an_elf_is_rejected() {
        let error = sections(b"not an elf at all").expect_err("must fail closed");
        assert!(error.to_string().contains("ELF"), "{error}");
    }

    #[test]
    fn an_empty_file_is_rejected() {
        assert!(sections(&[]).is_err());
    }

    #[test]
    fn an_unknown_class_is_rejected_rather_than_guessed() {
        let mut image = ElfBuilder::new(Class::Elf32).build();
        set(&mut image, 4, 7);
        let error = sections(&image).expect_err("must fail closed");
        assert!(error.to_string().contains("class"), "{error}");
    }

    #[test]
    fn an_unknown_byte_order_is_rejected_rather_than_guessed() {
        let mut image = ElfBuilder::new(Class::Elf32).build();
        set(&mut image, 5, 9);
        let error = sections(&image).expect_err("must fail closed");
        assert!(error.to_string().contains("byte order"), "{error}");
    }

    #[test]
    fn a_truncated_section_header_table_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        image.truncate(image.len() - 8);
        let error = sections(&image).expect_err("must fail closed");
        assert!(error.to_string().contains("truncated"), "{error}");
    }

    #[test]
    fn a_file_with_no_section_header_table_is_rejected() {
        let image = ElfBuilder::new(Class::Elf32)
            .without_section_headers()
            .build();
        let error = sections(&image).expect_err("must fail closed");
        assert!(
            error.to_string().contains("no section header table"),
            "{error}"
        );
    }

    #[test]
    fn an_out_of_range_name_offset_is_rejected() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC).with_name_offset(9_999))
            .build();
        let error = sections(&image).expect_err("must fail closed");
        assert!(error.to_string().contains("name"), "{error}");
    }

    #[test]
    fn an_extended_section_count_is_read_from_the_first_header() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 0x20, SHF_ALLOC))
            .with_extended_counts()
            .build();
        let sections = sections(&image).expect("readable");
        assert_eq!(find(&sections, ".text").size, 0x20);
    }

    #[test]
    fn an_image_with_only_the_null_section_header_is_rejected() {
        // What `llvm-objcopy --strip-sections` leaves behind: a table holding only the
        // reserved null entry. It parses perfectly, and every size in it reads zero, which
        // passes every budget.
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        write_u16_le(&mut image, 0x30, 1);
        let error = sections(&image).expect_err("a stripped image has not been measured");
        assert!(error.to_string().contains("null section"), "{error}");
    }

    #[test]
    fn a_name_offset_exactly_at_the_end_of_the_string_table_is_rejected() {
        // The boundary `strings.get(len..)` reads as an empty slice rather than as out of
        // range, which would have named the section `""` and quietly dropped its size out
        // of the per-section breakdown.
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        let strings_len = string_table_len(&image);
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC).with_name_offset(strings_len))
            .build();
        let error = sections(&image).expect_err("an offset past the last name is out of range");
        assert!(error.to_string().contains("outside"), "{error}");
    }

    #[test]
    fn a_section_header_table_inside_the_file_header_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        // e_shoff, pointing at the middle of the file header it belongs to.
        write_u32_le(&mut image, 0x20, 8);
        let error = sections(&image).expect_err("the table cannot precede itself");
        assert!(error.to_string().contains("file header"), "{error}");
    }

    #[test]
    fn a_section_header_entry_smaller_than_the_class_allows_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        // e_shentsize, smaller than an Elf32_Shdr.
        write_u16_le(&mut image, 0x2e, 20);
        let error = sections(&image).expect_err("overlapping entries are malformed");
        assert!(error.to_string().contains("40 byte entries"), "{error}");
    }

    #[test]
    fn a_section_header_entry_of_zero_size_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        write_u16_le(&mut image, 0x2e, 0);
        assert!(sections(&image).is_err());
    }

    #[test]
    fn a_string_table_index_past_the_end_of_the_table_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        // e_shstrndx, naming a section the table does not have. Below SHN_LORESERVE, so
        // it is read as an index rather than as the extended form.
        write_u16_le(&mut image, 0x32, 40);
        let error = sections(&image).expect_err("the names cannot come from nowhere");
        assert!(error.to_string().contains("string table"), "{error}");
    }

    #[test]
    fn a_section_name_that_is_not_utf8_is_rejected() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        // The section name string table begins at the end of the file header; byte 1 is
        // the first character of `.text`.
        set(&mut image, 0x34 + 1, 0xff);
        let error = sections(&image).expect_err("a name must be readable");
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn the_machine_is_read_from_the_header() {
        let mut image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(".text", 8, SHF_ALLOC))
            .build();
        write_u16_le(&mut image, 0x12, EM_ARM);
        assert_eq!(machine(&image).expect("readable"), EM_ARM);
        write_u16_le(&mut image, 0x12, 0x3e);
        assert_eq!(machine(&image).expect("readable"), 0x3e);
        assert!(machine(b"not an elf").is_err());
    }

    /// The size of the section name string table in a freshly built synthetic image.
    fn string_table_len(image: &[u8]) -> u32 {
        let sections = sections(image).expect("a synthetic image is readable");
        sections
            .iter()
            .find(|section| section.name == ".shstrtab")
            .map_or(0, |section| u32::try_from(section.size).unwrap_or(0))
    }

    fn write_u16_le(image: &mut [u8], at: usize, value: u16) {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            set(image, at + offset, byte);
        }
    }

    fn write_u32_le(image: &mut [u8], at: usize, value: u32) {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            set(image, at + offset, byte);
        }
    }

    fn find<'a>(sections: &'a [Section], name: &str) -> &'a Section {
        sections
            .iter()
            .find(|section| section.name == name)
            .unwrap_or_else(|| panic!("{name} is missing from {sections:?}"))
    }

    fn set(image: &mut [u8], at: usize, byte: u8) {
        *image.get_mut(at).expect("in range") = byte;
    }
}
