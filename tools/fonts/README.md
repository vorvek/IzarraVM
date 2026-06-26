# Vendored fonts

`ModernDOS8x8.ttf`, `ModernDOS8x14.ttf`, `ModernDOS8x16.ttf` are the native
per-size Modern DOS fonts by Jayvee Enaguas, released under CC0 1.0 (public
domain). Vendored from https://github.com/notpeter/ttf-moderndos (the `ttf/`
directory) so the code-page font generator runs offline.

These feed `tools/gen_codepage_fonts.py`, which renders the glyphs the alternate
code pages add beyond CP437. Our shipped CP437 font was already built from Modern
DOS, so CP437 stays byte-identical.
