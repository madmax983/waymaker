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

/// The first section index that is reserved rather than a real section.
///
/// A file with this many sections or more stores the real count and the real string-table
/// index in the otherwise unused first section header.
const SHN_LORESERVE: u16 = 0xff00;

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
#[derive(Debug, Clone, Copy)]
struct Layout {
    section_header_offset: usize,
    section_entry_size: usize,
    section_count: usize,
    string_table_index: usize,
    /// Offsets within one section header entry.
    sh_name: usize,
    sh_type: usize,
    sh_flags: usize,
    sh_size: usize,
    /// Whether `sh_flags` and `sh_size` are 64 bits wide.
    wide: bool,
}

const ELF32: Layout = Layout {
    section_header_offset: 0x20,
    section_entry_size: 0x2e,
    section_count: 0x30,
    string_table_index: 0x32,
    sh_name: 0x00,
    sh_type: 0x04,
    sh_flags: 0x08,
    sh_size: 0x14,
    wide: false,
};

const ELF64: Layout = Layout {
    section_header_offset: 0x28,
    section_entry_size: 0x3a,
    section_count: 0x3c,
    string_table_index: 0x3e,
    sh_name: 0x00,
    sh_type: 0x04,
    sh_flags: 0x08,
    sh_size: 0x20,
    wide: true,
};

/// A raw section header, before its name has been resolved.
#[derive(Debug, Clone, Copy)]
struct RawSection {
    name_offset: u32,
    kind: u32,
    flags: u64,
    size: u64,
    /// `sh_link`, read only from the first header, where it carries the extended
    /// string-table index.
    link: u32,
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
                name: read_name(strings, section.name_offset)?,
                size: section.size,
                kind: section.kind,
                flags: section.flags,
            })
        })
        .collect()
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

    let entry_size = usize::from(read_u16(bytes, layout.section_entry_size, endian)?);
    if entry_size == 0 {
        return Err(ElfError::new("the section header entries have zero size"));
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
        sh_offset_of(layout),
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

/// `sh_offset`'s position inside a section header, which differs by class.
const fn sh_offset_of(layout: Layout) -> usize {
    if layout.wide { 0x18 } else { 0x10 }
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
        size: read_address(
            bytes,
            section_field(table_offset, entry_size, index, layout.sh_size)?,
            endian,
            layout.wide,
        )?,
        link: read_u32(
            bytes,
            // `sh_link` follows `sh_size` in both classes.
            section_field(table_offset, entry_size, index, layout.sh_size)?
                .saturating_add(if layout.wide { 8 } else { 4 }),
            endian,
        )?,
    })
}

/// The NUL-terminated name at `offset` in the section name string table.
fn read_name(strings: &[u8], offset: u32) -> Result<String, ElfError> {
    let offset = usize::try_from(offset)
        .map_err(|_| ElfError::new("a section name offset does not fit in memory"))?;
    let rest = strings.get(offset..).ok_or_else(|| {
        ElfError::new(format!(
            "a section name offset ({offset}) points outside the {} byte string table",
            strings.len()
        ))
    })?;
    let end = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    let name = rest
        .get(..end)
        .ok_or_else(|| ElfError::new("a section name is truncated"))?;
    String::from_utf8(name.to_vec())
        .map_err(|err| ElfError::new(format!("a section name is not valid UTF-8: {err}")))
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
/// Public rather than `#[cfg(test)]` so that the size gate's own tests can measure a
/// synthetic image instead of linking real firmware: a rule about what happens when a
/// budget is exceeded should not need a build that exceeds it.
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
        const fn header_size(self) -> usize {
            match self {
                Self::Elf32 => 0x34,
                Self::Elf64 => 0x40,
            }
        }

        const fn entry_size(self) -> usize {
            match self {
                Self::Elf32 => 0x28,
                Self::Elf64 => 0x40,
            }
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

    /// Assembles a minimal but well-formed ELF image around a list of sections.
    #[derive(Debug, Clone)]
    pub struct ElfBuilder {
        class: Class,
        endian: Endian,
        sections: Vec<SectionSpec>,
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

        /// Renders the image.
        #[must_use]
        pub fn build(&self) -> Vec<u8> {
            let (strings, offsets) = self.string_table();
            let string_table_index = self.sections.len() + 1;
            let strings_offset = self.class.header_size();
            let table_offset = strings_offset + strings.len();

            let mut image = self.file_header(table_offset, strings.len(), string_table_index);
            image.extend_from_slice(&strings);
            image.extend_from_slice(&self.section_headers(
                &offsets,
                string_table_index,
                strings_offset,
                strings.len(),
            ));
            image
        }

        /// The section name string table, and each section's offset into it.
        fn string_table(&self) -> (Vec<u8>, Vec<u32>) {
            let mut strings = vec![0_u8];
            let mut offsets = Vec::new();
            let shstrtab = SectionSpec::progbits(".shstrtab", 0, 0);
            for section in self.sections.iter().chain(core::iter::once(&shstrtab)) {
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
        ) -> Vec<u8> {
            let header_size = self.class.header_size();
            let wide = self.class == Class::Elf64;
            let count = self.sections.len() + 2;

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

            let (shoff_at, shentsize_at, shnum_at, shstrndx_at) = if wide {
                (0x28, 0x3a, 0x3c, 0x3e)
            } else {
                (0x20, 0x2e, 0x30, 0x32)
            };
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
            offsets: &[u32],
            string_table_index: usize,
            strings_offset: usize,
            strings_len: usize,
        ) -> Vec<u8> {
            let count = self.sections.len() + 2;
            let mut headers = vec![0_u8; self.class.entry_size() * count];

            if self.extended_counts {
                // The null header carries the real count in `sh_size` and the real
                // string-table index in `sh_link`.
                self.write_header(
                    &mut headers,
                    0,
                    0,
                    0,
                    0,
                    u64::try_from(count).unwrap_or(0),
                    0,
                    u32::try_from(string_table_index).unwrap_or(0),
                );
            }
            for (index, section) in self.sections.iter().enumerate() {
                let name_offset = section
                    .name_offset
                    .or_else(|| offsets.get(index).copied())
                    .unwrap_or(0);
                self.write_header(
                    &mut headers,
                    index + 1,
                    name_offset,
                    section.kind,
                    section.flags,
                    section.size,
                    0,
                    0,
                );
            }
            self.write_header(
                &mut headers,
                string_table_index,
                offsets.last().copied().unwrap_or(0),
                3,
                0,
                u64::try_from(strings_len).unwrap_or(0),
                u64::try_from(strings_offset).unwrap_or(0),
                0,
            );
            headers
        }

        #[expect(
            clippy::too_many_arguments,
            reason = "a section header has this many fields; naming them in a struct here \
                      would move the same list one line up"
        )]
        fn write_header(
            &self,
            headers: &mut [u8],
            index: usize,
            name_offset: u32,
            kind: u32,
            flags: u64,
            size: u64,
            offset: u64,
            link: u32,
        ) {
            let wide = self.class == Class::Elf64;
            let at = index * self.class.entry_size();
            let (flags_at, offset_at, size_at, link_at) = if wide {
                (0x08, 0x18, 0x20, 0x28)
            } else {
                (0x08, 0x10, 0x14, 0x18)
            };
            write_u32(headers, at, name_offset, self.endian);
            write_u32(headers, at + 0x04, kind, self.endian);
            write_address(headers, at + flags_at, flags, self.endian, wide);
            write_address(headers, at + offset_at, offset, self.endian, wide);
            write_address(headers, at + size_at, size, self.endian, wide);
            write_u32(headers, at + link_at, link, self.endian);
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
    use crate::elf::tests_support::{Class, ElfBuilder, SectionSpec};

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
