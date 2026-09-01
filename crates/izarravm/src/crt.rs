// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The monitor presentation pass: a wgpu shader that stretches the guest
//! framebuffer to fill the 4:3 rect (correct pixel aspect for every mode) and,
//! when enabled, adds a faithful high-resolution-CRT look — sharp upscale, a
//! faint gaussian scanline beam, a barely-there shadow mask, and light halation.
//!
//! Drawn through an `egui_wgpu` paint callback so it composites inside egui's own
//! render pass. `CrtResources` (pipeline, sampler, source texture, uniform, bind
//! group) lives in the renderer's `callback_resources`; `CrtCallback` carries the
//! per-frame data (new framebuffer bytes when the guest advanced, the CRT style
//! selector, and a time for the Ye Olde grain) and uploads it in `prepare`.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use egui_wgpu::CallbackTrait;

static PACK_WALL_NS: AtomicU64 = AtomicU64::new(0);
static UPLOAD_SUBMIT_WALL_NS: AtomicU64 = AtomicU64::new(0);
static UPLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
static UPLOAD_ROWS: AtomicU64 = AtomicU64::new(0);
static UPLOAD_RUNS: AtomicU64 = AtomicU64::new(0);
static FULL_UPLOADS: AtomicU64 = AtomicU64::new(0);
static PARTIAL_UPLOADS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PresentationMetricsSnapshot {
    pub pack_wall_ns: u64,
    pub upload_submit_wall_ns: u64,
    pub upload_bytes: u64,
    pub upload_rows: u64,
    pub upload_runs: u64,
    pub full_uploads: u64,
    pub partial_uploads: u64,
}

impl PresentationMetricsSnapshot {
    pub(crate) fn delta_since(self, previous: Self) -> Self {
        Self {
            pack_wall_ns: self.pack_wall_ns.saturating_sub(previous.pack_wall_ns),
            upload_submit_wall_ns: self
                .upload_submit_wall_ns
                .saturating_sub(previous.upload_submit_wall_ns),
            upload_bytes: self.upload_bytes.saturating_sub(previous.upload_bytes),
            upload_rows: self.upload_rows.saturating_sub(previous.upload_rows),
            upload_runs: self.upload_runs.saturating_sub(previous.upload_runs),
            full_uploads: self.full_uploads.saturating_sub(previous.full_uploads),
            partial_uploads: self
                .partial_uploads
                .saturating_sub(previous.partial_uploads),
        }
    }
}

pub(crate) fn presentation_metrics_snapshot() -> PresentationMetricsSnapshot {
    PresentationMetricsSnapshot {
        pack_wall_ns: PACK_WALL_NS.load(Ordering::Relaxed),
        upload_submit_wall_ns: UPLOAD_SUBMIT_WALL_NS.load(Ordering::Relaxed),
        upload_bytes: UPLOAD_BYTES.load(Ordering::Relaxed),
        upload_rows: UPLOAD_ROWS.load(Ordering::Relaxed),
        upload_runs: UPLOAD_RUNS.load(Ordering::Relaxed),
        full_uploads: FULL_UPLOADS.load(Ordering::Relaxed),
        partial_uploads: PARTIAL_UPLOADS.load(Ordering::Relaxed),
    }
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

// CRT look. Two styles selected at runtime by the `style` uniform (0 off,
// 1 subtle, 2 Ye Olde). Both styles model a high-resolution VGA monitor; Ye
// Olde adds more glass, halation, fine mask texture, light curvature, and faint
// grain without TV/composite artifacts.

const SHADER: &str = r#"
struct U {
  src_size: vec2<f32>,
  style: f32, // 0 off, 1 subtle, 2 Ye Olde
  srgb: f32,
  time: f32,
  monitor_gamma: f32, // 0 = Raw (identity); otherwise the assumed CRT EOTF exponent
  pad1: f32,
  pad2: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
  var corners = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
  let q = corners[idx];
  var o: VsOut;
  o.pos = vec4<f32>(q, 0.0, 1.0);
  // Flip Y so uv.y = 0 is the top of the rect (texture row 0).
  o.uv = vec2<f32>(q.x * 0.5 + 0.5, 1.0 - (q.y * 0.5 + 0.5));
  return o;
}

// Sharp-bilinear with adjustable softness (higher `sharp` = crisper edges).
fn sample_sharp(t: vec2<f32>, sharp: f32) -> vec3<f32> {
  let px = t * u.src_size - vec2<f32>(0.5);
  let tf = floor(px);
  var f = px - tf;
  f = clamp((f - 0.5) * sharp + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
  let s = (tf + 0.5 + f) / u.src_size;
  return textureSample(tex, samp, s).rgb;
}

// 8-tap bright-source halation, radius in source texels. Dark samples add
// nothing; they never average down neighboring bright phosphor.
fn glow(t: vec2<f32>, radius: f32) -> vec3<f32> {
  var g = vec3<f32>(0.0);
  let r = radius / u.src_size;
  for (var i = 0; i < 8; i = i + 1) {
    let a = f32(i) / 8.0 * 6.2832;
    let s = textureSample(tex, samp, t + vec2<f32>(cos(a), sin(a)) * r).rgb;
    g = g + max(s - vec3<f32>(0.25), vec3<f32>(0.0));
  }
  return g / 8.0;
}

// Staggered RGB shadow-mask triads in physical output-pixel space.
fn shadow_mask(col: vec3<f32>, frag: vec2<f32>, pitch: f32, strength: f32) -> vec3<f32> {
  let lo = 1.0 - strength;
  let row = floor(frag.y / (pitch * 1.5)) % 2.0;
  let s = floor(frag.x / pitch + row * 1.5) % 3.0;
  var m = vec3<f32>(lo);
  if (s < 0.5) { m.r = 1.0; } else if (s < 1.5) { m.g = 1.0; } else { m.b = 1.0; }
  let gap = mix(1.0, lo, 0.5);
  let hg = step(1.0, floor(frag.y / (pitch * 0.75)) % 2.0) * 0.6;
  m = m * mix(1.0, gap, hg);
  return col * m;
}

// Decorrelated grain hash (Dave Hoskins, "hash without sine"): three inputs to
// one value, no sin() iso-lines to band along. Time goes in as the third input
// so each frame reseeds the whole field instead of translating it, which is what
// produced the diagonal scrolling stripes.
fn hash13(p: vec3<f32>) -> f32 {
  var q = fract(p * 0.1031);
  q = q + dot(q, q.zyx + 31.32);
  return fract((q.x + q.y) * q.z);
}

// Exact sRGB -> linear, to cancel an sRGB render target's encode. Used by
// styles Subtle and Ye Olde, and by Off when monitor_gamma is Raw (0.0).
fn to_linear(c: vec3<f32>) -> vec3<f32> {
  let lo = c / 12.92;
  let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
  return select(hi, lo, c <= vec3<f32>(0.04045));
}

// CRT EOTF: decode a nonlinear DAC code into the linear light a period
// monitor at this gamma would have emitted. The decode half of
// display_transform's correction (crates/izarravm/src/display_transform.rs).
fn to_light(c: vec3<f32>, gamma: f32) -> vec3<f32> {
  return pow(c, vec3<f32>(gamma));
}

// Exact sRGB OETF (IEC 61966-2-1), the re-encode half of display_transform's
// correction. Constants must match display_transform.rs's exactly; a
// crt_test.rs test checks it.
fn srgb_oetf(l: vec3<f32>) -> vec3<f32> {
  let lo = l * 12.92;
  let hi = 1.055 * pow(l, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
  return select(hi, lo, l <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let yeolde = u.style > 1.5;

  // Per-style parameters: subtle VGA monitor vs stronger high-resolution glass.
  let sharp         = select(2.5,   2.2,   yeolde);
  let scan_depth    = select(0.015, 0.288, yeolde);
  let beam          = select(0.45,  0.5,   yeolde);
  let mask_pitch    = select(1.0,   1.0,   yeolde);
  let mask_strength = select(0.004, 0.083, yeolde);
  let bloom         = select(0.16,  0.3,   yeolde);
  let glow_radius   = select(1.3,   2.0,   yeolde);
  let brightness    = select(1.06,  1.15,  yeolde);
  let curv          = select(0.0,   0.015, yeolde);

  // Ye Olde barrel curvature: warp the sample coord; pixels off the tube are
  // blacked out at the very end. We clamp the warped coord so the texture sample
  // below stays in uniform control flow (no per-pixel early return), which WGSL
  // requires for sampling.
  var t = in.uv;
  var edge = 1.0;
  if (curv > 0.0) {
    let c = in.uv * 2.0 - 1.0;
    let o = c.yx * c.yx * curv;
    let w = (c + c * o) * 0.5 + 0.5;
    // Antialias the curved border: fade coverage to 0 across the ~1px band
    // where the warped coord crosses the [0,1] edge, using the screen-space
    // derivative so only that border ring softens, not the interior image.
    let d = min(w, vec2<f32>(1.0) - w);
    let aa = fwidth(w);
    let cov = clamp(d / max(aa, vec2<f32>(1e-6)), vec2<f32>(0.0), vec2<f32>(1.0));
    edge = cov.x * cov.y;
    t = clamp(w, vec2<f32>(0.0), vec2<f32>(1.0));
  }

  var col = sample_sharp(t, sharp);
  if (u.style > 0.5) {
    let fy = fract(t.y * u.src_size.y) - 0.5;
    let b = exp(-(fy * fy) / (2.0 * beam * beam));
    col = col * mix(1.0, b, scan_depth);
    col = col + glow(t, glow_radius) * bloom * vec3<f32>(1.12, 0.98, 0.86);
    col = shadow_mask(col, in.pos.xy, mask_pitch, mask_strength);
    col = col * brightness;
    if (yeolde) {
      // Faint grain reseeded every frame.
      let n = hash13(vec3<f32>(in.pos.xy, u.time * 100.0)) - 0.5;
      col = col + vec3<f32>(n * 0.025);
    }
  }
  col = col * edge;
  col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));

  // Display-gamma correction, style Off only (u.style < 0.5): decode with
  // the assumed CRT EOTF, re-encode with the exact sRGB OETF, per
  // dev_docs/2026-09-01-display-gamma-design.md section 4.2 step 8. Styles
  // Subtle and Ye Olde, and Off at monitor_gamma == 0.0 ("Raw", an explicit
  // branch -- pow(c, 0.0) is NOT the identity), keep today's exact
  // to_linear cancellation.
  if (u.style < 0.5 && u.monitor_gamma > 0.0) {
    let light = to_light(col, u.monitor_gamma);
    if (u.srgb > 0.5) {
      // The sRGB render target's own hardware encode IS the re-encode step;
      // handing it the linear value directly is exact, not an approximation.
      col = light;
    } else {
      col = srgb_oetf(light);
    }
  } else if (u.srgb > 0.5) {
    col = to_linear(col);
  }
  return vec4<f32>(col, 1.0);
}
"#;

/// A new guest framebuffer plus the scanline runs that changed.
///
/// `changed_rows` is a delta against the frame BEFORE this one, so applying it
/// to a texture that never received that frame leaves the untouched rows stale
/// forever -- there is no later frame that repairs them, because every later
/// frame reports only its own changes. `update_from`/`update_to` carry the
/// publication numbers the runs span so `prepare` can notice the gap and upload
/// the frame whole instead. Nothing acknowledges a paint, and nothing needs to:
/// a dropped frame costs one full upload and corrects itself.
pub struct CrtFrame {
    pub words: Arc<Vec<u32>>,
    pub changed_rows: Vec<Range<usize>>,
    pub width: u32,
    pub height: u32,
    pub update_from: u64,
    pub update_to: u64,
}

/// Whether the texture must take the frame whole rather than by its runs.
///
/// `last_update` is the publication number this texture last had applied, or
/// `u64::MAX` for "nothing yet" -- publications start at 1, so the wrap makes
/// the very first frame answer `true` here on its own merits rather than by
/// coincidence.
pub(crate) fn upload_is_full(frame_update_from: u64, last_update: u64, recreated: bool) -> bool {
    recreated || frame_update_from != last_update.wrapping_add(1)
}

/// Per-paint callback: the optional new frame, the CRT style selector (0 off,
/// 1 subtle, 2 Ye Olde), a monotonic time in seconds for the Ye Olde grain, and
/// the assumed monitor gamma (0.0 is the "Raw" sentinel: identity, matching
/// `display_transform`'s `None`).
pub struct CrtCallback {
    pub frame: Option<CrtFrame>,
    pub style: u32,
    pub time: f32,
    pub monitor_gamma: f32,
}

/// Persistent GPU resources, stored in the renderer's `callback_resources`.
pub struct CrtResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    dims: (u32, u32),
    srgb: bool,
    upload_scratch: Vec<u8>,
    last_update: u64,
}

pub(crate) fn pack_argb_rows(words: &[u32], width: usize, rows: Range<usize>, out: &mut Vec<u8>) {
    let start = rows.start.saturating_mul(width).min(words.len());
    let end = rows.end.saturating_mul(width).min(words.len());
    out.clear();
    out.reserve(end.saturating_sub(start).saturating_mul(4));
    for &color in &words[start..end] {
        out.extend_from_slice(&[
            ((color >> 16) & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
            (color & 0xff) as u8,
            0xff,
        ]);
    }
}

/// Pack the shader's uniform block: 8 floats (32 bytes, std140-safe) --
/// `src_size.xy, style, srgb, time, monitor_gamma, pad1, pad2` -- little-endian,
/// matching WGSL struct `U` in `SHADER`. Pulled out of `prepare` so the byte
/// layout (buffer size, and which offset `monitor_gamma` lands at) is a plain
/// unit-testable function.
pub(crate) fn uniform_bytes(
    width: f32,
    height: f32,
    style: f32,
    srgb: bool,
    time: f32,
    monitor_gamma: f32,
) -> [u8; 32] {
    let data: [f32; 8] = [
        width,
        height,
        style,
        if srgb { 1.0 } else { 0.0 },
        time,
        monitor_gamma,
        0.0,
        0.0,
    ];
    let mut bytes = [0u8; 32];
    for (i, v) in data.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

fn source_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("crt-source"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("crt-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

impl CrtResources {
    /// Build the pipeline, sampler, uniform, and a 1x1 black source texture.
    /// `format` is the surface format the egui pass renders to; the pipeline
    /// target must match it, and `is_srgb()` decides whether the shader cancels
    /// an sRGB encode.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crt-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt-pll"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("crt-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crt-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt-uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = source_texture(device, 1, 1);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8, 0, 0, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = make_bind_group(device, &bind_group_layout, &uniform, &view, &sampler);
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform,
            texture,
            bind_group,
            dims: (1, 1),
            srgb: format.is_srgb(),
            upload_scratch: Vec::new(),
            last_update: u64::MAX,
        }
    }

    /// Recreate the source texture (and its bind group) when the guest mode
    /// changes the framebuffer dimensions.
    fn ensure_texture(&mut self, device: &wgpu::Device, w: u32, h: u32) -> bool {
        if self.dims == (w, h) {
            return false;
        }
        let texture = source_texture(device, w, h);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group = make_bind_group(
            device,
            &self.bind_group_layout,
            &self.uniform,
            &view,
            &self.sampler,
        );
        self.texture = texture;
        self.dims = (w, h);
        true
    }
}

impl CallbackTrait for CrtCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res = resources
            .get_mut::<CrtResources>()
            .expect("CrtResources registered at init");
        if let Some(frame) = &self.frame {
            let recreated = res.ensure_texture(device, frame.width, frame.height);
            let full = 0..frame.height as usize;
            let rows = if upload_is_full(frame.update_from, res.last_update, recreated) {
                std::slice::from_ref(&full)
            } else {
                frame.changed_rows.as_slice()
            };
            res.last_update = frame.update_to;
            for rows in rows {
                let start = rows.start.min(frame.height as usize);
                let end = rows.end.min(frame.height as usize);
                if start >= end {
                    continue;
                }
                let pack_started = Instant::now();
                pack_argb_rows(
                    &frame.words,
                    frame.width as usize,
                    start..end,
                    &mut res.upload_scratch,
                );
                PACK_WALL_NS.fetch_add(duration_ns(pack_started.elapsed()), Ordering::Relaxed);
                let upload_started = Instant::now();
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &res.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: start as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &res.upload_scratch,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * frame.width),
                        rows_per_image: Some((end - start) as u32),
                    },
                    wgpu::Extent3d {
                        width: frame.width,
                        height: (end - start) as u32,
                        depth_or_array_layers: 1,
                    },
                );
                UPLOAD_SUBMIT_WALL_NS
                    .fetch_add(duration_ns(upload_started.elapsed()), Ordering::Relaxed);
                UPLOAD_BYTES.fetch_add(res.upload_scratch.len() as u64, Ordering::Relaxed);
                UPLOAD_ROWS.fetch_add((end - start) as u64, Ordering::Relaxed);
                UPLOAD_RUNS.fetch_add(1, Ordering::Relaxed);
                if start == 0 && end == frame.height as usize {
                    FULL_UPLOADS.fetch_add(1, Ordering::Relaxed);
                } else {
                    PARTIAL_UPLOADS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let (w, h) = res.dims;
        let bytes = uniform_bytes(
            w as f32,
            h as f32,
            self.style as f32,
            res.srgb,
            self.time,
            self.monitor_gamma,
        );
        queue.write_buffer(&res.uniform, 0, &bytes);
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::epaint::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res = resources
            .get::<CrtResources>()
            .expect("CrtResources registered at init");
        // Map the fullscreen triangle to exactly the 4:3 rect (egui already set
        // the scissor to the callback's clip rect).
        let vp = info.viewport_in_pixels();
        render_pass.set_viewport(
            vp.left_px as f32,
            vp.top_px as f32,
            vp.width_px as f32,
            vp.height_px as f32,
            0.0,
            1.0,
        );
        render_pass.set_pipeline(&res.pipeline);
        render_pass.set_bind_group(0, &res.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
#[path = "crt_test.rs"]
mod tests;
