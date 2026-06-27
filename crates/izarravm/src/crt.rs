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

use egui_wgpu::CallbackTrait;

// CRT look. Two styles selected at runtime by the `style` uniform (0 off,
// 1 subtle, 2 Ye Olde). The subtle look is the approved tuner values; Ye Olde
// adds visible scanlines + shadow mask, 0.02 barrel curvature, softer focus,
// and faint animated grain. An aperture grille would be a mask-branch swap.

const SHADER: &str = r#"
struct U {
  src_size: vec2<f32>,
  style: f32, // 0 off, 1 subtle, 2 Ye Olde
  srgb: f32,
  time: f32,
  pad0: f32,
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

// 8-tap ring average for halation, radius in source texels.
fn glow(t: vec2<f32>, radius: f32) -> vec3<f32> {
  var g = vec3<f32>(0.0);
  let r = radius / u.src_size;
  for (var i = 0; i < 8; i = i + 1) {
    let a = f32(i) / 8.0 * 6.2832;
    g = g + textureSample(tex, samp, t + vec2<f32>(cos(a), sin(a)) * r).rgb;
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

// Exact sRGB -> linear, to cancel an sRGB render target's encode.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
  let lo = c / 12.92;
  let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
  return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let yeolde = u.style > 1.5;

  // Per-style parameters: subtle high-res SVGA vs heavier Ye Olde Screene.
  let sharp         = select(4.0,  2.5,  yeolde);
  let scan_depth    = select(0.03, 0.18, yeolde);
  let beam          = select(0.40, 0.30, yeolde);
  let mask_pitch    = select(2.0,  3.0,  yeolde);
  let mask_strength = select(0.02, 0.12, yeolde);
  let bloom         = select(0.10, 0.25, yeolde);
  let glow_radius   = select(1.2,  1.8,  yeolde);
  let brightness    = select(1.09, 1.22, yeolde);
  let curv          = select(0.0,  0.02, yeolde);

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
    let g = max(glow(t, glow_radius) - vec3<f32>(0.25), vec3<f32>(0.0));
    col = col + g * bloom * vec3<f32>(1.12, 0.98, 0.86);
    col = shadow_mask(col, in.pos.xy, mask_pitch, mask_strength);
    col = col * brightness;
    if (yeolde) {
      // Faint grain reseeded every frame.
      let n = hash13(vec3<f32>(in.pos.xy, u.time * 100.0)) - 0.5;
      col = col + vec3<f32>(n * 0.05);
    }
  }
  col = col * edge;
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

/// Per-paint callback: the optional new frame, the CRT style selector (0 off,
/// 1 subtle, 2 Ye Olde), and a monotonic time in seconds for the Ye Olde grain.
pub struct CrtCallback {
    pub frame: Option<CrtFrame>,
    pub style: u32,
    pub time: f32,
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
        // 8 floats = 32 bytes (std140-safe): src_size.xy, style, srgb, time, pad×3.
        let data: [f32; 8] = [
            w as f32,
            h as f32,
            self.style as f32,
            if res.srgb { 1.0 } else { 0.0 },
            self.time,
            0.0,
            0.0,
            0.0,
        ];
        let mut bytes = [0u8; 32];
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

#[cfg(test)]
mod tests {
    use super::SHADER;

    /// Parse and validate the WGSL through naga so a shader error fails the test
    /// suite instead of panicking at pipeline creation when the GUI launches.
    /// Catches the easy-to-trip cases: textureSample outside uniform control flow,
    /// type mismatches, and uniform-buffer layout errors.
    #[test]
    fn shader_compiles_under_naga() {
        let module = wgpu::naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("WGSL parse error: {e}"));
        let mut validator = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("WGSL validation error: {e}"));
    }
}
