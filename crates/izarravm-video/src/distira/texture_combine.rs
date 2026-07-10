// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Distira {
    pub(super) fn selected_color_or_source(
        &self,
        position: (u32, u32),
        source: (u8, u8, u8),
        texture: TextureRgba,
    ) -> (u8, u8, u8) {
        if self.trex_init1[0] & TREXINIT1_SEND_CONFIG != 0 {
            return (0, 0, DISTIRA_TMU_CONFIG);
        }

        let texture_rgb = texture.rgb();
        match self.fbz_color_path & FBZCP_RGB_SELECT_MASK {
            RGB_SELECT_TEXTURE => {
                let format = (self.texture_mode >> 8) & 0xf;
                if !matches!(
                    format,
                    TEX_RGB332
                        | TEX_Y4I2Q2
                        | TEX_A8
                        | TEX_I8
                        | TEX_AI8
                        | TEX_PAL8
                        | TEX_APAL8
                        | TEX_ARGB8332
                        | TEX_A8Y4I2Q2
                        | TEX_R5G6B5
                        | TEX_ARGB1555
                        | TEX_ARGB4444
                        | TEX_A8I8
                        | TEX_APAL88
                ) {
                    return source;
                }
                texture_rgb
            }
            RGB_SELECT_COLOR1 => (
                (self.color1 >> 16) as u8,
                (self.color1 >> 8) as u8,
                self.color1 as u8,
            ),
            RGB_SELECT_LFB => self.read_back_pixel_rgb(position.0, position.1),
            _ => source,
        }
    }

    pub(super) fn texture_color_or_source(
        &self,
        selected: (u8, u8, u8),
        source: (u8, u8, u8),
        alocal: u8,
        aother: u8,
        texture: TextureRgba,
    ) -> (u8, u8, u8) {
        let texture_rgb = texture.rgb();
        let color = self.apply_color_path_local_combine(
            selected,
            source,
            alocal,
            aother,
            texture.alpha,
            texture_rgb,
        );
        self.apply_color_path_output_invert(color)
    }

    pub(super) fn combined_texture(&self, samples: [TextureSample; 2]) -> TextureRgba {
        let mode = self.texture_mode;
        if mode & TEXTUREMODE_LOCAL_MASK == TEXTUREMODE_LOCAL {
            return self.sample_tmu_rgba(0, samples[0]);
        }
        if mode & TEXTUREMODE_COMBINE_MASK == TEXTUREMODE_PASSTHROUGH {
            return self.sample_tmu_rgba(1, samples[1]);
        }

        let local1 = self.sample_tmu_rgba(1, samples[1]);
        let downstream = self.combine_terminal_texture(local1, samples[1]);
        let local0 = self.sample_tmu_rgba(0, samples[0]);
        self.combine_texture_rgba(0, downstream, local0, samples[0])
    }

    fn sample_tmu_rgba(&self, tmu: usize, sample: TextureSample) -> TextureRgba {
        if self.texture_mode_for_tmu(tmu) & 0x6 != 0 {
            return self.sample_tmu_bilinear_rgba(tmu, sample);
        }
        self.sample_tmu_nearest_rgba(tmu, sample)
    }

    fn sample_tmu_nearest_rgba(&self, tmu: usize, sample: TextureSample) -> TextureRgba {
        let (red, green, blue) = self.sample_tmu_texture(tmu, sample);
        TextureRgba {
            red,
            green,
            blue,
            alpha: self.sample_tmu_alpha(tmu, sample),
        }
    }

    fn sample_tmu_bilinear_rgba(&self, tmu: usize, sample: TextureSample) -> TextureRgba {
        let scale = (1_u32 << sample.lod).max(1) as f32;
        let mode = self.texture_mode_for_tmu(tmu);
        let lod_reg = self.texture_lod_for_tmu(tmu);
        let (width, height) = texture_dimensions(lod_reg, sample.lod);
        let mirror = |coord: f32, size: usize, enabled: bool| {
            if !enabled {
                return coord;
            }
            let period = (size * 2) as f32;
            let coord = coord.rem_euclid(period);
            if coord >= size as f32 {
                period - coord - 1.0 / 16.0
            } else {
                coord
            }
        };
        let s = mirror(sample.s / scale, width, lod_reg & LOD_TMIRROR_S != 0) - 0.5;
        let t = mirror(sample.t / scale, height, lod_reg & LOD_TMIRROR_T != 0) - 0.5;
        let base_s = s.floor();
        let base_t = t.floor();
        let frac_s = ((s - base_s) * 16.0).floor().clamp(0.0, 15.0) as u32;
        let frac_t = ((t - base_t) * 16.0).floor().clamp(0.0, 15.0) as u32;
        let samples = [
            (base_s, base_t),
            (base_s + 1.0, base_t),
            (base_s, base_t + 1.0),
            (base_s + 1.0, base_t + 1.0),
        ]
        .map(|(s, t)| {
            let s =
                texture_coord_index_i32(s as i32, width, mode & TEXTUREMODE_TCLAMPS != 0, false);
            let t =
                texture_coord_index_i32(t as i32, height, mode & TEXTUREMODE_TCLAMPT != 0, false);
            self.sample_tmu_nearest_rgba(
                tmu,
                TextureSample {
                    s: s as f32 * scale,
                    t: t as f32 * scale,
                    ..sample
                },
            )
        });
        let weights = [
            (16 - frac_s) * (16 - frac_t),
            frac_s * (16 - frac_t),
            (16 - frac_s) * frac_t,
            frac_s * frac_t,
        ];
        let blend = |component: fn(TextureRgba) -> u8| {
            samples
                .iter()
                .zip(weights)
                .map(|(&sample, weight)| u32::from(component(sample)) * weight)
                .sum::<u32>()
                .checked_shr(8)
                .unwrap_or(0)
                .min(255) as u8
        };
        TextureRgba {
            red: blend(|sample| sample.red),
            green: blend(|sample| sample.green),
            blue: blend(|sample| sample.blue),
            alpha: blend(|sample| sample.alpha),
        }
    }

    fn combine_terminal_texture(&self, local: TextureRgba, sample: TextureSample) -> TextureRgba {
        let mode = self.texture_mode_tmu1;
        let (red, green, blue) = if mode & TC_SUB_CLOCAL != 0 {
            self.combine_texture_rgb(1, TextureRgba::TRANSPARENT_BLACK, local, sample)
        } else {
            local.rgb()
        };
        let alpha = if mode & TCA_SUB_CLOCAL != 0 {
            self.combine_texture_alpha(1, TextureRgba::TRANSPARENT_BLACK, local, sample)
        } else {
            local.alpha
        };
        TextureRgba {
            red,
            green,
            blue,
            alpha,
        }
    }

    fn combine_texture_rgba(
        &self,
        tmu: usize,
        other: TextureRgba,
        local: TextureRgba,
        sample: TextureSample,
    ) -> TextureRgba {
        let (red, green, blue) = self.combine_texture_rgb(tmu, other, local, sample);
        TextureRgba {
            red,
            green,
            blue,
            alpha: self.combine_texture_alpha(tmu, other, local, sample),
        }
    }

    fn combine_texture_rgb(
        &self,
        tmu: usize,
        other: TextureRgba,
        local: TextureRgba,
        sample: TextureSample,
    ) -> (u8, u8, u8) {
        let mode = self.texture_mode_for_tmu(tmu);
        let mut color = if mode & TC_ZERO_OTHER != 0 {
            [0_i32; 3]
        } else {
            [
                i32::from(other.red),
                i32::from(other.green),
                i32::from(other.blue),
            ]
        };
        let local_rgb = [
            i32::from(local.red),
            i32::from(local.green),
            i32::from(local.blue),
        ];
        if mode & TC_SUB_CLOCAL != 0 {
            for (color, local) in color.iter_mut().zip(local_rgb) {
                *color -= local;
            }
        }

        let factor = match (mode >> TC_MSELECT_SHIFT) & TC_MSELECT_MASK {
            TC_MSELECT_CLOCAL => local_rgb,
            TC_MSELECT_AOTHER => [i32::from(other.alpha); 3],
            TC_MSELECT_ALOCAL => [i32::from(local.alpha); 3],
            TC_MSELECT_DETAIL => [i32::from(self.texture_detail_factor(tmu, sample.lod_floor)); 3],
            TC_MSELECT_LOD_FRAC => [i32::from(sample.lod_fraction); 3],
            _ => [0; 3],
        };
        let reverse = (mode & TC_REVERSE_BLEND != 0)
            ^ (mode & TEXTUREMODE_TRILINEAR != 0 && sample.lod_floor & 1 != 0);
        for (color, factor) in color.iter_mut().zip(factor) {
            let factor = if reverse {
                factor + 1
            } else {
                (factor ^ 0xff) + 1
            };
            *color = (*color * factor) >> 8;
        }
        if mode & TC_ADD_CLOCAL != 0 {
            for (color, local) in color.iter_mut().zip(local_rgb) {
                *color += local;
            }
        } else if mode & TC_ADD_ALOCAL != 0 {
            color
                .iter_mut()
                .for_each(|color| *color += i32::from(local.alpha));
        }
        let mut color = (
            color[0].clamp(0, 255) as u8,
            color[1].clamp(0, 255) as u8,
            color[2].clamp(0, 255) as u8,
        );
        if mode & TC_INVERT_OUTPUT != 0 {
            color = (color.0 ^ 0xff, color.1 ^ 0xff, color.2 ^ 0xff);
        }
        color
    }

    fn combine_texture_alpha(
        &self,
        tmu: usize,
        other: TextureRgba,
        local: TextureRgba,
        sample: TextureSample,
    ) -> u8 {
        let mode = self.texture_mode_for_tmu(tmu);
        let mut alpha = if mode & TCA_ZERO_OTHER != 0 {
            0
        } else {
            i32::from(other.alpha)
        };
        if mode & TCA_SUB_CLOCAL != 0 {
            alpha -= i32::from(local.alpha);
        }
        let factor = match (mode >> TCA_MSELECT_SHIFT) & TCA_MSELECT_MASK {
            TCA_MSELECT_CLOCAL | TCA_MSELECT_ALOCAL => i32::from(local.alpha),
            TCA_MSELECT_AOTHER => i32::from(other.alpha),
            TCA_MSELECT_DETAIL => i32::from(self.texture_detail_factor(tmu, sample.lod_floor)),
            TCA_MSELECT_LOD_FRAC => i32::from(sample.lod_fraction),
            _ => 0,
        };
        let reverse = (mode & TCA_REVERSE_BLEND != 0)
            ^ (mode & TEXTUREMODE_TRILINEAR != 0 && sample.lod_floor & 1 != 0);
        let factor = if reverse {
            factor + 1
        } else {
            (factor ^ 0xff) + 1
        };
        alpha = (alpha * factor) >> 8;
        if mode & (TCA_ADD_CLOCAL | TCA_ADD_ALOCAL) != 0 {
            alpha += i32::from(local.alpha);
        }
        let mut alpha = alpha.clamp(0, 255) as u8;
        if mode & TCA_INVERT_OUTPUT != 0 {
            alpha ^= 0xff;
        }
        alpha
    }

    fn apply_color_path_output_invert(&self, color: (u8, u8, u8)) -> (u8, u8, u8) {
        if self.fbz_color_path & FBZCP_CC_INVERT_OUTPUT == 0 {
            return color;
        }
        (color.0 ^ 0xff, color.1 ^ 0xff, color.2 ^ 0xff)
    }
}
