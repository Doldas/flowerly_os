# C

Low-level C lives here only where a Rust or kernel alternative is not
appropriate: boot shims, early initramfs pieces, and tiny helpers.

- `lowlevel/` — small C components used around the image boot path

Rust is the primary system language; new C must be justified against
`adr/0001-rust-as-primary-system-language.md`. Every `unsafe` and every C
buffer promises documented bounds reasoning or it is rejected in review.