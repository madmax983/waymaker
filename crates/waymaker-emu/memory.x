/* The memory map the emulated images are linked against.
 *
 * One file for both machines rather than one per machine, and that is a fact about the two
 * rather than a convenience. `qemu-system-arm -machine microbit` is an nRF51822: 256 KiB of
 * flash at 0x00000000 and 16 KiB of RAM at 0x20000000. `-machine mps2-an386` has 4 MiB at
 * each of the same two origins. So these lengths are the micro:bit's, and the MPS2 image is
 * linked into the first 256 KiB and 16 KiB of a part that has far more of both.
 *
 * Taking the smaller of the two on purpose: an image that fits the tighter part fits the
 * looser one, and a second file would be a second thing to keep in step with a boot that is
 * meant to be the same boot on both cores.
 */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 256K
  RAM   : ORIGIN = 0x20000000, LENGTH = 16K
}
