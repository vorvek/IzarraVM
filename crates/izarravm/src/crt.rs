//! The monitor presentation pass: a wgpu shader that stretches the guest
//! framebuffer to fill the 4:3 rect (correct pixel aspect for every mode) and,
//! when enabled, adds a faithful high-resolution-CRT look — sharp upscale, a
//! faint gaussian scanline beam, a barely-there shadow mask, and light halation.
//!
//! Drawn through an `egui_wgpu` paint callback so it composites inside egui's own
//! render pass. `CrtResources` (pipeline, sampler, source texture, uniform, bind
//! group) lives in the renderer's `callback_resources`; `CrtCallback` carries the
//! per-frame data (new framebuffer bytes when the guest advanced, plus the on/off
//! flag) and uploads it in `prepare`.

use egui_wgpu::CallbackTrait;

// CRT model constants, locked from the approved tuner. A single shadow-mask
// look; the only runtime parameter is on/off. To switch to an aperture grille,
// change the mask branch in the shader — not exposed in the UI by design.
// (kept inline in the WGSL below; listed here for reference)
//   scanDepth 0.03  beam 0.40  sharp 4.0  maskStrength 0.02  maskPitch 2.0
//   bloom 0.10  glowRadius 1.2  brightness 1.09  (curvature 0 — omitted)

const SHADER: &str = r#"
struct U {
  src_size: vec2<f32>,
  crt_on: f32,
  srgb: f32,
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

const SCAN_DEPTH: f32 = 0.03;
const BEAM: f32 = 0.40;
const SHARP: f32 = 4.0;
const MASK_STRENGTH: f32 = 0.02;
const MASK_PITCH: f32 = 2.0;
const BLOOM: f32 = 0.10;
const GLOW_RADIUS: f32 = 1.2;
const BRIGHTNESS: f32 = 1.09;

// Sharp-bilinear: remap the fractional texel toward a step so edges are crisp
// without nearest-neighbour stair-stepping. Texture sampler is LINEAR.
fn sample_sharp(t: vec2<f32>) -> vec3<f32> {
  let px = t * u.src_size - vec2<f32>(0.5);
  let tf = floor(px);
  var f = px - tf;
  f = clamp((f - 0.5) * SHARP + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
  let s = (tf + 0.5 + f) / u.src_size;
  return textureSample(tex, samp, s).rgb;
}

// 8-tap ring average for halation.
fn glow(t: vec2<f32>) -> vec3<f32> {
  var g = vec3<f32>(0.0);
  let r = GLOW_RADIUS / u.src_size;
  for (var i = 0; i < 8; i = i + 1) {
    let a = f32(i) / 8.0 * 6.2832;
    g = g + textureSample(tex, samp, t + vec2<f32>(cos(a), sin(a)) * r).rgb;
  }
  return g / 8.0;
}

// Shadow mask in physical output-pixel space: staggered RGB triads plus a faint
// horizontal-gap term.
fn shadow_mask(col: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
  let lo = 1.0 - MASK_STRENGTH;
  let row = floor(frag.y / (MASK_PITCH * 1.5)) % 2.0;
  let s = floor(frag.x / MASK_PITCH + row * 1.5) % 3.0;
  var m = vec3<f32>(lo);
  if (s < 0.5) { m.r = 1.0; } else if (s < 1.5) { m.g = 1.0; } else { m.b = 1.0; }
  let gap = mix(1.0, lo, 0.5);
  let hg = step(1.0, floor(frag.y / (MASK_PITCH * 0.75)) % 2.0) * 0.6;
  m = m * mix(1.0, gap, hg);
  return col * m;
}

// Exact sRGB -> linear, used only to cancel an sRGB render target's encode so
// the on-screen result matches the tuner's nonlinear math.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
  let lo = c / 12.92;
  let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
  return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  var col = sample_sharp(in.uv);
  if (u.crt_on > 0.5) {
    let fy = fract(in.uv.y * u.src_size.y) - 0.5;
    let b = exp(-(fy * fy) / (2.0 * BEAM * BEAM));
    col = col * mix(1.0, b, SCAN_DEPTH);
    let g = max(glow(in.uv) - vec3<f32>(0.25), vec3<f32>(0.0));
    col = col + g * BLOOM * vec3<f32>(1.12, 0.98, 0.86);
    col = shadow_mask(col, in.pos.xy);
    col = col * BRIGHTNESS;
  }
  col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
  if (u.srgb > 0.5) { col = to_linear(col); }
  return vec4<f32>(col, 1.0);
}
"#;

/// A new guest framebuffer to upload (RGBA8), with its dimensions. Built only
/// when the guest frame counter advances.
pub struct CrtFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Per-paint callback: the optional new frame plus the live on/off flag.
pub struct CrtCallback {
    pub frame: Option<CrtFrame>,
    pub crt_on: bool,
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
            size: 16,
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
        }
    }

    /// Recreate the source texture (and its bind group) when the guest mode
    /// changes the framebuffer dimensions.
    fn ensure_texture(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.dims == (w, h) {
            return;
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
            res.ensure_texture(device, frame.width, frame.height);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &res.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * frame.width),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let (w, h) = res.dims;
        let data: [f32; 4] = [
            w as f32,
            h as f32,
            if self.crt_on { 1.0 } else { 0.0 },
            if res.srgb { 1.0 } else { 0.0 },
        ];
        let mut bytes = [0u8; 16];
        for (i, v) in data.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
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
