// Authoring-only: write the built-in CP437 glyphs (8x8, 8x14, 8x16, in that
// order, concatenated) to tools/cp437-fonts.bin so the font-blob generator can
// reuse them verbatim and keep CP437 byte-identical to what ships.
use izarravm_video::font::{VGAFONT_8X14, VGAFONT_8X16, VGAFONT_8X8};

fn main() {
    let mut out = Vec::new();
    out.extend_from_slice(&VGAFONT_8X8);
    out.extend_from_slice(&VGAFONT_8X14);
    out.extend_from_slice(&VGAFONT_8X16);
    std::fs::write("tools/cp437-fonts.bin", &out).expect("write tools/cp437-fonts.bin");
    eprintln!("wrote {} bytes", out.len());
}
