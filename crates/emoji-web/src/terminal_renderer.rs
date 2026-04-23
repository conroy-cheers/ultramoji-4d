use std::collections::HashMap;

use anyhow::Result;

static FONT_BYTES: &[u8] = include_bytes!("Glass_TTY_VT220.ttf");

pub const TERM_COLS: u16 = 80;
pub const TERM_ROWS: u16 = 40;
const TERM_RENDER_SCALE: usize = 2;
const BASE_FONT_SIZE: f32 = 20.0;

#[derive(Clone, Copy)]
pub struct TerminalCell {
    pub ch: char,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: [0, 0, 0, 0],
            bg: [0, 0, 0, 0],
        }
    }
}

pub struct TerminalGrid {
    cells: Vec<TerminalCell>,
}

#[derive(Clone, Copy)]
pub struct OccupiedRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl TerminalGrid {
    pub fn new() -> Self {
        Self {
            cells: vec![TerminalCell::default(); TERM_COLS as usize * TERM_ROWS as usize],
        }
    }

    pub fn clear(&mut self, bg: [u8; 4]) {
        for cell in &mut self.cells {
            *cell = TerminalCell {
                ch: ' ',
                fg: [0, 0, 0, 0],
                bg,
            };
        }
    }

    pub fn put_cell(&mut self, x: u16, y: u16, ch: char, fg: [u8; 4], bg: [u8; 4]) {
        if x >= TERM_COLS || y >= TERM_ROWS {
            return;
        }
        self.cells[y as usize * TERM_COLS as usize + x as usize] = TerminalCell { ch, fg, bg };
    }

    pub fn put_text(&mut self, x: u16, y: u16, text: &str, fg: [u8; 4], bg: [u8; 4]) {
        if y >= TERM_ROWS {
            return;
        }
        for (i, ch) in text.chars().enumerate() {
            let xi = x as usize + i;
            if xi >= TERM_COLS as usize {
                break;
            }
            self.put_cell(xi as u16, y, ch, fg, bg);
        }
    }

    pub fn put_centered(&mut self, y: u16, text: &str, fg: [u8; 4], bg: [u8; 4]) {
        let len = text.chars().count().min(TERM_COLS as usize);
        let x = ((TERM_COLS as usize).saturating_sub(len)) / 2;
        self.put_text(x as u16, y, text, fg, bg);
    }

    pub fn cells(&self) -> &[TerminalCell] {
        &self.cells
    }

    pub fn occupied_rect(&self) -> Option<OccupiedRect> {
        let mut min_x = TERM_COLS;
        let mut min_y = TERM_ROWS;
        let mut max_x = 0u16;
        let mut max_y = 0u16;
        let mut any = false;

        for y in 0..TERM_ROWS {
            for x in 0..TERM_COLS {
                let cell = self.cells[y as usize * TERM_COLS as usize + x as usize];
                let visible = cell.ch != ' ' || cell.fg[3] != 0 || cell.bg[3] != 0;
                if !visible {
                    continue;
                }
                any = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }

        any.then_some(OccupiedRect {
            x: min_x,
            y: min_y,
            width: max_x - min_x + 1,
            height: max_y - min_y + 1,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RendererUniforms {
    screen_size: [f32; 2],
    grid_size: [f32; 2],
    cell_size: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellInstance {
    uv_rect: [f32; 4],
    fg: [f32; 4],
    bg: [f32; 4],
}

pub struct TerminalRenderer {
    cell_w: u32,
    cell_h: u32,
    pixel_w: u32,
    pixel_h: u32,
    glyph_uvs: Vec<[f32; 4]>,
    glyph_map: HashMap<char, u16>,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    output_texture: wgpu::Texture,
    output_view: wgpu::TextureView,
}

impl TerminalRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self> {
        let font_size = BASE_FONT_SIZE * TERM_RENDER_SCALE as f32;
        let font = fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .map_err(anyhow::Error::msg)?;
        let metrics = font.metrics('M', font_size);
        let cell_w = metrics.advance_width.ceil() as u32;
        let line_metrics = font.horizontal_line_metrics(font_size).unwrap();
        let cell_h = line_metrics.new_line_size.ceil() as u32;
        let baseline = (-line_metrics.descent).ceil() as i32;

        let (glyph_map, glyph_uvs, atlas_texture, atlas_view) =
            build_glyph_atlas(device, queue, &font, font_size, cell_w, cell_h, baseline);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminal_uniforms"),
            size: std::mem::size_of::<RendererUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform = RendererUniforms {
            screen_size: [cell_w as f32 * TERM_COLS as f32, cell_h as f32 * TERM_ROWS as f32],
            grid_size: [TERM_COLS as f32, TERM_ROWS as f32],
            cell_size: [cell_w as f32, cell_h as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal_bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal_shader"),
            source: wgpu::ShaderSource::Wgsl(TERMINAL_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminal_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminal_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CellInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 32,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let instance_capacity = TERM_COLS as usize * TERM_ROWS as usize;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminal_instance_buffer"),
            size: (instance_capacity * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pixel_w = cell_w * TERM_COLS as u32;
        let pixel_h = cell_h * TERM_ROWS as u32;
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terminal_output"),
            size: wgpu::Extent3d {
                width: pixel_w,
                height: pixel_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            cell_w,
            cell_h,
            pixel_w,
            pixel_h,
            glyph_uvs,
            glyph_map,
            atlas_view,
            sampler,
            bind_group,
            pipeline,
            uniform_buffer,
            instance_buffer,
            instance_capacity,
            output_texture,
            output_view,
        })
    }

    pub fn pixel_width(&self) -> u32 {
        self.pixel_w
    }

    pub fn pixel_height(&self) -> u32 {
        self.pixel_h
    }

    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &TerminalGrid,
    ) {
        let mut instances = Vec::with_capacity(grid.cells().len());
        for cell in grid.cells() {
            let glyph_idx = self
                .glyph_map
                .get(&cell.ch)
                .copied()
                .unwrap_or_else(|| self.glyph_map.get(&'?').copied().unwrap_or(0));
            let uv_rect = self.glyph_uvs[glyph_idx as usize];
            instances.push(CellInstance {
                uv_rect,
                fg: rgba_to_f32(cell.fg),
                bg: rgba_to_f32(cell.bg),
            });
        }

        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("terminal_encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            pass.draw(0..6, 0..instances.len() as u32);
        }
        queue.submit(Some(encoder.finish()));
    }
}

fn rgba_to_f32(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

fn build_glyph_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &fontdue::Font,
    font_size: f32,
    cell_w: u32,
    cell_h: u32,
    baseline: i32,
) -> (HashMap<char, u16>, Vec<[f32; 4]>, wgpu::Texture, wgpu::TextureView) {
    let glyphs: Vec<char> = (32u8..=126u8).map(|c| c as char).collect();
    let cols = 16u32;
    let rows = (glyphs.len() as u32).div_ceil(cols);
    let atlas_w = cols * cell_w;
    let atlas_h = rows * cell_h;
    let mut atlas = vec![0u8; (atlas_w * atlas_h) as usize];
    let mut glyph_map = HashMap::new();
    let mut glyph_uvs = vec![[0.0; 4]; glyphs.len()];

    for (i, ch) in glyphs.iter().enumerate() {
        glyph_map.insert(*ch, i as u16);
        let tile_x = (i as u32 % cols) * cell_w;
        let tile_y = (i as u32 / cols) * cell_h;
        let uv_rect = [
            tile_x as f32 / atlas_w as f32,
            tile_y as f32 / atlas_h as f32,
            (tile_x + cell_w) as f32 / atlas_w as f32,
            (tile_y + cell_h) as f32 / atlas_h as f32,
        ];
        glyph_uvs[i] = uv_rect;

        if *ch == ' ' {
            continue;
        }

        let (metrics, bitmap) = font.rasterize(*ch, font_size);
        let origin_y = (cell_h as i32 - baseline) - metrics.height as i32 - metrics.ymin;
        let origin_x = metrics.xmin;

        for gy in 0..metrics.height {
            let sy = origin_y + gy as i32;
            if sy < 0 || sy >= cell_h as i32 {
                continue;
            }
            for gx in 0..metrics.width {
                let sx = origin_x + gx as i32;
                if sx < 0 || sx >= cell_w as i32 {
                    continue;
                }
                let dst_x = tile_x as i32 + sx;
                let dst_y = tile_y as i32 + sy;
                let dst = dst_y as usize * atlas_w as usize + dst_x as usize;
                atlas[dst] = bitmap[gy * metrics.width + gx];
            }
        }
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terminal_glyph_atlas"),
        size: wgpu::Extent3d {
            width: atlas_w,
            height: atlas_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas_w),
            rows_per_image: Some(atlas_h),
        },
        wgpu::Extent3d {
            width: atlas_w,
            height: atlas_h,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    (glyph_map, glyph_uvs, texture, view)
}

const TERMINAL_SHADER: &str = r#"
struct RendererUniforms {
    screen_size: vec2f,
    grid_size: vec2f,
    cell_size: vec2f,
    _pad: vec2f,
}

@group(0) @binding(0) var<uniform> u: RendererUniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) fg: vec4f,
    @location(2) bg: vec4f,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
    @location(0) uv_rect: vec4f,
    @location(1) fg: vec4f,
    @location(2) bg: vec4f,
) -> VsOut {
    var quad = array<vec2f, 6>(
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(0.0, 1.0),
        vec2f(0.0, 1.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
    );

    let cols = u32(u.grid_size.x);
    let cell_x = f32(instance_index % cols);
    let cell_y = f32(instance_index / cols);
    let local = quad[vertex_index];
    let px = vec2f(cell_x, cell_y) * u.cell_size + local * u.cell_size;
    let clip = vec2f(
        px.x / u.screen_size.x * 2.0 - 1.0,
        1.0 - px.y / u.screen_size.y * 2.0,
    );

    var out: VsOut;
    out.position = vec4f(clip, 0.0, 1.0);
    out.uv = mix(uv_rect.xy, uv_rect.zw, local);
    out.fg = fg;
    out.bg = bg;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let glyph = textureSampleLevel(atlas_tex, atlas_sampler, in.uv, 0.0).r * in.fg.a;
    if in.bg.a > 0.0 {
        let rgb = mix(in.bg.rgb, in.fg.rgb, glyph);
        return vec4f(rgb, 1.0);
    }
    if glyph <= 0.0 {
        return vec4f(0.0);
    }
    return vec4f(in.fg.rgb, glyph);
}
"#;
