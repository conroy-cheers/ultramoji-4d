use anyhow::{Result, anyhow};
use wgpu::util::DeviceExt;

use crate::texture::*;

#[derive(Clone, Copy, Debug, Default)]
pub struct SceneParams {
    pub rotation: Option<f32>,
    pub camera_pitch: Option<f32>,
    pub light_azimuth: Option<f32>,
    pub light_elevation: Option<f32>,
    pub light_distance: Option<f32>,
    pub ground_y: Option<f32>,
    pub bob: Option<f32>,
    pub fill: Option<f32>,
    pub bg_color: Option<[f32; 3]>,
    pub sharpen: Option<f32>,
    pub contrast: Option<f32>,
    pub dither: Option<f32>,
    pub vhs: Option<f32>,
    pub jitter: Option<f32>,
    pub supersample: bool,
    pub ssao_strength: Option<f32>,
    pub ssao_depth_threshold: Option<f32>,
    pub ssao_start_dist: Option<f32>,
    pub ssao_step_growth: Option<f32>,
    pub ssao_max_shadow: Option<f32>,
    pub show_depth: bool,
    pub render_scale: Option<f32>,
}

pub fn emoji_preview_scene_params() -> SceneParams {
    SceneParams {
        camera_pitch: Some(0.26),
        light_azimuth: Some(0.8),
        light_elevation: Some(0.96),
        light_distance: Some(4.8),
        ground_y: Some(-1.15),
        fill: Some(0.65),
        bg_color: Some([
            0x13 as f32 / 255.0,
            0x0f as f32 / 255.0,
            0x17 as f32 / 255.0,
        ]),
        ..SceneParams::default()
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    face_type: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    normal_rot: [[f32; 4]; 4],
    shadow_mvp: [[f32; 4]; 4],
    ground_mvp: [[f32; 4]; 4],
    light_dir: [f32; 4],
    bg_color: [f32; 4],
    camera_pos: [f32; 4],
    ground_y: f32,
    debug_flags: u32,
    near: f32,
    far: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SsaoUniforms {
    strength: f32,
    depth_threshold: f32,
    start_dist: f32,
    step_growth: f32,
    max_shadow: f32,
    jitter_spread: f32,
    object_bbox_min: [f32; 2],
    object_bbox_max: [f32; 2],
    _pad1: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PostprocessUniforms {
    contrast: f32,
    sharpen: f32,
    dither: f32,
    frame: f32,
    vhs: f32,
    _pp_pad: [f32; 3],
}

// Column-major 4x4 matrices: m[col][row], matching WGSL mat4x4f layout.

fn mat4_mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = a[0][row] * b[col][0]
                + a[1][row] * b[col][1]
                + a[2][row] * b[col][2]
                + a[3][row] * b[col][3];
        }
    }
    out
}

fn mat4_rotate_y(angle: f32) -> [[f32; 4]; 4] {
    let c = angle.cos();
    let s = angle.sin();
    [
        [c, 0.0, s, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [-s, 0.0, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat4_rotate_x(angle: f32) -> [[f32; 4]; 4] {
    let c = angle.cos();
    let s = angle.sin();
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, c, s, 0.0],
        [0.0, -s, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat4_translate(tx: f32, ty: f32, tz: f32) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [tx, ty, tz, 1.0],
    ]
}

fn mat4_scale(sx: f32, sy: f32, sz: f32) -> [[f32; 4]; 4] {
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, sz, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 12,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 24,
        shader_location: 2,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Uint32,
        offset: 32,
        shader_location: 3,
    },
];

fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

fn linear_to_srgb(v: u8) -> u8 {
    let c = v as f32 / 255.0;
    let s = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

fn halton(index: u32, base: u32) -> f32 {
    let mut f = 1.0f32;
    let mut r = 0.0f32;
    let mut i = index;
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

fn mat4_perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far * nf, -1.0],
        [0.0, 0.0, near * far * nf, 0.0],
    ]
}

fn screen_aabb_from_mvp(
    mvp: &[[f32; 4]; 4],
    half_h: f32,
    half_d: f32,
    screen_w: f32,
    screen_h: f32,
) -> ([f32; 2], [f32; 2]) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &sx in &[-1.0f32, 1.0] {
        for &sy in &[-half_h, half_h] {
            for &sz in &[-half_d, half_d] {
                let x = mvp[0][0] * sx + mvp[1][0] * sy + mvp[2][0] * sz + mvp[3][0];
                let y = mvp[0][1] * sx + mvp[1][1] * sy + mvp[2][1] * sz + mvp[3][1];
                let w = mvp[0][3] * sx + mvp[1][3] * sy + mvp[2][3] * sz + mvp[3][3];
                if w.abs() < 1e-6 {
                    continue;
                }
                let ndc_x = x / w;
                let ndc_y = y / w;
                let px = (ndc_x * 0.5 + 0.5) * screen_w;
                let py = (ndc_y * 0.5 + 0.5) * screen_h;
                min_x = min_x.min(px);
                min_y = min_y.min(py);
                max_x = max_x.max(px);
                max_y = max_y.max(py);
            }
        }
    }
    ([min_x, min_y], [max_x, max_y])
}

fn mat4_shadow_projection(light: [f32; 3], ground_y: f32) -> [[f32; 4]; 4] {
    // Plane: y = ground_y -> (0,1,0,-ground_y).
    // Point light L = (lx, ly, lz, 1).
    // M[r][c] = dot*I[r][c] - L[r]*plane[c], dot = ly - ground_y
    let [lx, ly, lz] = light;
    let g = ground_y;
    let dot = ly - g;
    [
        [dot, 0.0, 0.0, 0.0],
        [-lx, -g, -lz, -1.0],
        [0.0, 0.0, dot, 0.0],
        [lx * g, ly * g, lz * g, ly],
    ]
}

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_color_pipeline: Option<wgpu::RenderPipeline>,
    ground_pipeline: wgpu::RenderPipeline,
    ssao_pipeline: wgpu::RenderPipeline,
    ssao_bind_group_layout: wgpu::BindGroupLayout,
    downsample_pipeline: wgpu::RenderPipeline,
    downsample_bind_group_layout: wgpu::BindGroupLayout,
    postprocess_pipeline: wgpu::RenderPipeline,
    postprocess_bind_group_layout: wgpu::BindGroupLayout,
    postprocess_uniform_buffer: wgpu::Buffer,
    ssao_uniform_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    tex_bind_group_layout: wgpu::BindGroupLayout,
    edge_color_buffer: wgpu::Buffer,
    tex_state: Option<TexState>,
    render_target: Option<RenderTargetState>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    max_texture_dimension_2d: u32,
    cached_mesh_key: Option<(u32, u32, usize)>,
    line_pipeline: Option<wgpu::RenderPipeline>,
    show_wireframe: bool,
    show_all_white: bool,
    show_stencil_shadow: bool,
    linear_depth_format: wgpu::TextureFormat,
    cached_frames: Vec<FrameGpuState>,
    cached_frames_key: Option<u64>,
    active_frame_idx: Option<usize>,
    last_offscreen_stats: Option<OffscreenPerfStats>,
    render_target_generation: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OffscreenPerfStats {
    pub scene_width: u32,
    pub scene_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub pass_count: u32,
    pub draw_call_count: u32,
    pub has_downsample: bool,
}

struct TexState {
    gpu_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    tex_w: u32,
    tex_h: u32,
    data_ptr: usize,
}

struct FrameGpuState {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    tex_bind_group: wgpu::BindGroup,
    _gpu_texture: wgpu::Texture,
}

struct RenderTargetState {
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    linear_depth_view: wgpu::TextureView,
    ssao_output_texture: wgpu::Texture,
    ssao_output_view: wgpu::TextureView,
    downsample_output_texture: Option<wgpu::Texture>,
    downsample_output_view: Option<wgpu::TextureView>,
    downsample_bind_group: Option<wgpu::BindGroup>,
    postprocess_output_texture: wgpu::Texture,
    postprocess_output_view: wgpu::TextureView,
    postprocess_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    ssao_bind_group: wgpu::BindGroup,
    staging_buffer: wgpu::Buffer,
    scene_width: u32,
    scene_height: u32,
    output_width: u32,
    output_height: u32,
    padded_row_bytes: u32,
}

impl GpuRenderer {
    pub fn from_device_queue(
        device: wgpu::Device,
        queue: wgpu::Queue,
        features: wgpu::Features,
        linear_depth_format: wgpu::TextureFormat,
        independent_blend_supported: bool,
    ) -> Result<Self> {
        let max_texture_dimension_2d = device.limits().max_texture_dimension_2d;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("billboard_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("uniform_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let tex_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tex_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("billboard_pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, &tex_bind_group_layout],
            push_constant_ranges: &[],
        });

        let scene_color_targets: &[Option<wgpu::ColorTargetState>] = &[
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: if independent_blend_supported {
                    Some(wgpu::BlendState::ALPHA_BLENDING)
                } else {
                    None
                },
                write_mask: wgpu::ColorWrites::ALL,
            }),
            Some(wgpu::ColorTargetState {
                format: linear_depth_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("billboard_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_buffer_layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: scene_color_targets,
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let line_pipeline = if features.contains(wgpu::Features::POLYGON_MODE_LINE) {
            Some(
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("billboard_line_pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[vertex_buffer_layout()],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        targets: scene_color_targets,
                        compilation_options: Default::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        polygon_mode: wgpu::PolygonMode::Line,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth24PlusStencil8,
                        depth_write_enabled: false,
                        depth_compare: wgpu::CompareFunction::LessEqual,
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState {
                            constant: -1,
                            slope_scale: -1.0,
                            clamp: 0.0,
                        },
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                }),
            )
        } else {
            None
        };

        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow_pipeline_layout"),
                bind_group_layouts: &[&uniform_bind_group_layout],
                push_constant_ranges: &[],
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow_pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[vertex_buffer_layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_shadow"),
                targets: scene_color_targets,
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState {
                    front: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::Equal,
                        fail_op: wgpu::StencilOperation::Keep,
                        depth_fail_op: wgpu::StencilOperation::Keep,
                        pass_op: wgpu::StencilOperation::IncrementClamp,
                    },
                    back: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::Equal,
                        fail_op: wgpu::StencilOperation::Keep,
                        depth_fail_op: wgpu::StencilOperation::Keep,
                        pass_op: wgpu::StencilOperation::IncrementClamp,
                    },
                    read_mask: 0xff,
                    write_mask: 0xff,
                },
                bias: wgpu::DepthBiasState {
                    constant: -2,
                    slope_scale: -2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let shadow_color_pipeline = if independent_blend_supported {
            None
        } else {
            Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shadow_color_pipeline"),
                layout: Some(&shadow_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_shadow"),
                    buffers: &[vertex_buffer_layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_shadow_color"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24PlusStencil8,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: wgpu::StencilState {
                        front: wgpu::StencilFaceState {
                            compare: wgpu::CompareFunction::Equal,
                            fail_op: wgpu::StencilOperation::Keep,
                            depth_fail_op: wgpu::StencilOperation::Keep,
                            pass_op: wgpu::StencilOperation::IncrementClamp,
                        },
                        back: wgpu::StencilFaceState {
                            compare: wgpu::CompareFunction::Equal,
                            fail_op: wgpu::StencilOperation::Keep,
                            depth_fail_op: wgpu::StencilOperation::Keep,
                            pass_op: wgpu::StencilOperation::IncrementClamp,
                        },
                        read_mask: 0xff,
                        write_mask: 0xff,
                    },
                    bias: wgpu::DepthBiasState {
                        constant: -2,
                        slope_scale: -2.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            }))
        };

        let ground_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ground_pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_ground"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ground"),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: if independent_blend_supported {
                            Some(wgpu::BlendState::REPLACE)
                        } else {
                            None
                        },
                    write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: linear_depth_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let ssao_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ssao_bgl"),
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
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let ssao_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ssao_pipeline_layout"),
            bind_group_layouts: &[&ssao_bind_group_layout],
            push_constant_ranges: &[],
        });
        let ssao_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ssao_pipeline"),
            layout: Some(&ssao_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ssao"),
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

        let downsample_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("downsample_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    },
                    count: None,
                }],
            });
        let downsample_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("downsample_pipeline_layout"),
                bind_group_layouts: &[&downsample_bind_group_layout],
                push_constant_ranges: &[],
            });
        let downsample_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("downsample_pipeline"),
            layout: Some(&downsample_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_downsample"),
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

        let postprocess_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("postprocess_bgl"),
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
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        },
                        count: None,
                    },
                ],
            });
        let postprocess_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("postprocess_pipeline_layout"),
                bind_group_layouts: &[&postprocess_bind_group_layout],
                push_constant_ranges: &[],
            });
        let postprocess_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("postprocess_pipeline"),
            layout: Some(&postprocess_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_postprocess"),
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
        let postprocess_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("postprocess_uniforms"),
            size: std::mem::size_of::<PostprocessUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let ssao_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssao_uniforms"),
            size: std::mem::size_of::<SsaoUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform_bg"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let edge_color_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("edge_color"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (vertices, indices) = billboard_geometry_rect(1.0, 0.1, true);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let num_indices = indices.len() as u32;
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            shadow_pipeline,
            shadow_color_pipeline,
            ground_pipeline,
            ssao_pipeline,
            ssao_bind_group_layout,
            downsample_pipeline,
            downsample_bind_group_layout,
            postprocess_pipeline,
            postprocess_bind_group_layout,
            postprocess_uniform_buffer,
            ssao_uniform_buffer,
            uniform_buffer,
            uniform_bind_group,
            tex_bind_group_layout,
            edge_color_buffer,
            tex_state: None,
            render_target: None,
            vertex_buffer,
            index_buffer,
            num_indices,
            max_texture_dimension_2d,
            cached_mesh_key: None,
            line_pipeline,
            show_wireframe: false,
            show_all_white: false,
            show_stencil_shadow: true,
            linear_depth_format,
            cached_frames: Vec::new(),
            cached_frames_key: None,
            active_frame_idx: None,
            last_offscreen_stats: None,
            render_target_generation: 0,
        })
    }

    pub fn load_frames(
        &mut self,
        cache_key: u64,
        frames: &[Vec<[u8; 4]>],
        width: u32,
        height: u32,
    ) {
        let key = cache_key;
        if self.cached_frames_key == Some(key) && self.cached_frames.len() == frames.len() {
            return;
        }

        self.cached_frames.clear();
        for pixels in frames {
            let tex = Texture {
                pixels,
                width,
                height,
            };
            let (vertices, indices) = extruded_billboard_geometry(&tex, 0.1, true);

            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("frame_vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("frame_indices"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
            let num_indices = indices.len() as u32;

            let mut rgba_data = Vec::with_capacity(pixels.len() * 4);
            for p in pixels.iter() {
                rgba_data.push(p[0]);
                rgba_data.push(p[1]);
                rgba_data.push(p[2]);
                rgba_data.push(255);
            }

            let w = width.max(1);
            let h = height.max(1);
            let gpu_texture = self.device.create_texture_with_data(
                &self.queue,
                &wgpu::TextureDescriptor {
                    label: Some("frame_tex"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba_data,
            );

            let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            let edge = tex.edge_color();
            let edge_data: [f32; 4] = [
                edge[0] as f32 / 255.0,
                edge[1] as f32 / 255.0,
                edge[2] as f32 / 255.0,
                1.0,
            ];
            let edge_buf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("frame_edge_color"),
                    contents: bytemuck::bytes_of(&edge_data),
                    usage: wgpu::BufferUsages::UNIFORM,
                });

            let tex_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("frame_tex_bg"),
                layout: &self.tex_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: edge_buf.as_entire_binding(),
                    },
                ],
            });

            self.cached_frames.push(FrameGpuState {
                vertex_buffer,
                index_buffer,
                num_indices,
                tex_bind_group,
                _gpu_texture: gpu_texture,
            });
        }
        self.cached_frames_key = Some(key);
    }

    pub fn render_animated_frame_to_offscreen_params(
        &mut self,
        cache_key: u64,
        frames: &[Vec<[u8; 4]>],
        frame_idx: usize,
        tex_w: u32,
        tex_h: u32,
        px_w: u32,
        px_h: u32,
        time_secs: f64,
        params: &SceneParams,
    ) -> anyhow::Result<()> {
        self.load_frames(cache_key, frames, tex_w, tex_h);
        if frame_idx >= self.cached_frames.len()
            || px_w == 0
            || px_h == 0
            || tex_w == 0
            || tex_h == 0
        {
            return Ok(());
        }

        self.active_frame_idx = Some(frame_idx);
        self.ensure_render_target(px_w, px_h, params.supersample);

        let tex_aspect = tex_w as f32 / tex_h as f32;
        let result = self.render_scene(tex_aspect, px_w, px_h, time_secs, params);
        self.active_frame_idx = None;
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn readback_offscreen_animated(
        &mut self,
        frame_idx: usize,
        tex_w: u32,
        tex_h: u32,
        px_width: usize,
        px_height: usize,
        time_secs: f64,
        params: &SceneParams,
    ) -> Vec<(u8, u8, u8)> {
        if frame_idx >= self.cached_frames.len() {
            return vec![];
        }
        let px_w = px_width as u32;
        let px_h = px_height as u32;
        if px_w == 0 || px_h == 0 || tex_w == 0 || tex_h == 0 {
            return vec![];
        }

        self.active_frame_idx = Some(frame_idx);
        self.ensure_render_target(px_w, px_h, params.supersample);

        let tex_aspect = tex_w as f32 / tex_h as f32;
        if self
            .render_scene(tex_aspect, px_w, px_h, time_secs, params)
            .is_err()
        {
            self.active_frame_idx = None;
            return vec![];
        }

        let result = self.readback_pixels(px_w, px_h);
        self.active_frame_idx = None;
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_billboard_rgb(
        &mut self,
        texture: &Texture,
        px_width: usize,
        px_height: usize,
        time_secs: f64,
    ) -> Vec<(u8, u8, u8)> {
        self.readback_offscreen_rgb(
            texture,
            px_width,
            px_height,
            time_secs,
            &SceneParams::default(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn readback_offscreen_rgb(
        &mut self,
        texture: &Texture,
        px_width: usize,
        px_height: usize,
        time_secs: f64,
        params: &SceneParams,
    ) -> Vec<(u8, u8, u8)> {
        let px_w = px_width as u32;
        let px_h = px_height as u32;

        if px_w == 0 || px_h == 0 || texture.width == 0 || texture.height == 0 {
            return vec![];
        }

        if self
            .render_to_offscreen_params(texture, px_w, px_h, time_secs, params)
            .is_err()
        {
            return vec![];
        }

        self.readback_pixels(px_w, px_h)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn readback_pixels(&mut self, px_w: u32, px_h: u32) -> Vec<(u8, u8, u8)> {
        let rt = self.render_target.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &rt.postprocess_output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &rt.staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(rt.padded_row_bytes),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: px_w,
                height: px_h,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));

        let rt = self.render_target.as_ref().unwrap();
        let buffer_slice = rt.staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);

        if rx.recv().ok().and_then(|r| r.ok()).is_none() {
            return vec![];
        }

        let data = buffer_slice.get_mapped_range();
        let row_bytes = px_w as usize * 4;
        let padded = rt.padded_row_bytes as usize;

        let mut fb = vec![(0u8, 0u8, 0u8); (px_w * px_h) as usize];
        for y in 0..px_h as usize {
            let src_y = px_h as usize - 1 - y;
            let row_start = src_y * padded;
            let row = &data[row_start..row_start + row_bytes];
            for x in 0..px_w as usize {
                let i = x * 4;
                fb[y * px_w as usize + x] = (
                    linear_to_srgb(row[i]),
                    linear_to_srgb(row[i + 1]),
                    linear_to_srgb(row[i + 2]),
                );
            }
        }

        drop(data);
        rt.staging_buffer.unmap();
        fb
    }

    pub fn render_to_offscreen_params(
        &mut self,
        texture: &Texture,
        px_w: u32,
        px_h: u32,
        time_secs: f64,
        params: &SceneParams,
    ) -> Result<()> {
        let mesh_key = (
            texture.width,
            texture.height,
            texture.pixels.as_ptr() as usize,
        );
        if self.cached_mesh_key != Some(mesh_key) {
            self.update_geometry(texture);
            self.cached_mesh_key = Some(mesh_key);
        }

        let tex_aspect = texture.width as f32 / texture.height as f32;
        self.ensure_texture(texture);

        let scale = params
            .render_scale
            .unwrap_or(if params.supersample { 2.0 } else { 1.0 });
        let scaled_w = ((px_w as f32 * scale) as u32).max(1);
        let scaled_h = ((px_h as f32 * scale) as u32).max(1);
        self.ensure_render_target_scaled(px_w, px_h, scaled_w, scaled_h);

        if self.tex_state.is_none() {
            return Err(anyhow!("emoji preview texture state unavailable"));
        }

        self.render_scene(tex_aspect, px_w, px_h, time_secs, params)
    }

    fn render_scene(
        &mut self,
        tex_aspect: f32,
        px_w: u32,
        px_h: u32,
        time_secs: f64,
        params: &SceneParams,
    ) -> Result<()> {
        let vp_aspect = px_w as f32 / px_h as f32;
        let fill = params.fill.unwrap_or(0.65);

        let spin = params.rotation.unwrap_or_else(|| {
            let phase = time_secs * 0.8;
            (phase - 0.6 * phase.sin()) as f32
        });
        let pitch = params.camera_pitch.unwrap_or(0.26);
        let bob = params
            .bob
            .unwrap_or_else(|| (time_secs * 0.7).sin() as f32 * 0.06);

        let light_az = params.light_azimuth.unwrap_or(0.8);
        let light_el = params.light_elevation.unwrap_or(0.96);
        let light_dist = params.light_distance.unwrap_or(4.8);
        let ground_y = params.ground_y.unwrap_or(-1.15);

        let (cos_az, sin_az) = (light_az.cos(), light_az.sin());
        let (cos_el, sin_el) = (light_el.cos(), light_el.sin());
        let light_pos = [
            cos_az * cos_el * light_dist,
            sin_el * light_dist,
            sin_az * cos_el * light_dist,
        ];
        let light_dir = [cos_az * cos_el, sin_el, sin_az * cos_el, 0.0];

        let billboard_h = 1.0 / tex_aspect;
        let fov_y = 0.6f32;
        let cam_dist = billboard_h / (fill * (fov_y * 0.5).tan());
        let near = 0.1f32;
        let far = cam_dist + 600.0;

        let mut proj = mat4_mul(
            &mat4_scale(1.0, -1.0, 1.0),
            &mat4_perspective(fov_y, vp_aspect, near, far),
        );
        if let Some(jitter_amp) = params.jitter {
            let rt = self.render_target.as_ref().unwrap();
            let sw = rt.scene_width;
            let sh = rt.scene_height;
            if sw > 0 && sh > 0 {
                let idx = ((time_secs * 60.0) as u32 % 16) + 1;
                let jx = (halton(idx, 2) - 0.5) * jitter_amp;
                let jy = (halton(idx, 3) - 0.5) * jitter_amp;
                proj[3][0] += jx * 2.0 / sw as f32;
                proj[3][1] += jy * 2.0 / sh as f32;
            }
        }
        let view = mat4_mul(&mat4_translate(0.0, 0.0, -cam_dist), &mat4_rotate_x(pitch));
        let view_proj = mat4_mul(&proj, &view);

        let model_rot = mat4_rotate_y(spin);
        let model = mat4_mul(&mat4_translate(0.0, bob, 0.0), &model_rot);
        let mvp = mat4_mul(&view_proj, &model);

        let shadow_model = mat4_mul(&mat4_shadow_projection(light_pos, ground_y), &model);
        let shadow_mvp = mat4_mul(&view_proj, &shadow_model);

        let ground_mvp = view_proj;

        let bg = params.bg_color.unwrap_or([
            0x13 as f32 / 255.0,
            0x0f as f32 / 255.0,
            0x17 as f32 / 255.0,
        ]);

        let camera_pos = [0.0, cam_dist * pitch.sin(), cam_dist * pitch.cos(), 1.0];

        let uniforms = Uniforms {
            mvp,
            normal_rot: model_rot,
            shadow_mvp,
            ground_mvp,
            light_dir,
            bg_color: [bg[0], bg[1], bg[2], 1.0],
            camera_pos,
            ground_y,
            debug_flags: (self.show_all_white as u32) | ((params.show_depth as u32) << 1),
            near,
            far,
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let rt = self.render_target.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene_pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &rt.color_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: bg[0] as f64,
                                g: bg[1] as f64,
                                b: bg[2] as f64,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &rt.linear_depth_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 1.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &rt.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                ..Default::default()
            });

            pass.set_pipeline(&self.ground_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.draw(0..6, 0..1);

            let (vb, ib, n_idx, tex_bg) = if let Some(fi) = self.active_frame_idx {
                let f = &self.cached_frames[fi];
                (
                    &f.vertex_buffer,
                    &f.index_buffer,
                    f.num_indices,
                    &f.tex_bind_group,
                )
            } else {
                (
                    &self.vertex_buffer,
                    &self.index_buffer,
                    self.num_indices,
                    &self.tex_state.as_ref().unwrap().bind_group,
                )
            };

            if n_idx > 0 {
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);

                if self.show_stencil_shadow && self.shadow_color_pipeline.is_none() {
                    pass.set_pipeline(&self.shadow_pipeline);
                    pass.draw_indexed(0..n_idx, 0, 0..1);
                }

                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(1, tex_bg, &[]);
                pass.draw_indexed(0..n_idx, 0, 0..1);

                if self.show_wireframe {
                    if let Some(line_pipeline) = &self.line_pipeline {
                        pass.set_pipeline(line_pipeline);
                        pass.draw_indexed(0..n_idx, 0, 0..1);
                    }
                }
            }
        }

        if self.show_stencil_shadow && self.shadow_color_pipeline.is_some() {
            let (vb, ib, n_idx) = if let Some(fi) = self.active_frame_idx {
                let f = &self.cached_frames[fi];
                (&f.vertex_buffer, &f.index_buffer, f.num_indices)
            } else {
                (&self.vertex_buffer, &self.index_buffer, self.num_indices)
            };

            if n_idx > 0 {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow_color_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &rt.color_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &rt.depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                    }),
                    ..Default::default()
                });
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_pipeline(self.shadow_color_pipeline.as_ref().unwrap());
                pass.draw_indexed(0..n_idx, 0, 0..1);
            }
        }

        let scene_w = rt.scene_width as f32;
        let scene_h = rt.scene_height as f32;
        let (bbox_min, bbox_max) = screen_aabb_from_mvp(&mvp, billboard_h, 0.1, scene_w, scene_h);
        let ref_height = 720.0f32;
        let res_scale = scene_h / ref_height;
        let ssao_uniforms = SsaoUniforms {
            strength: params.ssao_strength.unwrap_or(10.0),
            depth_threshold: params.ssao_depth_threshold.unwrap_or(0.0),
            start_dist: params.ssao_start_dist.unwrap_or(0.1) * res_scale,
            step_growth: params.ssao_step_growth.unwrap_or(1.20),
            max_shadow: params.ssao_max_shadow.unwrap_or(0.4),
            jitter_spread: 0.35,
            object_bbox_min: bbox_min,
            object_bbox_max: bbox_max,
            _pad1: [0.0; 2],
        };
        self.queue.write_buffer(
            &self.ssao_uniform_buffer,
            0,
            bytemuck::bytes_of(&ssao_uniforms),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ssao_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt.ssao_output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.ssao_pipeline);
            pass.set_bind_group(0, &rt.ssao_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        if let (Some(ds_view), Some(ds_bg)) =
            (&rt.downsample_output_view, &rt.downsample_bind_group)
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("downsample_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: ds_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.downsample_pipeline);
            pass.set_bind_group(0, ds_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        let pp_uniforms = PostprocessUniforms {
            contrast: params.contrast.unwrap_or(1.15),
            sharpen: params.sharpen.unwrap_or(0.0),
            dither: params.dither.unwrap_or(0.0),
            frame: (time_secs * 60.0) as f32,
            vhs: params.vhs.unwrap_or(0.0),
            _pp_pad: [0.0; 3],
        };
        self.queue.write_buffer(
            &self.postprocess_uniform_buffer,
            0,
            bytemuck::bytes_of(&pp_uniforms),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("postprocess_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt.postprocess_output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.postprocess_pipeline);
            pass.set_bind_group(0, &rt.postprocess_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let mut draw_call_count = 4u32;
        if self.show_stencil_shadow {
            draw_call_count += 1;
        }
        if self.show_wireframe && self.line_pipeline.is_some() {
            draw_call_count += 1;
        }
        let has_downsample = rt.downsample_output_view.is_some();
        if has_downsample {
            draw_call_count += 1;
        }
        self.last_offscreen_stats = Some(OffscreenPerfStats {
            scene_width: rt.scene_width,
            scene_height: rt.scene_height,
            output_width: rt.output_width,
            output_height: rt.output_height,
            pass_count: if has_downsample {
                3 + has_downsample as u32 + self.show_stencil_shadow as u32
            } else {
                3 + self.show_stencil_shadow as u32
            },
            draw_call_count,
            has_downsample,
        });

        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn max_texture_dimension_2d(&self) -> u32 {
        self.max_texture_dimension_2d
    }

    pub fn offscreen_view(&self) -> Option<&wgpu::TextureView> {
        self.render_target
            .as_ref()
            .map(|rt| &rt.postprocess_output_view)
    }

    pub fn scene_view(&self) -> Option<&wgpu::TextureView> {
        self.render_target.as_ref().map(|rt| {
            rt.downsample_output_view
                .as_ref()
                .unwrap_or(&rt.ssao_output_view)
        })
    }

    pub fn offscreen_width(&self) -> Option<u32> {
        self.render_target.as_ref().map(|rt| rt.output_width)
    }

    pub fn offscreen_height(&self) -> Option<u32> {
        self.render_target.as_ref().map(|rt| rt.output_height)
    }

    pub fn offscreen_perf_stats(&self) -> Option<OffscreenPerfStats> {
        self.last_offscreen_stats
    }

    pub fn render_target_generation(&self) -> u64 {
        self.render_target_generation
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn set_wireframe(&mut self, enabled: bool) {
        self.show_wireframe = enabled;
    }

    pub fn wireframe(&self) -> bool {
        self.show_wireframe
    }

    pub fn wireframe_supported(&self) -> bool {
        self.line_pipeline.is_some()
    }

    pub fn set_all_white(&mut self, enabled: bool) {
        self.show_all_white = enabled;
    }

    pub fn all_white(&self) -> bool {
        self.show_all_white
    }

    pub fn set_stencil_shadow(&mut self, enabled: bool) {
        self.show_stencil_shadow = enabled;
    }

    pub fn stencil_shadow(&self) -> bool {
        self.show_stencil_shadow
    }

    pub fn write_to_postprocess_output(&mut self, rgba: &[u8], width: u32, height: u32) {
        self.ensure_render_target(width, height, false);
        let rt = self.render_target.as_ref().unwrap();
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &rt.postprocess_output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn update_geometry(&mut self, texture: &Texture) {
        let (vertices, indices) = extruded_billboard_geometry(texture, 0.1, true);
        self.vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
        self.index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.num_indices = indices.len() as u32;
    }

    fn ensure_texture(&mut self, texture: &Texture) {
        let data_ptr = texture.pixels.as_ptr() as usize;
        if self.tex_state.as_ref().is_some_and(|ts| {
            ts.data_ptr == data_ptr && ts.tex_w == texture.width && ts.tex_h == texture.height
        }) {
            return;
        }

        let mut rgba_data: Vec<u8> = Vec::with_capacity(texture.pixels.len() * 4);
        for p in texture.pixels.iter() {
            rgba_data.push(p[0]);
            rgba_data.push(p[1]);
            rgba_data.push(p[2]);
            rgba_data.push(255);
        }

        let same_size = self
            .tex_state
            .as_ref()
            .is_some_and(|ts| ts.tex_w == texture.width && ts.tex_h == texture.height);

        if same_size {
            let ts = self.tex_state.as_mut().unwrap();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &ts.gpu_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba_data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(texture.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: texture.width,
                    height: texture.height,
                    depth_or_array_layers: 1,
                },
            );
            ts.data_ptr = data_ptr;
            return;
        }

        let w = texture.width.max(1);
        let h = texture.height.max(1);

        let gpu_texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("emoji_tex"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &rgba_data,
        );

        let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let edge = texture.edge_color();
        let edge_data: [f32; 4] = [
            edge[0] as f32 / 255.0,
            edge[1] as f32 / 255.0,
            edge[2] as f32 / 255.0,
            1.0,
        ];
        self.queue
            .write_buffer(&self.edge_color_buffer, 0, bytemuck::bytes_of(&edge_data));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tex_bg"),
            layout: &self.tex_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.edge_color_buffer.as_entire_binding(),
                },
            ],
        });

        self.tex_state = Some(TexState {
            gpu_texture,
            bind_group,
            tex_w: texture.width,
            tex_h: texture.height,
            data_ptr,
        });
    }

    pub fn ensure_render_target(&mut self, width: u32, height: u32, supersample: bool) {
        let scene_w = if supersample { width * 2 } else { width };
        let scene_h = if supersample { height * 2 } else { height };
        self.ensure_render_target_scaled(width, height, scene_w, scene_h);
    }

    pub fn ensure_render_target_scaled(
        &mut self,
        width: u32,
        height: u32,
        scene_w: u32,
        scene_h: u32,
    ) {
        let needs_update = match &self.render_target {
            Some(rt) => {
                rt.output_width != width
                    || rt.output_height != height
                    || rt.scene_width != scene_w
                    || rt.scene_height != scene_h
            }
            None => true,
        };
        if !needs_update {
            return;
        }

        let color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rt_color"),
            size: wgpu::Extent3d {
                width: scene_w,
                height: scene_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let ssao_output_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rt_ssao_output"),
            size: wgpu::Extent3d {
                width: scene_w,
                height: scene_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ssao_output_view =
            ssao_output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let linear_depth_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rt_linear_depth"),
            size: wgpu::Extent3d {
                width: scene_w,
                height: scene_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.linear_depth_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let linear_depth_view =
            linear_depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let depth_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rt_depth"),
            size: wgpu::Extent3d {
                width: scene_w,
                height: scene_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let ssao_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ssao_bg"),
            layout: &self.ssao_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&linear_depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.ssao_uniform_buffer.as_entire_binding(),
                },
            ],
        });

        // Downsample pass (only when scene is larger than output)
        let needs_downsample = scene_w > width || scene_h > height;
        let (downsample_output_texture, downsample_output_view, downsample_bind_group) =
            if needs_downsample {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("rt_downsample_output"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("downsample_bg"),
                    layout: &self.downsample_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&ssao_output_view),
                    }],
                });
                (Some(tex), Some(view), Some(bg))
            } else {
                (None, None, None)
            };

        let pp_input_view = if needs_downsample {
            downsample_output_view.as_ref().unwrap()
        } else {
            &ssao_output_view
        };

        let postprocess_output_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rt_postprocess_output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let postprocess_output_view =
            postprocess_output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let postprocess_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("postprocess_bg"),
            layout: &self.postprocess_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.postprocess_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(pp_input_view),
                },
            ],
        });

        let row_bytes = width * 4;
        let padded_row_bytes = (row_bytes + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
            / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: padded_row_bytes as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        self.render_target = Some(RenderTargetState {
            color_texture,
            color_view,
            linear_depth_view,
            ssao_output_texture,
            ssao_output_view,
            downsample_output_texture,
            downsample_output_view,
            downsample_bind_group,
            postprocess_output_texture,
            postprocess_output_view,
            postprocess_bind_group,
            depth_view,
            ssao_bind_group,
            staging_buffer,
            scene_width: scene_w,
            scene_height: scene_h,
            output_width: width,
            output_height: height,
            padded_row_bytes,
        });
        self.render_target_generation = self.render_target_generation.wrapping_add(1);
    }

    pub fn start_offscreen_readback(
        &mut self,
    ) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        let rt = self.render_target.as_ref()?;
        let px_w = rt.output_width;
        let px_h = rt.output_height;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &rt.postprocess_output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &rt.staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(rt.padded_row_bytes),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: px_w,
                height: px_h,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));

        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready_clone = ready.clone();
        rt.staging_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |_| {
                ready_clone.store(true, std::sync::atomic::Ordering::Release);
            });
        Some(ready)
    }

    pub fn finish_offscreen_readback(&mut self) -> Option<Vec<(u8, u8, u8)>> {
        let rt = self.render_target.as_ref()?;
        let px_w = rt.output_width;
        let px_h = rt.output_height;

        let buffer_slice = rt.staging_buffer.slice(..);
        let data = buffer_slice.get_mapped_range();

        let row_bytes = px_w as usize * 4;
        let padded = rt.padded_row_bytes as usize;

        let mut fb = vec![(0u8, 0u8, 0u8); (px_w * px_h) as usize];
        for y in 0..px_h as usize {
            let src_y = px_h as usize - 1 - y;
            let row_start = src_y * padded;
            let row = &data[row_start..row_start + row_bytes];
            for x in 0..px_w as usize {
                let i = x * 4;
                fb[y * px_w as usize + x] = (
                    linear_to_srgb(row[i]),
                    linear_to_srgb(row[i + 1]),
                    linear_to_srgb(row[i + 2]),
                );
            }
        }

        drop(data);
        rt.staging_buffer.unmap();
        Some(fb)
    }
}

fn billboard_geometry_rect(
    aspect: f32,
    depth_ratio: f32,
    mirror_back_face: bool,
) -> (Vec<Vertex>, Vec<u16>) {
    let hw = 1.0f32;
    let hh = 1.0 / aspect;
    let hd = hw * depth_ratio;

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);

    let mut quad =
        |positions: [[f32; 3]; 4], normal: [f32; 3], uvs: [[f32; 2]; 4], face_type: u32| {
            let base = vertices.len() as u16;
            for i in 0..4 {
                vertices.push(Vertex {
                    position: positions[i],
                    normal,
                    uv: uvs[i],
                    face_type,
                    _pad: 0,
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        };

    // Front face (z = +hd)
    quad(
        [[-hw, -hh, hd], [hw, -hh, hd], [hw, hh, hd], [-hw, hh, hd]],
        [0.0, 0.0, 1.0],
        [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        0,
    );

    // Back face (z = -hd), mirrored so the rear reads like the true back.
    quad(
        [
            [hw, -hh, -hd],
            [-hw, -hh, -hd],
            [-hw, hh, -hd],
            [hw, hh, -hd],
        ],
        [0.0, 0.0, -1.0],
        if mirror_back_face {
            [[1.0, 1.0], [0.0, 1.0], [0.0, 0.0], [1.0, 0.0]]
        } else {
            [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]
        },
        1,
    );

    // Right face (x = +hw)
    quad(
        [[hw, -hh, hd], [hw, -hh, -hd], [hw, hh, -hd], [hw, hh, hd]],
        [1.0, 0.0, 0.0],
        [[1.0, 1.0], [1.0, 1.0], [1.0, 0.0], [1.0, 0.0]],
        2,
    );

    // Left face (x = -hw)
    quad(
        [
            [-hw, -hh, -hd],
            [-hw, -hh, hd],
            [-hw, hh, hd],
            [-hw, hh, -hd],
        ],
        [-1.0, 0.0, 0.0],
        [[0.0, 1.0], [0.0, 1.0], [0.0, 0.0], [0.0, 0.0]],
        3,
    );

    // Top face (y = +hh)
    quad(
        [[-hw, hh, hd], [hw, hh, hd], [hw, hh, -hd], [-hw, hh, -hd]],
        [0.0, 1.0, 0.0],
        [[0.0, 0.0], [1.0, 0.0], [1.0, 0.0], [0.0, 0.0]],
        4,
    );

    // Bottom face (y = -hh)
    quad(
        [
            [-hw, -hh, -hd],
            [hw, -hh, -hd],
            [hw, -hh, hd],
            [-hw, -hh, hd],
        ],
        [0.0, -1.0, 0.0],
        [[0.0, 1.0], [1.0, 1.0], [1.0, 1.0], [0.0, 1.0]],
        5,
    );

    (vertices, indices)
}

fn extruded_billboard_geometry(
    texture: &Texture,
    depth_ratio: f32,
    _mirror_back_face: bool,
) -> (Vec<Vertex>, Vec<u16>) {
    let aspect = if texture.height > 0 {
        texture.width as f32 / texture.height as f32
    } else {
        1.0
    };

    if texture.width == 0 || texture.height == 0 {
        return billboard_geometry_rect(aspect, depth_ratio, true);
    }

    let has_opaque = texture.pixels.iter().any(|p| p[3] >= 160);
    if !has_opaque {
        return (Vec::new(), Vec::new());
    }

    let hw = 1.0f32;
    let hh = 1.0 / aspect.max(0.0001);
    let hd = hw * depth_ratio;

    let max_cells = 256usize;
    let (grid_w, grid_h) = if texture.width >= texture.height {
        let gh = ((texture.height as f32 / texture.width as f32) * max_cells as f32)
            .round()
            .clamp(1.0, max_cells as f32) as usize;
        (max_cells, gh)
    } else {
        let gw = ((texture.width as f32 / texture.height as f32) * max_cells as f32)
            .round()
            .clamp(1.0, max_cells as f32) as usize;
        (gw, max_cells)
    };

    let cols = grid_w + 1;
    let rows = grid_h + 1;
    let field = alpha_field(texture, cols, rows);

    let threshold = 160.0 / 255.0;
    let field_f64: Vec<f64> = field.iter().map(|&v| v as f64).collect();

    let builder = contour::ContourBuilder::new(cols, rows, true)
        .x_step(1.0 / grid_w as f64)
        .y_step(1.0 / grid_h as f64);

    let contours = match builder.contours(&field_f64, &[threshold]) {
        Ok(c) => c,
        Err(_) => return billboard_geometry_rect(aspect, depth_ratio, true),
    };

    if contours.is_empty() {
        return billboard_geometry_rect(aspect, depth_ratio, true);
    }

    let multi_polygon = contours.into_iter().next().unwrap().into_inner().0;
    if multi_polygon.0.is_empty() {
        return billboard_geometry_rect(aspect, depth_ratio, true);
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let texel_u = 0.5 / texture.width.max(1) as f32;
    let texel_v = 0.5 / texture.height.max(1) as f32;

    for polygon in &multi_polygon.0 {
        let exterior = polygon.exterior();
        let ext_coords = exterior.coords().collect::<Vec<_>>();
        if ext_coords.len() < 4 {
            continue;
        }

        // Build flat coordinate array for earcutr: exterior + holes
        let mut flat_coords: Vec<f64> = Vec::new();
        let mut hole_indices: Vec<usize> = Vec::new();

        // Exterior ring (skip the closing duplicate point)
        let ext_len = ext_coords.len() - 1;
        for &coord in &ext_coords[..ext_len] {
            flat_coords.push(coord.x);
            flat_coords.push(coord.y);
        }

        // Interior rings (holes)
        for interior in polygon.interiors() {
            let hole_coords: Vec<_> = interior.coords().collect();
            if hole_coords.len() < 4 {
                continue;
            }
            hole_indices.push(flat_coords.len() / 2);
            let hole_len = hole_coords.len() - 1;
            for &coord in &hole_coords[..hole_len] {
                flat_coords.push(coord.x);
                flat_coords.push(coord.y);
            }
        }

        let n_verts = flat_coords.len() / 2;
        if n_verts < 3 {
            continue;
        }

        let tri_indices = match earcutr::earcut(&flat_coords, &hole_indices, 2) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Build UV and position arrays from flat coords
        // contour crate outputs coordinates in [0, 1] UV space (due to x_step/y_step)
        let uv_points: Vec<[f32; 2]> = (0..n_verts)
            .map(|i| [flat_coords[i * 2] as f32, flat_coords[i * 2 + 1] as f32])
            .collect();

        let pos_points: Vec<[f32; 2]> = uv_points
            .iter()
            .map(|&[u, v]| [-hw + u * 2.0 * hw, hh - v * 2.0 * hh])
            .collect();

        // Front cap (z = +hd, face_type = 0)
        emit_cap(
            &mut vertices,
            &mut indices,
            &uv_points,
            &pos_points,
            &tri_indices,
            texel_u,
            texel_v,
            hd,
            false,
            false,
        );

        // Back cap (z = -hd, face_type = 1, flipped winding)
        // No UV mirror -- the Y-axis rotation already mirrors the view naturally
        emit_cap(
            &mut vertices,
            &mut indices,
            &uv_points,
            &pos_points,
            &tri_indices,
            texel_u,
            texel_v,
            -hd,
            true,
            false,
        );

        // Side walls along exterior ring
        emit_side_walls(
            &mut vertices,
            &mut indices,
            &uv_points[..ext_len],
            &pos_points[..ext_len],
            texel_u,
            texel_v,
            hd,
        );

        // Side walls along each hole (wound opposite direction for inward-facing normals)
        let mut hole_start = ext_len;
        for interior in polygon.interiors() {
            let hole_coords: Vec<_> = interior.coords().collect();
            if hole_coords.len() < 4 {
                continue;
            }
            let hole_len = hole_coords.len() - 1;
            let hole_end = hole_start + hole_len;
            emit_side_walls(
                &mut vertices,
                &mut indices,
                &uv_points[hole_start..hole_end],
                &pos_points[hole_start..hole_end],
                texel_u,
                texel_v,
                hd,
            );
            hole_start = hole_end;
        }
    }

    if indices.is_empty() {
        billboard_geometry_rect(aspect, depth_ratio, true)
    } else {
        (vertices, indices)
    }
}

fn push_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u16>,
    positions: [[f32; 3]; 4],
    normal: [f32; 3],
    uvs: [[f32; 2]; 4],
    face_type: u32,
) {
    let base = vertices.len() as u16;
    for i in 0..4 {
        vertices.push(Vertex {
            position: positions[i],
            normal,
            uv: uvs[i],
            face_type,
            _pad: 0,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn emit_cap(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u16>,
    uv_points: &[[f32; 2]],
    pos_points: &[[f32; 2]],
    tri_indices: &[usize],
    texel_u: f32,
    texel_v: f32,
    z: f32,
    flip_winding: bool,
    mirror_u: bool,
) {
    let base = vertices.len() as u16;
    let face_type = if z >= 0.0 { 0u32 } else { 1 };
    let nz = if z >= 0.0 { 1.0f32 } else { -1.0 };

    for (&[u, v], &[px, py]) in uv_points.iter().zip(pos_points.iter()) {
        let u_final = if mirror_u { 1.0 - u } else { u }.clamp(texel_u, 1.0 - texel_u);
        let v_final = v.clamp(texel_v, 1.0 - texel_v);
        vertices.push(Vertex {
            position: [px, py, z],
            normal: [0.0, 0.0, nz],
            uv: [u_final, v_final],
            face_type,
            _pad: 0,
        });
    }

    for tri in tri_indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as u16, tri[1] as u16, tri[2] as u16);
        if flip_winding {
            indices.extend_from_slice(&[base + a, base + c, base + b]);
        } else {
            indices.extend_from_slice(&[base + a, base + b, base + c]);
        }
    }
}

fn emit_side_walls(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u16>,
    uv_ring: &[[f32; 2]],
    pos_ring: &[[f32; 2]],
    texel_u: f32,
    texel_v: f32,
    hd: f32,
) {
    let n = uv_ring.len();
    if n < 2 {
        return;
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let [au, av] = uv_ring[i];
        let [bu, bv] = uv_ring[j];
        let [ax, ay] = pos_ring[i];
        let [bx, by] = pos_ring[j];

        let normal = normalize([(by - ay) as f64, -(bx - ax) as f64, 0.0]);

        let au_c = au.clamp(texel_u, 1.0 - texel_u);
        let av_c = av.clamp(texel_v, 1.0 - texel_v);
        let bu_c = bu.clamp(texel_u, 1.0 - texel_u);
        let bv_c = bv.clamp(texel_v, 1.0 - texel_v);

        push_quad(
            vertices,
            indices,
            [[ax, ay, -hd], [ax, ay, hd], [bx, by, hd], [bx, by, -hd]],
            [normal[0] as f32, normal[1] as f32, 0.0],
            [[au_c, av_c], [au_c, av_c], [bu_c, bv_c], [bu_c, bv_c]],
            2,
        );
    }
}

fn alpha_field(texture: &Texture, cols: usize, rows: usize) -> Vec<f32> {
    let mut field = vec![0.0f32; cols * rows];
    for gy in 0..rows {
        let v = gy as f64 / (rows - 1).max(1) as f64;
        for gx in 0..cols {
            let u = gx as f64 / (cols - 1).max(1) as f64;
            field[gy * cols + gx] = texture.sample(u, v)[3] as f32 / 255.0;
        }
    }
    field
}

#[cfg(test)]
mod tests {
    use super::*;

    fn padded_texture() -> Texture<'static> {
        let mut pixels = vec![[0, 0, 0, 0]; 16 * 16];
        for y in 4..12 {
            for x in 4..12 {
                pixels[y * 16 + x] = [240, 100, 40, 255];
            }
        }
        let leaked = Box::leak(pixels.into_boxed_slice());
        Texture {
            pixels: leaked,
            width: 16,
            height: 16,
        }
    }

    #[test]
    fn geometry_trims_to_opaque_content() {
        let texture = padded_texture();
        let (vertices, _) = extruded_billboard_geometry(&texture, 0.1, true);
        let min_x = vertices
            .iter()
            .map(|v| v.position[0])
            .fold(f32::INFINITY, f32::min);
        let max_x = vertices
            .iter()
            .map(|v| v.position[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::INFINITY, f32::min);
        let max_y = vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(
            min_x > -0.7 && max_x < 0.7,
            "side walls should be trimmed to opaque region, got {min_x}..{max_x}"
        );
        assert!(
            min_y > -0.7 && max_y < 0.7,
            "top/bottom should be trimmed to opaque region, got {min_y}..{max_y}"
        );
    }

    #[test]
    fn extruded_geometry_has_front_back_caps_and_sides() {
        let texture = padded_texture();
        let (vertices, indices) = extruded_billboard_geometry(&texture, 0.1, true);
        assert!(!indices.is_empty(), "should produce geometry");

        let count = |ft: u32| {
            indices
                .chunks(3)
                .filter(|tri| tri.iter().all(|&i| vertices[i as usize].face_type == ft))
                .count()
        };
        let front = count(0);
        let back = count(1);
        let sides = count(2);

        eprintln!(
            "front: {front}, back: {back}, sides: {sides}, total: {}",
            indices.len() / 3
        );
        assert!(front > 0, "must have front cap triangles");
        assert!(back > 0, "must have back cap triangles");
        assert!(sides > 0, "must have side wall triangles");
        assert_eq!(
            front, back,
            "front and back should have equal triangle count"
        );
    }

    #[test]
    fn front_cap_uvs_in_opaque_region() {
        let texture = padded_texture();
        let (vertices, _) = extruded_billboard_geometry(&texture, 0.1, true);
        let front_verts: Vec<_> = vertices.iter().filter(|v| v.face_type == 0).collect();
        assert!(!front_verts.is_empty());

        let min_u = front_verts
            .iter()
            .map(|v| v.uv[0])
            .fold(f32::INFINITY, f32::min);
        let max_u = front_verts
            .iter()
            .map(|v| v.uv[0])
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(
            min_u > 0.1 && max_u < 0.9,
            "front cap UVs should be trimmed to opaque region; got u=[{min_u}, {max_u}]"
        );
    }

    #[test]
    fn circle_has_diagonal_side_normals() {
        let mut pixels = vec![[0, 0, 0, 0]; 32 * 32];
        for y in 0..32 {
            for x in 0..32 {
                let dx = x as f32 - 15.5;
                let dy = y as f32 - 15.5;
                if dx * dx + dy * dy < 12.0 * 12.0 {
                    pixels[y * 32 + x] = [255, 0, 0, 255];
                }
            }
        }
        let leaked = Box::leak(pixels.into_boxed_slice());
        let texture = Texture {
            pixels: leaked,
            width: 32,
            height: 32,
        };
        let (vertices, indices) = extruded_billboard_geometry(&texture, 0.1, true);
        assert!(!indices.is_empty());

        let has_diagonal = vertices
            .iter()
            .filter(|v| v.face_type == 2)
            .any(|v| v.normal[0].abs() > 0.01 && v.normal[1].abs() > 0.01);
        assert!(
            has_diagonal,
            "circle should have diagonal side-wall normals"
        );
    }

    #[test]
    fn fully_opaque_falls_back_to_rect() {
        let pixels = vec![[255, 0, 0, 255]; 16 * 16];
        let leaked = Box::leak(pixels.into_boxed_slice());
        let texture = Texture {
            pixels: leaked,
            width: 16,
            height: 16,
        };
        let (_vertices, indices) = extruded_billboard_geometry(&texture, 0.1, true);
        assert!(
            !indices.is_empty(),
            "fully opaque should produce rect fallback geometry"
        );
    }

    #[test]
    fn fully_transparent_produces_empty_geometry() {
        let pixels = vec![[0, 0, 0, 0]; 16 * 16];
        let leaked = Box::leak(pixels.into_boxed_slice());
        let texture = Texture {
            pixels: leaked,
            width: 16,
            height: 16,
        };
        let (vertices, indices) = extruded_billboard_geometry(&texture, 0.1, true);
        assert!(
            vertices.is_empty() && indices.is_empty(),
            "fully transparent should produce no geometry"
        );
    }
}
