# The wire format

Everything below is the frozen v1 specification, included from
`docs/format/wire-format-v1.md`. That file is the single copy. This chapter shows it rather
than restating it, because a second copy would pass every check that reads the first and
still say something else.

Twenty-one files of frozen bytes hold the format. The `corpus` CI stage decodes them on
every push, and the `wire-format` gate rule pins their lengths and digests, so a case is
added and never regenerated.

---

{{#include ../../format/wire-format-v1.md}}
