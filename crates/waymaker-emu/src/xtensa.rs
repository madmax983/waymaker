//! The ESP32-S3 startup: `_start`, UART0 console, and no exit.
//!
//! There is no `cortex-m-rt` for Xtensa, so this module spells out what that crate expands
//! on ARM: installing the stack pointer, zeroing `.bss`, and poking the UART registers.
//! That is the hand-written half of the workspace's `unsafe_code` exception, and it is
//! scoped by the `#[cfg(target_arch = "xtensa")]` gate on the module declaration in the
//! crate root — the `emulation-boot` rule checks the gate, so no ARM image can contain
//! this code.
//!
//! The guest never exits: after the census it drains the UART FIFO and parks, and the
//! harness terminates QEMU from the host once it has parsed the census.

use core::arch::naked_asm;
use core::fmt::{self, Write as _};
use core::panic::PanicInfo;

use waymaker_rig::run::Rig;

use crate::boot::{Census, Trouble};
use crate::nor::Nor;
use crate::{PREFIX, halt};

// Top of the mapped data-bus SRAM (0x3FD0_0000), with 32 bytes of slack.
// Installed as the stack in `_start`. The address is deliberately NOT a linker
// region: the data-bus view aliases the same physical HP SRAM the image links
// into (see memory-xtensa.x), so the stack lives in the window's top 64 KiB
// (physical 0x3FCF_0000..0x3FD0_0000), which sits above both linked regions —
// the DRAM region ends at physical 0x3FCF_0000 — and has no instruction-bus
// address. The downward-growing stack therefore cannot physically overlap the
// linked image.
const STACK_TOP: u32 = 0x3FCF_FFE0;

unsafe extern "C" {
    static mut _sbss: u32;
    static mut _ebss: u32;
}

/// Firmware entry point. The ESP32-S3 ROM loads this image from flash offset 0x0 and
/// jumps here with `a1` pointing at its own (small but valid) stack; this installs the
/// real stack and enters `firmware_main`, never to return.
///
/// Naked because the stack switch cannot live in inline `asm!` inside an ordinary Rust
/// function: `options(nostack)` promises the compiler the assembly does not modify the
/// stack pointer, and replacing `a1` contradicts that contract outright — "no locals,
/// nothing live across the block" is an argument about today's codegen, not about the
/// promise. A naked function cannot be `-> !` (its body must be the single
/// `naked_asm!`, which evaluates to `()`), so the signature says `()` and the
/// never-returns is by construction: the template ends in the windowed call into
/// `firmware_main`, which diverges.
///
/// The template hand-writes the `entry` the windowed ABI requires of a caller before a
/// windowed call — a naked `l32r`/`call8` without it was tried: the image built and the
/// entry disassembly looked right, but the guest faulted in QEMU.
///
/// The stack address is built from `STACK_TOP` with plain immediates (`const`
/// operands; `naked_asm!` takes no `in(reg)`): `addi`'s 12-bit signed immediate cannot
/// add the low half (`0xFFE0`), so the template builds `STACK_TOP + 32` and subtracts
/// 32. A hand-written `l32r` with an inline literal must not be used: the Xtensa
/// linker's literal-pool handling mangles relocations for hand-placed literals in
/// `.text`. `a1` needs no handoff for the call — the window rotation keeps the caller's
/// `a1` as the callee's `a1` — and `call8`'s ±128 KiB range comfortably covers the
/// image (the link would fail loudly if it ever did not).
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() {
    naked_asm!(
        "entry a1, 32",
        "movi a1, {hi}",
        "slli a1, a1, 8",
        "addi a1, a1, {mid}",
        "slli a1, a1, 8",
        "slli a1, a1, 8",
        "movi a2, 32",
        "sub a1, a1, a2",
        "call8 {main}",
        hi = const (STACK_TOP >> 24),
        mid = const (((STACK_TOP + 32) >> 16) & 0xFF),
        main = sym firmware_main,
    );
}

fn firmware_main() -> ! {
    zero_bss();

    // Both locals, not statics: a `static` would need interior mutability this crate has
    // no way to spell.
    let mut part = Nor::new();
    let mut page = [0_u8; Rig::PAGE_BYTES];

    match crate::boot::run(&mut part, &mut page) {
        Ok(census) => {
            report(&census);
            uprintln(format_args!("{} ok", PREFIX));
        }
        Err(trouble) => {
            // The breach code rides on the failed line itself: the harness kills
            // QEMU as soon as a terminated `failed` line reaches the serial log,
            // so a second line would race the kill and lose the code identifying
            // the violated outcome.
            if let Trouble::Breach(outcome) = trouble {
                uprintln(format_args!(
                    "{} failed {} (breach code={})",
                    PREFIX,
                    trouble.message(),
                    outcome.code()
                ));
            } else {
                uprintln(format_args!("{} failed {}", PREFIX, trouble.message()));
            }
        }
    }

    // No guest-side exit exists on this machine; the harness terminates QEMU from the
    // host after it has parsed the census. Drain the FIFO so the last lines actually
    // escape, then park.
    uart_drain();
    halt()
}

/// Writes the census as two lines the harness parses and a person can read.
fn report(census: &Census) {
    uprintln(format_args!(
        "{} cases passed={} exempt={}",
        PREFIX, census.cases_passed, census.cases_exempt
    ));
    uprintln(format_args!(
        "{} rig iterations={} cuts={} resumes={} unextendable={} redeliveries={} verdicts={} dispatched={}",
        PREFIX,
        census.iterations,
        census.cuts,
        census.resumes,
        census.unextendable,
        census.redeliveries,
        census.verdicts_passed,
        census.dispatched
    ));
}

/// Zeros the `.bss` section. The ROM loads sections but does not clear them.
fn zero_bss() {
    unsafe {
        let mut cursor = core::ptr::addr_of_mut!(_sbss).cast::<u32>();
        let end = core::ptr::addr_of_mut!(_ebss).cast::<u32>();
        while cursor < end {
            cursor.write_volatile(0);
            cursor = cursor.add(1);
        }
    }
}

// ---------------------------------------------------------------------------
// UART0: the console. The ROM leaves UART0 clocked and configured; under QEMU the
// machine drains the TX FIFO on every write regardless. Base 0x6000_0000, FIFO at +0x00,
// STATUS at +0x1C with TXFIFO_CNT at [25:16], 128 bytes deep.
// ---------------------------------------------------------------------------

const UART0_BASE: usize = 0x6000_0000;
const UART_FIFO: *mut u32 = (UART0_BASE + 0x00) as *mut u32;
const UART_STATUS: *const u32 = (UART0_BASE + 0x1C) as *const u32;

fn txfifo_count() -> u32 {
    (unsafe { UART_STATUS.read_volatile() } >> 16) & 0xFF
}

/// UART0 console writer. There is exactly one; it is a unit struct because there is no
/// state to keep — the hardware holds it all.
struct Writer;

impl Writer {
    fn putc(byte: u8) {
        while txfifo_count() >= 128 {}
        unsafe { UART_FIFO.write_volatile(u32::from(byte)) };
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            // The S3 UART does no newline translation; this does it here.
            if byte == b'\n' {
                Self::putc(b'\r');
            }
            Self::putc(byte);
        }
        Ok(())
    }
}

/// Writes formatted output to UART0, without a trailing newline.
/// `format_args!` needs no allocator.
fn uprint(args: fmt::Arguments<'_>) {
    let _ = Writer.write_fmt(args);
}

/// Writes formatted output to UART0. `format_args!` needs no allocator.
fn uprintln(args: fmt::Arguments<'_>) {
    uprint(args);
    Writer::putc(b'\n');
}

/// A `fmt::Write` adapter that escapes `\n` and `\r` so a panic message with
/// embedded newlines — assertion diagnostics commonly span lines — cannot
/// terminate the `panicked:` line early: the harness kills QEMU on the first
/// terminated `panicked:` line, which would truncate the diagnostic before
/// `uart_drain()` runs (Codex P2, `review_comment` 3997773910). Needs no
/// allocator: the message is escaped as it is written.
struct EscapeNewlines<W>(W);

impl<W: fmt::Write> fmt::Write for EscapeNewlines<W> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            match ch {
                '\n' => self.0.write_str("\\n")?,
                '\r' => self.0.write_str("\\r")?,
                _ => self.0.write_char(ch)?,
            }
        }
        Ok(())
    }
}

/// Spins until the TX FIFO is fully shifted out, so no output is lost before the harness
/// terminates the machine.
fn uart_drain() {
    while txfifo_count() != 0 {
        core::hint::spin_loop();
    }
}

/// A panic anywhere in the rig fails the run: prints what it can, drains, parks. The
/// harness treats a missing census as a failure.
///
/// The report is one line on purpose. `PanicInfo`'s `Display` writes
/// `panicked at <location>:` followed by a newline and then the message, so
/// `panicked: {info}` is two terminated lines — and the harness kills QEMU on
/// the first terminated `panicked:` line, before the message is written and
/// before `uart_drain` runs, losing the very diagnostic the detector exists to
/// preserve (Codex P2, `review_comment` 3997629597). The message and the
/// location are printed as separate pieces so no formatting step can
/// reintroduce the structural newline; the `panicked:` prefix stays first so
/// the harness's detector still recognizes the line.
///
/// The message itself goes through [`EscapeNewlines`]: assertion payloads
/// commonly contain newlines of their own, and one of those would terminate
/// the line just as surely as `PanicInfo`'s `Display` did (Codex P2,
/// `review_comment` 3997773910).
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    uprint(format_args!("{PREFIX} panicked: "));
    let mut escaped = EscapeNewlines(Writer);
    let _ = escaped.write_fmt(format_args!("{}", info.message()));
    if let Some(location) = info.location() {
        uprint(format_args!(" at {location}"));
    }
    Writer::putc(b'\n');
    uart_drain();
    halt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write as _;

    /// A `fmt::Write` sink over a fixed buffer: no allocator, so the escape
    /// adapter stays testable in this `#![no_std]` crate.
    struct Sink {
        buf: [u8; 64],
        len: usize,
    }

    impl Sink {
        fn new() -> Self {
            Self {
                buf: [0; 64],
                len: 0,
            }
        }

        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buf[..self.len]).unwrap_or("<invalid utf-8>")
        }
    }

    impl fmt::Write for Sink {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            let bytes = text.as_bytes();
            assert!(self.len + bytes.len() <= self.buf.len());
            self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
            Ok(())
        }
    }

    #[test]
    fn embedded_newlines_in_the_panic_message_are_escaped() {
        // Codex P2 (review_comment 3997773910): assertion payloads commonly
        // contain newlines; the `panicked:` line must stay one terminated
        // line or the harness kills QEMU before `uart_drain()` runs.
        let mut escaped = EscapeNewlines(Sink::new());
        let _ = escaped.write_fmt(format_args!("first\nsecond\rlast"));
        assert_eq!(escaped.0.as_str(), "first\\nsecond\\rlast");
    }

    #[test]
    fn plain_text_passes_through_unescaped() {
        let mut escaped = EscapeNewlines(Sink::new());
        let _ = escaped.write_str("no newlines here");
        assert_eq!(escaped.0.as_str(), "no newlines here");
    }
}
