use eframe::egui;
use font8x8::{BASIC_FONTS, UnicodeFonts};
use izarravm_video::TextFrame;

const GLYPH_SIZE: usize = 8;
const VGA_PALETTE: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa, 0x555555,
    0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

/// Pack 0x00RRGGBB words into an opaque egui image.
fn words_to_color_image(words: &[u32], width: usize, height: usize) -> egui::ColorImage {
    let mut rgba = vec![0u8; width * height * 4];
    for (i, &color) in words.iter().enumerate().take(width * height) {
        let o = i * 4;
        rgba[o] = ((color >> 16) & 0xff) as u8;
        rgba[o + 1] = ((color >> 8) & 0xff) as u8;
        rgba[o + 2] = (color & 0xff) as u8;
        rgba[o + 3] = 0xff;
    }
    egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba)
}

/// Palette-indexed pixels (mode 13h, the VGA raster core) to an image.
fn indexed_to_color_image(
    pixels: &[u8],
    width: usize,
    height: usize,
    palette: &[u32; 256],
) -> egui::ColorImage {
    let words: Vec<u32> = pixels.iter().map(|&i| palette[i as usize]).collect();
    words_to_color_image(&words, width, height)
}

/// An 80x25 text frame rasterized through the 8x8 font at native 1x.
fn text_to_color_image(frame: &TextFrame) -> egui::ColorImage {
    let width = frame.columns * GLYPH_SIZE;
    let height = frame.rows * GLYPH_SIZE;
    let mut words = vec![VGA_PALETTE[0]; width * height];
    for (index, cell) in frame.cells.iter().enumerate() {
        let column = index % frame.columns;
        let row = index / frame.columns;
        if row >= frame.rows {
            break;
        }
        let character = match cell.character {
            0 => ' ',
            byte => char::from(byte),
        };
        let glyph = BASIC_FONTS.get(character).unwrap_or([0; GLYPH_SIZE]);
        let foreground = VGA_PALETTE[usize::from(cell.attribute & 0x0f)];
        let background = VGA_PALETTE[usize::from((cell.attribute >> 4) & 0x0f)];
        for (glyph_y, bits) in glyph.iter().copied().enumerate() {
            for glyph_x in 0..GLYPH_SIZE {
                let color = if bits & (1 << glyph_x) != 0 {
                    foreground
                } else {
                    background
                };
                let x = column * GLYPH_SIZE + glyph_x;
                let y = row * GLYPH_SIZE + glyph_y;
                words[y * width + x] = color;
            }
        }
    }
    words_to_color_image(&words, width, height)
}

/// Nearest-neighbour integer upscale per axis, as large as fits the target
/// without exceeding it. The caller then lets egui stretch the small remainder
/// with bilinear filtering, which gives a sharp-bilinear look without a shader.
fn sharp_prescale(image: &egui::ColorImage, target_w: usize, target_h: usize) -> egui::ColorImage {
    let [source_w, source_h] = image.size;
    if source_w == 0 || source_h == 0 {
        return image.clone();
    }
    let factor_x = (target_w / source_w).max(1);
    let factor_y = (target_h / source_h).max(1);
    if factor_x == 1 && factor_y == 1 {
        return image.clone();
    }
    let dest_w = source_w * factor_x;
    let dest_h = source_h * factor_y;
    let mut pixels = Vec::with_capacity(dest_w * dest_h);
    for y in 0..dest_h {
        let source_row = (y / factor_y) * source_w;
        for x in 0..dest_w {
            pixels.push(image.pixels[source_row + x / factor_x]);
        }
    }
    egui::ColorImage::new([dest_w, dest_h], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use izarravm_video::TextCell;

    #[test]
    fn indexed_image_maps_through_palette() {
        let pixels = [0u8, 1, 0, 1];
        let mut palette = [0u32; 256];
        palette[1] = 0x00AB_CDEF;
        let image = indexed_to_color_image(&pixels, 2, 2, &palette);
        assert_eq!(image.size, [2, 2]);
        let p = image.pixels[1];
        assert_eq!((p.r(), p.g(), p.b()), (0xAB, 0xCD, 0xEF));
    }

    #[test]
    fn prescale_uses_per_axis_integer_factor() {
        // 2x1 source, target 6x6: x factor 3, y factor 6.
        let src = egui::ColorImage::new(
            [2, 1],
            vec![
                egui::Color32::from_rgb(10, 0, 0),
                egui::Color32::from_rgb(0, 20, 0),
            ],
        );
        let out = sharp_prescale(&src, 6, 6);
        assert_eq!(out.size, [6, 6]);
        // First source pixel fills the left 3 columns, second fills the right 3.
        assert_eq!(out.pixels[0], egui::Color32::from_rgb(10, 0, 0));
        assert_eq!(out.pixels[2], egui::Color32::from_rgb(10, 0, 0));
        assert_eq!(out.pixels[3], egui::Color32::from_rgb(0, 20, 0));
        // Second output row repeats the first (vertical factor applied).
        assert_eq!(out.pixels[6], egui::Color32::from_rgb(10, 0, 0));
    }

    #[test]
    fn prescale_is_identity_when_target_smaller() {
        let src = egui::ColorImage::new([4, 4], vec![egui::Color32::BLACK; 16]);
        let out = sharp_prescale(&src, 3, 3);
        assert_eq!(out.size, [4, 4]);
    }

    #[test]
    fn text_image_is_native_size_and_draws_foreground() {
        let mut cells = vec![TextCell::default(); 80 * 25];
        cells[0] = TextCell {
            character: b'X',
            attribute: 0x0f,
        };
        let frame = TextFrame {
            columns: 80,
            rows: 25,
            cells,
            cursor_offset: 0,
        };
        let image = text_to_color_image(&frame);
        assert_eq!(image.size, [80 * GLYPH_SIZE, 25 * GLYPH_SIZE]);
        let white = egui::Color32::from_rgb(0xff, 0xff, 0xff);
        assert!(image.pixels.contains(&white));
    }
}
