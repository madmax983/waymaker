/* Linker script for the ESP32-S3 spike image.
 *
 * The ROM bootloader loads the image's LOAD segments from flash offset 0x0
 * and jumps to ENTRY(_start).
 *
 * The two SRAM bus views alias the SAME physical HP SRAM, so the linker must
 * never see both views at their full mapped ranges: it would then be free to
 * place .text at the same physical address as .data/.bss, and loading data or
 * zero_bss() would overwrite instructions. Instead the script grants the
 * linker a non-overlapping split of the physical SRAM:
 *
 *   IRAM (instruction-bus view, 0x4037_0000, executable, 128 KiB)
 *       -> physical SRAM Blocks 0-3 (IBUS 0x4037_0000..0x4039_0000):
 *          .text/.literal. Per TRM Table 15.3-3, Blocks 2-3 are also
 *          visible at DBUS 0x3FC8_8000..0x3FCA_0000; Blocks 0-1 have no
 *          data-bus address at all.
 *   DRAM (data-bus view, 0x3FCA_0000, 320 KiB)
 *       -> physical SRAM Blocks 4-8 (DBUS 0x3FCA_0000..0x3FCF_0000):
 *          .data/.bss/.rodata.
 *
 * The two regions are adjacent in physical SRAM and disjoint, so the combined
 * physical bound is enforced structurally: the linker cannot alias them.
 *
 * Data must live in the data-bus view, not the instruction-bus view: the S3's
 * instruction SRAM is fetch-only for ordinary loads/stores (only `l32r`
 * literal loads reach it), so .data/.bss in IRAM would fault on silicon the
 * first time a static is touched. `.rodata` is data in exactly that sense:
 * the guest reads its byte-addressed constants — the format strings the
 * census prints — with ordinary loads, so `.rodata` at instruction-bus
 * addresses would fault on the first byte printed.
 *
 * The stack is not a linker region at all: _start installs it as a bare
 * address at the top of the data-bus window (0x3FCF_FFE0). The window's top
 * 64 KiB (physical 0x3FCF_0000..0x3FD0_0000) is outside both linked regions —
 * it has no instruction-bus address either — so the downward-growing stack
 * can never physically overlap the linked image.
 */

ENTRY(_start);

MEMORY
{
    IRAM : ORIGIN = 0x40370000, LENGTH = 0x20000
    DRAM : ORIGIN = 0x3FCA0000, LENGTH = 0x50000
}

SECTIONS
{
    .text : ALIGN(4)
    {
        /* Xtensa code loads constants with PC-relative `l32r`, whose pools
           live in `.literal*` sections. The Xtensa linker requires each
           literal to be placed BEFORE the code that loads it ("dangerous
           relocation: l32r: literal placed after use" otherwise), so all
           literals go first. Byte-addressed constants are deliberately NOT
           here: `.rodata` has its own output section in the data-bus view
           below, because ordinary loads cannot reach instruction-bus
           addresses. */
        *(.literal .literal.*);
        *(.text .text.*);
    } > IRAM

    .rodata : ALIGN(4)
    {
        /* Byte-addressed constants (format strings, `&str` literals) are read
           with ordinary data loads, which the S3's instruction SRAM does not
           serve — only `l32r` literal loads reach it — and blocks 0-1 have no
           data-bus address at all. Linked at an instruction-bus address, the
           first byte the guest prints would fault on silicon. */
        *(.rodata .rodata.*);
    } > DRAM

    .data : ALIGN(4)
    {
        *(.data .data.*);
    } > DRAM

    .bss (NOLOAD) : ALIGN(4)
    {
        _sbss = .;
        *(.bss .bss.*);
        *(COMMON);
        /* Only the output section's start is aligned here: a final input with
           byte or halfword alignment would leave `_ebss` unaligned, and
           `zero_bss()` clears whole `u32` words — its last volatile write would
           then clear up to three bytes past `.bss`. Rounding the end up to a
           word boundary keeps every word write inside the section; the bytes
           past the true end sit in unlinked DRAM, which nothing else uses. */
        . = ALIGN(4);
        _ebss = .;
    } > DRAM

    /DISCARD/ :
    {
        *(.comment .comment.*);
        *(.note .note.*);
        *(.eh_frame .eh_frame.*);
    }
}
