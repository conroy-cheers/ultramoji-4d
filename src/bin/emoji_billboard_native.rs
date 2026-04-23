use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use egui_wgpu::{Renderer as EguiRenderer, ScreenDescriptor};
use egui_winit::State as EguiWinitState;
use emoji_renderer::gpu::{GpuRenderer, emoji_preview_scene_params};
use pollster::block_on;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopBuilder};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

mod terminal_renderer {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/emoji-web/src/terminal_renderer.rs"
    ));
}

mod gallery {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/emoji-web/src/gallery.rs"
    ));
}

mod egui_panel {
    use super::{RenderConfig, TransferTuning};

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/emoji-web/src/egui_panel.rs"
    ));
}

use terminal_renderer::{TERM_COLS, TERM_ROWS, TerminalGrid, TerminalRenderer};

const COMPOSITE_SHADER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/emoji-web/src/composite.wgsl"
));
const BLIT_SHADER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/emoji-renderer/src/present_blit.wgsl"
));

#[derive(Clone, Copy, PartialEq)]
struct TransferTuning {
    linear_gain: f32,
    gamma: f32,
    lift: f32,
    saturation: f32,
}

impl Default for TransferTuning {
    fn default() -> Self {
        Self {
            linear_gain: 1.15,
            gamma: 1.0,
            lift: -0.05,
            saturation: 1.25,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct PerfToggles {
    crt: bool,
    transfer: bool,
    overlay_filter: bool,
    billboard: bool,
}

impl Default for PerfToggles {
    fn default() -> Self {
        Self {
            crt: true,
            transfer: true,
            overlay_filter: true,
            billboard: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct RenderConfig {
    gallery_canvas_scale: f32,
    preview_canvas_scale: f32,
    preview_max_dim: u32,
    preview_render_scale: f32,
    display_pixelated: bool,
    overlay_filter: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            gallery_canvas_scale: 1.0,
            preview_canvas_scale: 1.0,
            preview_max_dim: 320,
            preview_render_scale: 2.0,
            display_pixelated: false,
            overlay_filter: false,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    output_size: [f32; 2],
    time_secs: f32,
    preview_mix: f32,
    terminal_rect: [f32; 4],
    overlay_uv_rect: [f32; 4],
    billboard_rect: [f32; 4],
    terminal_grid: [f32; 4],
    transfer_tuning: [f32; 4],
    perf_toggles: [f32; 4],
    channel_switch: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitUniforms {
    output_size: [f32; 2],
    opacity: f32,
    apply_transfer: f32,
    dest_rect: [f32; 4],
    uv_rect: [f32; 4],
    transfer_tuning: [f32; 4],
    extra_params: [f32; 4],
}

#[derive(Clone, Copy, Debug, Default)]
struct NativePerfSnapshot {
    smoothed_fps: f32,
    smoothed_frame_cpu_ms: f32,
    smoothed_frame_interval_ms: f32,
    smoothed_surface_acquire_ms: f32,
    smoothed_terminal_ms: f32,
    smoothed_screen_ms: f32,
    smoothed_scene_ms: f32,
    smoothed_egui_ms: f32,
    smoothed_composite_ms: f32,
    window_width: u32,
    window_height: u32,
    surface_width: u32,
    surface_height: u32,
    terminal_width: u32,
    terminal_height: u32,
    scale_factor: f32,
    preview_mix: f32,
    egui_paint_jobs: u32,
    egui_textures_delta: u32,
    last_screen_redrew: bool,
    last_previewing: bool,
    last_uses_billboard: bool,
    offscreen_stats: Option<emoji_renderer::gpu::OffscreenPerfStats>,
}

struct NativeBillboardApp {
    options: RuntimeOptions,
    window: Option<Arc<Window>>,
    renderer: Option<RendererState>,
    exit_error: Option<anyhow::Error>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendChoice {
    Auto,
    X11,
    Wayland,
}

#[derive(Clone, Copy, Debug)]
struct RuntimeOptions {
    window_width: u32,
    window_height: u32,
    start_preview: bool,
    show_egui: bool,
    log_perf: bool,
    exit_after_secs: Option<f64>,
}

impl RuntimeOptions {
    fn from_env() -> Self {
        fn env_u32(name: &str, default: u32) -> u32 {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }

        fn env_bool(name: &str, default: bool) -> bool {
            std::env::var(name)
                .ok()
                .map(|s| matches!(s.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(default)
        }

        fn env_f64(name: &str) -> Option<f64> {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| *v > 0.0)
        }

        Self {
            window_width: env_u32("EMOJI_BILLBOARD_WIDTH", 1280),
            window_height: env_u32("EMOJI_BILLBOARD_HEIGHT", 960),
            start_preview: env_bool("EMOJI_BILLBOARD_START_PREVIEW", false),
            show_egui: env_bool("EMOJI_BILLBOARD_SHOW_EGUI", true),
            log_perf: env_bool("EMOJI_BILLBOARD_LOG_PERF", false),
            exit_after_secs: env_f64("EMOJI_BILLBOARD_EXIT_AFTER_SECS"),
        }
    }
}

struct RendererState {
    options: RuntimeOptions,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: GpuRenderer,
    surface_format: wgpu::TextureFormat,

    screen_pipeline: wgpu::RenderPipeline,
    screen_bind_group_layout: wgpu::BindGroupLayout,
    screen_bind_group: wgpu::BindGroup,
    screen_overlay_filter: bool,
    screen_uniform_buffer: wgpu::Buffer,
    screen_texture: wgpu::Texture,
    screen_texture_view: wgpu::TextureView,
    screen_dirty: bool,
    last_screen_transfer: TransferTuning,
    last_screen_perf_toggles: PerfToggles,
    last_screen_render_config: RenderConfig,
    last_screen_preview_mix: f32,

    composite_pipeline: wgpu::RenderPipeline,
    composite_bind_group_layout: wgpu::BindGroupLayout,
    composite_bind_group: wgpu::BindGroup,
    composite_uses_billboard: bool,
    composite_display_pixelated: bool,
    composite_billboard_generation: u64,
    composite_uniform_buffer: wgpu::Buffer,
    blit_pipeline: wgpu::RenderPipeline,
    blit_pipeline_opaque: wgpu::RenderPipeline,
    blit_bind_group_layout: wgpu::BindGroupLayout,
    blit_billboard_uniform_buffer: wgpu::Buffer,
    blit_screen_uniform_buffer: wgpu::Buffer,
    preview_screen_bind_group: wgpu::BindGroup,
    preview_billboard_bind_group: wgpu::BindGroup,
    preview_screen_display_pixelated: bool,
    preview_billboard_display_pixelated: bool,
    preview_billboard_generation: u64,

    overlay_sampler_linear: wgpu::Sampler,
    overlay_sampler_nearest: wgpu::Sampler,
    billboard_sampler_linear: wgpu::Sampler,
    billboard_sampler_nearest: wgpu::Sampler,
    placeholder_billboard_view: wgpu::TextureView,

    terminal_renderer: TerminalRenderer,
    terminal_grid: TerminalGrid,
    terminal_dirty: bool,
    gallery: gallery::Gallery,
    demo_pixels: Vec<[u8; 4]>,
    demo_w: u32,
    demo_h: u32,

    egui_ctx: egui::Context,
    egui_state: EguiWinitState,
    egui_renderer: EguiRenderer,
    egui_dirty: bool,
    last_egui_update_secs: f64,
    cached_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
    cached_screen_descriptor: ScreenDescriptor,

    transfer: TransferTuning,
    perf_toggles: PerfToggles,
    render_config: RenderConfig,

    start_time: Instant,
    last_time_secs: f64,
    last_blink_on: bool,
    last_preview_overlay_visible: bool,
    preview_scene_time_origin_secs: f64,
    last_preview_reset_nonce: u32,
    perf: NativePerfSnapshot,
    next_perf_log_secs: f64,
}

impl NativeBillboardApp {
    fn new(options: RuntimeOptions) -> Self {
        Self {
            options,
            window: None,
            renderer: None,
            exit_error: None,
        }
    }
}

impl ApplicationHandler for NativeBillboardApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }

        let window = match event_loop.create_window(
            WindowAttributes::default()
                .with_title("Emoji Billboard Native")
                .with_inner_size(PhysicalSize::new(
                    self.options.window_width,
                    self.options.window_height,
                )),
        ) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.exit_error = Some(anyhow!(err));
                event_loop.exit();
                return;
            }
        };

        match block_on(RendererState::new(window.clone(), self.options)) {
            Ok(renderer) => {
                self.window = Some(window);
                self.renderer = Some(renderer);
            }
            Err(err) => {
                self.exit_error = Some(err);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        if renderer.window.id() != window_id {
            return;
        }

        if renderer
            .egui_state
            .on_window_event(&renderer.window, &event)
            .consumed
        {
            renderer.egui_dirty = true;
            renderer.window.request_redraw();
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                renderer.resize(renderer.window.inner_size());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !renderer.egui_ctx.wants_keyboard_input() {
                    renderer.handle_key_event(&event);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = renderer.frame() {
                    self.exit_error = Some(err);
                    event_loop.exit();
                } else if renderer.should_exit() {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window.request_redraw();
        }
    }
}

impl RendererState {
    async fn new(window: Arc<Window>, options: RuntimeOptions) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .ok_or_else(|| anyhow!("no Vulkan/WebGPU adapter for native billboard viewer"))?;

        let adapter_limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("emoji_billboard_native"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter_limits,
                    ..Default::default()
                },
                None,
            )
            .await?;

        let linear_depth_format = if adapter
            .get_texture_format_features(wgpu::TextureFormat::R32Float)
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            wgpu::TextureFormat::R32Float
        } else {
            wgpu::TextureFormat::R16Float
        };

        let independent_blend_supported = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::INDEPENDENT_BLEND);
        let renderer = GpuRenderer::from_device_queue(
            device,
            queue,
            wgpu::Features::empty(),
            linear_depth_format,
            independent_blend_supported,
        )?;

        let caps = surface.get_capabilities(&adapter);
        let surface_format = preferred_surface_format(&caps.formats);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: preferred_present_mode(&caps.present_modes),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(renderer.device(), &config);

        let terminal_renderer = TerminalRenderer::new(renderer.device(), renderer.queue())?;
        let terminal_grid = TerminalGrid::new();
        let render_config = RenderConfig::default();
        let transfer = TransferTuning::default();
        let perf_toggles = PerfToggles::default();

        let shader = renderer
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("emoji_billboard_native_shader"),
                source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
            });

        let screen_bind_group_layout =
            renderer
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("screen_bgl"),
                    entries: &[
                        bgl_texture(0),
                        bgl_sampler(1),
                        uniform_bgl_entry(2),
                        bgl_texture(3),
                        bgl_sampler(4),
                    ],
                });
        let composite_bind_group_layout =
            renderer
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("composite_bgl"),
                    entries: &[
                        bgl_texture(0),
                        bgl_sampler(1),
                        uniform_bgl_entry(2),
                        bgl_texture(3),
                        bgl_sampler(4),
                    ],
                });

        let screen_uniform_buffer = renderer.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("screen_uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_uniform_buffer = renderer.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite_uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let screen_pipeline_layout =
            renderer
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("screen_pipeline_layout"),
                    bind_group_layouts: &[&screen_bind_group_layout],
                    push_constant_ranges: &[],
                });
        let screen_pipeline =
            renderer
                .device()
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("screen_pipeline"),
                    layout: Some(&screen_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_screen"),
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

        let composite_pipeline_layout =
            renderer
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("composite_pipeline_layout"),
                    bind_group_layouts: &[&composite_bind_group_layout],
                    push_constant_ranges: &[],
                });
        let composite_pipeline =
            renderer
                .device()
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("composite_pipeline"),
                    layout: Some(&composite_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_composite"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: config.format,
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

        let blit_shader = renderer
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("emoji_billboard_native_blit_shader"),
                source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
            });
        let blit_bind_group_layout =
            renderer
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("blit_bgl"),
                    entries: &[
                        bgl_texture(0),
                        bgl_sampler(1),
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });
        let blit_billboard_uniform_buffer =
            renderer.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("blit_billboard_uniforms"),
                size: std::mem::size_of::<BlitUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        let blit_screen_uniform_buffer = renderer.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit_screen_uniforms"),
            size: std::mem::size_of::<BlitUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blit_pipeline_layout =
            renderer
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("blit_pipeline_layout"),
                    bind_group_layouts: &[&blit_bind_group_layout],
                    push_constant_ranges: &[],
                });
        let blit_pipeline =
            renderer
                .device()
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("blit_pipeline"),
                    layout: Some(&blit_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &blit_shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &blit_shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: config.format,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
        let blit_pipeline_opaque =
            renderer
                .device()
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("blit_pipeline_opaque"),
                    layout: Some(&blit_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &blit_shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &blit_shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: config.format,
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

        let overlay_sampler_linear = create_sampler(renderer.device(), wgpu::FilterMode::Linear);
        let overlay_sampler_nearest = create_sampler(renderer.device(), wgpu::FilterMode::Nearest);
        let billboard_sampler_linear = create_sampler(renderer.device(), wgpu::FilterMode::Linear);
        let billboard_sampler_nearest =
            create_sampler(renderer.device(), wgpu::FilterMode::Nearest);

        let (placeholder_billboard_tex, placeholder_billboard_view) =
            create_rgba_texture(renderer.device(), 1, 1);
        let _keep_placeholder = placeholder_billboard_tex;
        let (screen_texture, screen_texture_view) = create_render_target_texture(
            renderer.device(),
            terminal_renderer.pixel_width(),
            terminal_renderer.pixel_height(),
            "screen_effect_texture",
        );

        let screen_bind_group = renderer
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("screen_bg"),
                layout: &screen_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            terminal_renderer.texture_view(),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&overlay_sampler_linear),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: screen_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&placeholder_billboard_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&overlay_sampler_linear),
                    },
                ],
            });
        let composite_bind_group =
            renderer
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("composite_bg"),
                    layout: &composite_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&screen_texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&overlay_sampler_linear),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: composite_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                &placeholder_billboard_view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::Sampler(&billboard_sampler_linear),
                        },
                    ],
                });
        let preview_screen_bind_group =
            renderer
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("preview_screen_bg"),
                    layout: &blit_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&screen_texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&overlay_sampler_linear),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: blit_screen_uniform_buffer.as_entire_binding(),
                        },
                    ],
                });
        let preview_billboard_bind_group =
            renderer
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("preview_billboard_bg"),
                    layout: &blit_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(
                                &placeholder_billboard_view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&billboard_sampler_linear),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: blit_billboard_uniform_buffer.as_entire_binding(),
                        },
                    ],
                });

        let egui_ctx = egui::Context::default();
        let egui_state = EguiWinitState::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let egui_renderer = EguiRenderer::new(renderer.device(), surface_format, None, 1, false);

        let (demo_pixels, demo_w, demo_h) = demo_texture();
        let initial_surface_width = config.width;
        let initial_surface_height = config.height;
        let initial_terminal_width = terminal_renderer.pixel_width();
        let initial_terminal_height = terminal_renderer.pixel_height();
        let initial_scale_factor = window.scale_factor() as f32;
        let mut gallery = gallery::Gallery::new();
        let mut terminal_dirty = true;
        let mut screen_dirty = true;
        if options.start_preview {
            gallery.enter_preview_immediate();
        }

        Ok(Self {
            options,
            window,
            surface,
            config,
            renderer,
            surface_format,
            screen_pipeline,
            screen_bind_group_layout,
            screen_bind_group,
            screen_overlay_filter: true,
            screen_uniform_buffer,
            screen_texture,
            screen_texture_view,
            screen_dirty,
            last_screen_transfer: transfer,
            last_screen_perf_toggles: perf_toggles,
            last_screen_render_config: render_config,
            last_screen_preview_mix: -1.0,
            composite_pipeline,
            composite_bind_group_layout,
            composite_bind_group,
            composite_uses_billboard: false,
            composite_display_pixelated: false,
            composite_billboard_generation: 0,
            composite_uniform_buffer,
            blit_pipeline,
            blit_pipeline_opaque,
            blit_bind_group_layout,
            blit_billboard_uniform_buffer,
            blit_screen_uniform_buffer,
            preview_screen_bind_group,
            preview_billboard_bind_group,
            preview_screen_display_pixelated: false,
            preview_billboard_display_pixelated: false,
            preview_billboard_generation: 0,
            overlay_sampler_linear,
            overlay_sampler_nearest,
            billboard_sampler_linear,
            billboard_sampler_nearest,
            placeholder_billboard_view,
            terminal_renderer,
            terminal_grid,
            terminal_dirty,
            gallery,
            demo_pixels,
            demo_w,
            demo_h,
            egui_ctx,
            egui_state,
            egui_renderer,
            egui_dirty: true,
            last_egui_update_secs: -1.0,
            cached_paint_jobs: Vec::new(),
            cached_screen_descriptor: ScreenDescriptor {
                size_in_pixels: [initial_surface_width, initial_surface_height],
                pixels_per_point: initial_scale_factor,
            },
            transfer,
            perf_toggles,
            render_config,
            start_time: Instant::now(),
            last_time_secs: 0.0,
            last_blink_on: true,
            last_preview_overlay_visible: false,
            preview_scene_time_origin_secs: 0.0,
            last_preview_reset_nonce: 0,
            perf: NativePerfSnapshot {
                smoothed_fps: 60.0,
                window_width: size.width.max(1),
                window_height: size.height.max(1),
                surface_width: initial_surface_width,
                surface_height: initial_surface_height,
                terminal_width: initial_terminal_width,
                terminal_height: initial_terminal_height,
                scale_factor: initial_scale_factor,
                last_screen_redrew: true,
                ..Default::default()
            },
            next_perf_log_secs: 1.0,
        })
    }

    fn should_exit(&self) -> bool {
        self.options
            .exit_after_secs
            .map(|limit| self.start_time.elapsed().as_secs_f64() >= limit)
            .unwrap_or(false)
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.reconfigure_surface(size);
    }

    fn reconfigure_surface(&mut self, size: PhysicalSize<u32>) {
        let scale = if self.gallery.is_previewing() {
            self.render_config.preview_canvas_scale
        } else {
            self.render_config.gallery_canvas_scale
        };
        self.config.width = ((size.width as f32) * scale).round().max(1.0) as u32;
        self.config.height = ((size.height as f32) * scale).round().max(1.0) as u32;
        self.surface.configure(self.renderer.device(), &self.config);
        self.egui_dirty = true;
    }

    fn handle_key_event(&mut self, event: &winit::event::KeyEvent) {
        if !event.state.is_pressed() {
            return;
        }
        let action = match &event.logical_key {
            Key::Named(NamedKey::ArrowUp) => Some(gallery::KeyAction::Up),
            Key::Named(NamedKey::ArrowDown) => Some(gallery::KeyAction::Down),
            Key::Named(NamedKey::Enter) => Some(gallery::KeyAction::Enter),
            Key::Named(NamedKey::Escape) => Some(gallery::KeyAction::Escape),
            Key::Named(NamedKey::Backspace) => Some(gallery::KeyAction::Backspace),
            Key::Character(text) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) if ch.is_ascii_graphic() || ch == ' ' => {
                        Some(gallery::KeyAction::Char(ch))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(action) = action {
            self.gallery.handle_key(action);
            self.terminal_dirty = true;
            self.screen_dirty = true;
        }
    }

    fn frame(&mut self) -> Result<()> {
        let frame_start = Instant::now();
        let now = self.start_time.elapsed().as_secs_f64();
        let dt_secs = (now - self.last_time_secs).max(0.0);
        self.last_time_secs = now;
        self.gallery.tick(dt_secs as f32);
        if dt_secs > 0.0 {
            let fps = (1.0 / dt_secs) as f32;
            self.perf.smoothed_fps = self.perf.smoothed_fps * 0.9 + fps * 0.1;
            self.perf.smoothed_frame_interval_ms =
                self.perf.smoothed_frame_interval_ms * 0.9 + (dt_secs as f32 * 1000.0) * 0.1;
        }

        let blink_on = gallery::cursor_blink_on(now);
        let preview_overlay_visible = gallery::show_preview_overlay(&self.gallery);
        if blink_on != self.last_blink_on
            || preview_overlay_visible != self.last_preview_overlay_visible
        {
            self.last_blink_on = blink_on;
            self.last_preview_overlay_visible = preview_overlay_visible;
            self.terminal_dirty = true;
            self.screen_dirty = true;
        }

        let mut egui_ms = 0.0f32;
        let mut textures_delta_set_count = 0usize;
        let mut textures_delta_free_count = 0usize;
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: self.window.scale_factor() as f32,
        };
        if self.options.show_egui {
            let mut transfer = self.transfer;
            let mut render_config = self.render_config;
            let perf = self.perf;
            let egui_refresh_due = (now - self.last_egui_update_secs) >= 0.1;
            if self.egui_dirty || egui_refresh_due {
                let raw_input = self.egui_state.take_egui_input(&self.window);
                let egui_ui_start = Instant::now();
                let panel_response = self.egui_ctx.run(raw_input, |ctx| {
                    Self::draw_egui(ctx, &mut transfer, &mut render_config, perf)
                });
                egui_ms += egui_ui_start.elapsed().as_secs_f32() * 1000.0;
                self.transfer = transfer;
                self.render_config = render_config;
                self.egui_state
                    .handle_platform_output(&self.window, panel_response.platform_output.clone());
                let egui_prepare_start = Instant::now();
                let paint_jobs = self
                    .egui_ctx
                    .tessellate(panel_response.shapes, screen_descriptor.pixels_per_point);
                textures_delta_set_count = panel_response.textures_delta.set.len();
                textures_delta_free_count = panel_response.textures_delta.free.len();
                for (id, delta) in &panel_response.textures_delta.set {
                    self.egui_renderer.update_texture(
                        self.renderer.device(),
                        self.renderer.queue(),
                        *id,
                        delta,
                    );
                }
                self.cached_paint_jobs = paint_jobs;
                self.cached_screen_descriptor = screen_descriptor;
                self.last_egui_update_secs = now;
                self.egui_dirty = false;
                egui_ms += egui_prepare_start.elapsed().as_secs_f32() * 1000.0;
                for id in &panel_response.textures_delta.free {
                    self.egui_renderer.free_texture(id);
                }
            }
        } else {
            self.cached_paint_jobs.clear();
            self.cached_screen_descriptor = screen_descriptor;
        }

        let desired_surface_scale = if self.gallery.is_previewing() {
            self.render_config.preview_canvas_scale
        } else {
            self.render_config.gallery_canvas_scale
        };
        let actual_surface_scale =
            self.config.width as f32 / self.window.inner_size().width.max(1) as f32;
        if (desired_surface_scale - actual_surface_scale).abs() > 0.01 {
            self.reconfigure_surface(self.window.inner_size());
        }

        let term_start = Instant::now();
        if self.terminal_dirty {
            gallery::render_to_grid(&mut self.terminal_grid, &self.gallery, now);
            self.terminal_renderer.render(
                self.renderer.device(),
                self.renderer.queue(),
                &self.terminal_grid,
            );
            self.terminal_dirty = false;
            self.screen_dirty = true;
        }
        let terminal_ms = term_start.elapsed().as_secs_f32() * 1000.0;

        let previewing = self.gallery.is_previewing();
        let preview_mix = self.gallery.preview_mix();
        let preview_reset_nonce = self.gallery.preview_reset_nonce();
        if previewing && preview_reset_nonce != self.last_preview_reset_nonce {
            self.preview_scene_time_origin_secs = now;
            self.last_preview_reset_nonce = preview_reset_nonce;
        }
        let preview_scene_time_secs = if previewing {
            (now - self.preview_scene_time_origin_secs).max(0.0)
        } else {
            now
        };
        if self.last_screen_transfer != self.transfer
            || self.last_screen_perf_toggles != self.perf_toggles
            || self.last_screen_render_config.overlay_filter != self.render_config.overlay_filter
            || (self.last_screen_preview_mix - preview_mix).abs() > 0.001
        {
            self.screen_dirty = true;
        }

        let overlay_w = self.terminal_renderer.pixel_width();
        let overlay_h = self.terminal_renderer.pixel_height();
        let scene_start = Instant::now();
        let billboard_pixel_rect: [f32; 4] = if previewing && self.perf_toggles.billboard {
            let canvas_w = self.config.width.max(1) as f32;
            let canvas_h = self.config.height.max(1) as f32;
            let target_max_dim = self.render_config.preview_max_dim as f32;
            let canvas_max = canvas_w.max(canvas_h).max(1.0);
            let scale = target_max_dim / canvas_max;
            let render_w = (canvas_w * scale).round().max(1.0) as u32;
            let render_h = (canvas_h * scale).round().max(1.0) as u32;

            let texture = emoji_renderer::texture::Texture {
                pixels: &self.demo_pixels,
                width: self.demo_w,
                height: self.demo_h,
            };
            let mut params = emoji_preview_scene_params();
            params.sharpen = Some(0.1);
            params.dither = Some(0.3);
            params.vhs = Some(0.5);
            params.jitter = Some(0.1);
            params.supersample = true;
            params.render_scale = Some(self.render_config.preview_render_scale);
            self.renderer.render_to_offscreen_params(
                &texture,
                render_w,
                render_h,
                preview_scene_time_secs,
                &params,
            )?;

            [0.0, 0.0, canvas_w, canvas_h]
        } else {
            [0.0; 4]
        };
        let scene_ms = scene_start.elapsed().as_secs_f32() * 1000.0;

        let screen_redrew = self.screen_dirty;
        let screen_start = Instant::now();
        if self.screen_dirty {
            let screen_uniforms = CompositeUniforms {
                output_size: [overlay_w as f32, overlay_h as f32],
                time_secs: now as f32,
                preview_mix,
                terminal_rect: [0.0; 4],
                overlay_uv_rect: [0.0, 0.0, 1.0, 1.0],
                billboard_rect: [0.0; 4],
                terminal_grid: [
                    TERM_COLS as f32,
                    TERM_ROWS as f32,
                    overlay_w as f32 / TERM_COLS as f32,
                    overlay_h as f32 / TERM_ROWS as f32,
                ],
                transfer_tuning: [
                    self.transfer.linear_gain,
                    self.transfer.gamma,
                    self.transfer.lift,
                    self.transfer.saturation,
                ],
                perf_toggles: [
                    if self.perf_toggles.crt { 1.0 } else { 0.0 },
                    if self.perf_toggles.transfer { 1.0 } else { 0.0 },
                    if self.perf_toggles.overlay_filter && self.render_config.overlay_filter {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                ],
                channel_switch: [
                    self.gallery.channel_switch(),
                    self.gallery.channel_switch_dir(),
                    0.0,
                    0.0,
                ],
            };
            self.renderer.queue().write_buffer(
                &self.screen_uniform_buffer,
                0,
                bytemuck::bytes_of(&screen_uniforms),
            );
            self.ensure_screen_bind_group(self.render_config.overlay_filter);
            let mut encoder =
                self.renderer
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("screen_effect_encoder"),
                    });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("screen_effect_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.screen_texture_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&self.screen_pipeline);
                pass.set_bind_group(0, &self.screen_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            self.renderer.queue().submit(Some(encoder.finish()));
            self.screen_dirty = false;
            self.last_screen_transfer = self.transfer;
            self.last_screen_perf_toggles = self.perf_toggles;
            self.last_screen_render_config = self.render_config;
            self.last_screen_preview_mix = preview_mix;
        }
        let screen_ms = screen_start.elapsed().as_secs_f32() * 1000.0;
        self.perf.last_screen_redrew = screen_redrew;

        let preview_overlay_bounds = if previewing {
            self.terminal_grid.occupied_rect()
        } else {
            None
        };

        let term_aspect = overlay_w as f32 / overlay_h as f32;
        let canvas_aspect = self.config.width as f32 / self.config.height as f32;
        let (term_w, term_h) = if canvas_aspect > term_aspect {
            let th = self.config.height as f32;
            (th * term_aspect, th)
        } else {
            let tw = self.config.width as f32;
            (tw, tw / term_aspect)
        };
        let term_x = (self.config.width as f32 - term_w) * 0.5;
        let term_y = (self.config.height as f32 - term_h) * 0.5;
        let (terminal_rect, overlay_uv_rect) = if let Some(bounds) = preview_overlay_bounds {
            let cell_w = term_w / TERM_COLS as f32;
            let cell_h = term_h / TERM_ROWS as f32;
            (
                [
                    term_x + bounds.x as f32 * cell_w,
                    term_y + bounds.y as f32 * cell_h,
                    bounds.width as f32 * cell_w,
                    bounds.height as f32 * cell_h,
                ],
                [
                    bounds.x as f32 / TERM_COLS as f32,
                    bounds.y as f32 / TERM_ROWS as f32,
                    bounds.width as f32 / TERM_COLS as f32,
                    bounds.height as f32 / TERM_ROWS as f32,
                ],
            )
        } else {
            ([term_x, term_y, term_w, term_h], [0.0, 0.0, 1.0, 1.0])
        };

        let sx = term_w / overlay_w as f32;
        let sy = term_h / overlay_h as f32;
        let billboard_canvas_rect = if previewing {
            [
                billboard_pixel_rect[0],
                billboard_pixel_rect[1],
                billboard_pixel_rect[2],
                billboard_pixel_rect[3],
            ]
        } else {
            [
                term_x + billboard_pixel_rect[0] * sx,
                term_y + billboard_pixel_rect[1] * sy,
                billboard_pixel_rect[2] * sx,
                billboard_pixel_rect[3] * sy,
            ]
        };

        let uniforms = CompositeUniforms {
            output_size: [self.config.width as f32, self.config.height as f32],
            time_secs: now as f32,
            preview_mix,
            terminal_rect,
            overlay_uv_rect,
            billboard_rect: billboard_canvas_rect,
            terminal_grid: [
                TERM_COLS as f32,
                TERM_ROWS as f32,
                term_w / TERM_COLS as f32,
                term_h / TERM_ROWS as f32,
            ],
            transfer_tuning: [
                self.transfer.linear_gain,
                self.transfer.gamma,
                self.transfer.lift,
                self.transfer.saturation,
            ],
            perf_toggles: [
                if self.perf_toggles.crt { 1.0 } else { 0.0 },
                if self.perf_toggles.transfer { 1.0 } else { 0.0 },
                if self.perf_toggles.overlay_filter && self.render_config.overlay_filter {
                    1.0
                } else {
                    0.0
                },
                if self.perf_toggles.billboard {
                    1.0
                } else {
                    0.0
                },
            ],
            channel_switch: [
                self.gallery.channel_switch(),
                self.gallery.channel_switch_dir(),
                0.0,
                0.0,
            ],
        };
        self.renderer.queue().write_buffer(
            &self.composite_uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let uses_billboard = previewing && preview_mix > 0.0 && billboard_canvas_rect[2] > 0.0;
        self.ensure_composite_bind_group(
            uses_billboard,
            self.render_config.display_pixelated,
            self.renderer.render_target_generation(),
        );

        let surface_acquire_start = Instant::now();
        let output = self.surface.get_current_texture()?;
        let surface_acquire_ms = surface_acquire_start.elapsed().as_secs_f32() * 1000.0;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let composite_start = Instant::now();
        let mut encoder =
            self.renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("native_composite_encoder"),
                });
        if self.options.show_egui {
            self.egui_renderer.update_buffers(
                self.renderer.device(),
                self.renderer.queue(),
                &mut encoder,
                &self.cached_paint_jobs,
                &self.cached_screen_descriptor,
            );
        }
        let clear = if preview_mix > 0.0 {
            wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }
        } else {
            wgpu::Color {
                r: 0.0,
                g: 0.03,
                b: 0.01,
                a: 1.0,
            }
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native_composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if preview_mix > 0.0 {
                self.ensure_preview_blit_bind_groups(
                    self.render_config.display_pixelated,
                    self.renderer.render_target_generation(),
                );

                if uses_billboard {
                    let uniforms = BlitUniforms {
                        output_size: [self.config.width as f32, self.config.height as f32],
                        opacity: preview_mix,
                        apply_transfer: 1.0,
                        dest_rect: billboard_canvas_rect,
                        uv_rect: [0.0, 0.0, 1.0, 1.0],
                        transfer_tuning: [
                            self.transfer.linear_gain,
                            self.transfer.gamma,
                            self.transfer.lift,
                            self.transfer.saturation,
                        ],
                        extra_params: [
                            1.0,
                            self.gallery.channel_switch(),
                            self.gallery.channel_switch_dir(),
                            now as f32,
                        ],
                    };
                    self.renderer.queue().write_buffer(
                        &self.blit_billboard_uniform_buffer,
                        0,
                        bytemuck::bytes_of(&uniforms),
                    );
                    let billboard_pipeline = if preview_mix >= 0.999 {
                        &self.blit_pipeline_opaque
                    } else {
                        &self.blit_pipeline
                    };
                    pass.set_pipeline(billboard_pipeline);
                    pass.set_bind_group(0, &self.preview_billboard_bind_group, &[]);
                    pass.draw(0..6, 0..1);
                }

                let screen_uniforms = BlitUniforms {
                    output_size: [self.config.width as f32, self.config.height as f32],
                    opacity: 1.0,
                    apply_transfer: 0.0,
                    dest_rect: terminal_rect,
                    uv_rect: overlay_uv_rect,
                    transfer_tuning: [0.0; 4],
                    extra_params: [
                        0.0,
                        self.gallery.channel_switch(),
                        self.gallery.channel_switch_dir(),
                        now as f32,
                    ],
                };
                self.renderer.queue().write_buffer(
                    &self.blit_screen_uniform_buffer,
                    0,
                    bytemuck::bytes_of(&screen_uniforms),
                );
                pass.set_pipeline(&self.blit_pipeline);
                pass.set_bind_group(0, &self.preview_screen_bind_group, &[]);
                pass.draw(0..6, 0..1);
            } else {
                pass.set_pipeline(&self.composite_pipeline);
                pass.set_bind_group(0, &self.composite_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        if self.options.show_egui {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(
                &mut pass,
                &self.cached_paint_jobs,
                &self.cached_screen_descriptor,
            );
        }

        self.renderer.queue().submit(Some(encoder.finish()));
        output.present();

        let composite_ms = composite_start.elapsed().as_secs_f32() * 1000.0;
        let frame_cpu_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        self.perf.smoothed_surface_acquire_ms =
            self.perf.smoothed_surface_acquire_ms * 0.85 + surface_acquire_ms * 0.15;
        self.perf.smoothed_terminal_ms = self.perf.smoothed_terminal_ms * 0.85 + terminal_ms * 0.15;
        self.perf.smoothed_screen_ms = self.perf.smoothed_screen_ms * 0.85 + screen_ms * 0.15;
        self.perf.smoothed_scene_ms = self.perf.smoothed_scene_ms * 0.85 + scene_ms * 0.15;
        self.perf.smoothed_egui_ms = self.perf.smoothed_egui_ms * 0.85 + egui_ms * 0.15;
        self.perf.smoothed_composite_ms =
            self.perf.smoothed_composite_ms * 0.85 + composite_ms * 0.15;
        self.perf.smoothed_frame_cpu_ms =
            self.perf.smoothed_frame_cpu_ms * 0.85 + frame_cpu_ms * 0.15;
        self.perf.window_width = self.window.inner_size().width.max(1);
        self.perf.window_height = self.window.inner_size().height.max(1);
        self.perf.surface_width = self.config.width;
        self.perf.surface_height = self.config.height;
        self.perf.terminal_width = overlay_w;
        self.perf.terminal_height = overlay_h;
        self.perf.scale_factor = self.window.scale_factor() as f32;
        self.perf.preview_mix = preview_mix;
        self.perf.egui_paint_jobs = self.cached_paint_jobs.len() as u32;
        self.perf.egui_textures_delta =
            (textures_delta_set_count + textures_delta_free_count) as u32;
        self.perf.last_previewing = previewing;
        self.perf.last_uses_billboard = uses_billboard;
        self.perf.offscreen_stats = self.renderer.offscreen_perf_stats();
        if self.options.log_perf && now >= self.next_perf_log_secs {
            self.log_perf_line();
            self.next_perf_log_secs += 1.0;
        }

        Ok(())
    }

    fn log_perf_line(&self) {
        let offscreen = self.perf.offscreen_stats.unwrap_or_default();
        eprintln!(
            "PERF fps={:.1} frame_ms={:.2} interval_ms={:.2} acquire_ms={:.2} term_ms={:.2} screen_ms={:.2} scene_ms={:.2} egui_ms={:.2} comp_ms={:.2} surf={}x{} term={}x{} preview={} bill={} scene={}x{} out={}x{} passes={} draws={} downsample={}",
            self.perf.smoothed_fps,
            self.perf.smoothed_frame_cpu_ms,
            self.perf.smoothed_frame_interval_ms,
            self.perf.smoothed_surface_acquire_ms,
            self.perf.smoothed_terminal_ms,
            self.perf.smoothed_screen_ms,
            self.perf.smoothed_scene_ms,
            self.perf.smoothed_egui_ms,
            self.perf.smoothed_composite_ms,
            self.perf.surface_width,
            self.perf.surface_height,
            self.perf.terminal_width,
            self.perf.terminal_height,
            self.perf.last_previewing,
            self.perf.last_uses_billboard,
            offscreen.scene_width,
            offscreen.scene_height,
            offscreen.output_width,
            offscreen.output_height,
            offscreen.pass_count,
            offscreen.draw_call_count,
            offscreen.has_downsample,
        );
    }

    fn draw_egui(
        ctx: &egui::Context,
        transfer: &mut TransferTuning,
        render_config: &mut RenderConfig,
        perf: NativePerfSnapshot,
    ) {
        egui_panel::show_controls_panel(
            ctx,
            transfer,
            render_config,
            egui_panel::PerfPanelData {
                smoothed_fps: perf.smoothed_fps,
                smoothed_frame_cpu_ms: perf.smoothed_frame_cpu_ms,
                smoothed_frame_interval_ms: perf.smoothed_frame_interval_ms,
                smoothed_surface_acquire_ms: perf.smoothed_surface_acquire_ms,
                smoothed_terminal_ms: perf.smoothed_terminal_ms,
                smoothed_screen_ms: perf.smoothed_screen_ms,
                smoothed_scene_ms: perf.smoothed_scene_ms,
                smoothed_egui_ms: perf.smoothed_egui_ms,
                smoothed_composite_ms: perf.smoothed_composite_ms,
                window_width: perf.window_width,
                window_height: perf.window_height,
                surface_width: perf.surface_width,
                surface_height: perf.surface_height,
                terminal_width: perf.terminal_width,
                terminal_height: perf.terminal_height,
                scale_factor: perf.scale_factor,
                preview_mix: perf.preview_mix,
                egui_paint_jobs: perf.egui_paint_jobs,
                egui_textures_delta: perf.egui_textures_delta,
                last_screen_redrew: perf.last_screen_redrew,
                last_previewing: perf.last_previewing,
                last_uses_billboard: perf.last_uses_billboard,
                offscreen_stats: perf.offscreen_stats,
            },
        );
    }

    fn ensure_composite_bind_group(
        &mut self,
        uses_billboard: bool,
        display_pixelated: bool,
        billboard_generation: u64,
    ) {
        if self.composite_uses_billboard == uses_billboard
            && self.composite_display_pixelated == display_pixelated
            && (!uses_billboard || self.composite_billboard_generation == billboard_generation)
        {
            return;
        }

        let billboard_view = if uses_billboard {
            self.renderer
                .offscreen_view()
                .unwrap_or(&self.placeholder_billboard_view)
        } else {
            &self.placeholder_billboard_view
        };
        let overlay_sampler = if display_pixelated {
            &self.overlay_sampler_nearest
        } else {
            &self.overlay_sampler_linear
        };
        let billboard_sampler = if display_pixelated {
            &self.billboard_sampler_nearest
        } else {
            &self.billboard_sampler_linear
        };

        self.composite_bind_group =
            self.renderer
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("native_composite_bg"),
                    layout: &self.composite_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&self.screen_texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(overlay_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.composite_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(billboard_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::Sampler(billboard_sampler),
                        },
                    ],
                });
        self.composite_uses_billboard = uses_billboard;
        self.composite_display_pixelated = display_pixelated;
        self.composite_billboard_generation = billboard_generation;
    }

    fn ensure_screen_bind_group(&mut self, overlay_filter: bool) {
        if self.screen_overlay_filter == overlay_filter {
            return;
        }
        let overlay_sampler = if overlay_filter {
            &self.overlay_sampler_linear
        } else {
            &self.overlay_sampler_nearest
        };
        self.screen_bind_group =
            self.renderer
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("native_screen_bg"),
                    layout: &self.screen_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(
                                self.terminal_renderer.texture_view(),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(overlay_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.screen_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                &self.placeholder_billboard_view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::Sampler(overlay_sampler),
                        },
                    ],
                });
        self.screen_overlay_filter = overlay_filter;
    }

    fn ensure_preview_blit_bind_groups(
        &mut self,
        display_pixelated: bool,
        billboard_generation: u64,
    ) {
        if self.preview_screen_display_pixelated != display_pixelated {
            let screen_sampler = if display_pixelated {
                &self.overlay_sampler_nearest
            } else {
                &self.overlay_sampler_linear
            };
            self.preview_screen_bind_group =
                self.renderer
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("preview_screen_bg"),
                        layout: &self.blit_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(
                                    &self.screen_texture_view,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(screen_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blit_screen_uniform_buffer.as_entire_binding(),
                            },
                        ],
                    });
            self.preview_screen_display_pixelated = display_pixelated;
        }

        if self.preview_billboard_display_pixelated != display_pixelated
            || self.preview_billboard_generation != billboard_generation
        {
            let billboard_view = self
                .renderer
                .offscreen_view()
                .unwrap_or(&self.placeholder_billboard_view);
            let billboard_sampler = if display_pixelated {
                &self.billboard_sampler_nearest
            } else {
                &self.billboard_sampler_linear
            };
            self.preview_billboard_bind_group =
                self.renderer
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("preview_billboard_bg"),
                        layout: &self.blit_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(billboard_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(billboard_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blit_billboard_uniform_buffer.as_entire_binding(),
                            },
                        ],
                    });
            self.preview_billboard_display_pixelated = display_pixelated;
            self.preview_billboard_generation = billboard_generation;
        }
    }
}

fn create_sampler(device: &wgpu::Device, filter: wgpu::FilterMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: filter,
        min_filter: filter,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    })
}

fn create_rgba_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("native_rgba_texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_render_target_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn preferred_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    for format in formats {
        match format {
            wgpu::TextureFormat::Bgra8Unorm => return *format,
            wgpu::TextureFormat::Rgba8Unorm => return *format,
            _ => {}
        }
    }
    formats[0]
}

fn preferred_present_mode(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    if modes.contains(&wgpu::PresentMode::Immediate) {
        return wgpu::PresentMode::Immediate;
    }
    if modes.contains(&wgpu::PresentMode::Mailbox) {
        return wgpu::PresentMode::Mailbox;
    }
    if modes.contains(&wgpu::PresentMode::AutoNoVsync) {
        return wgpu::PresentMode::AutoNoVsync;
    }
    modes[0]
}

fn bgl_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            view_dimension: wgpu::TextureViewDimension::D2,
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
        },
        count: None,
    }
}

fn bgl_sampler(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_bgl_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn demo_texture() -> (Vec<[u8; 4]>, u32, u32) {
    let w = 96u32;
    let h = 96u32;
    let mut pixels = vec![[0u8, 0, 0, 0]; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let inside = x > 12 && x < 84 && y > 12 && y < 84;
            pixels[idx] = if inside {
                if x < 48 {
                    [230, 90, 50, 255]
                } else {
                    [60, 130, 255, 255]
                }
            } else {
                [0, 0, 0, 0]
            };
        }
    }
    (pixels, w, h)
}

fn main() -> Result<()> {
    ensure_linux_gui_runtime_env()?;
    let mut builder = EventLoop::builder();
    configure_event_loop(&mut builder, BackendChoice::Auto);
    let event_loop = builder.build()?;
    let mut app = NativeBillboardApp::new(RuntimeOptions::from_env());
    event_loop.run_app(&mut app)?;
    if let Some(err) = app.exit_error {
        return Err(err);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_linux_gui_runtime_env() -> Result<()> {
    use std::collections::BTreeSet;
    use std::os::unix::process::CommandExt;

    const REEXEC_MARKER: &str = "SLACKSLACK_EMOJI_BILLBOARD_NATIVE_REEXEC";
    if std::env::var_os(REEXEC_MARKER).is_some() {
        return Ok(());
    }

    let mut paths: BTreeSet<String> = std::env::var("LD_LIBRARY_PATH")
        .ok()
        .into_iter()
        .flat_map(|value| value.split(':').map(str::to_owned).collect::<Vec<_>>())
        .filter(|value| !value.is_empty())
        .collect();

    for dir in linux_gui_library_dirs()? {
        paths.insert(dir);
    }

    if paths.is_empty() {
        return Ok(());
    }

    let joined = paths.into_iter().collect::<Vec<_>>().join(":");
    let current = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    if joined == current {
        return Ok(());
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1));
    command.env("LD_LIBRARY_PATH", joined);
    command.env(REEXEC_MARKER, "1");
    let err = command.exec();
    Err(anyhow!(err)).context("failed to re-exec emoji_billboard_native with GUI library path")
}

#[cfg(target_os = "linux")]
fn linux_gui_library_dirs() -> Result<Vec<String>> {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    let mut dirs = BTreeSet::new();

    for dir in [
        "/run/current-system/sw/lib",
        "/run/opengl-driver/lib",
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
    ] {
        if Path::new(dir).exists() {
            dirs.insert(dir.to_string());
        }
    }

    let nix_store = Path::new("/nix/store");
    if nix_store.exists() {
        let wanted = [
            "-wayland-",
            "-libX11-",
            "-libxcb-",
            "-libxkbcommon-",
            "-libXcursor-",
            "-libXi-",
            "-libXrandr-",
            "-libXrender-",
            "-libXext-",
        ];

        for entry in fs::read_dir(nix_store).context("failed to read /nix/store")? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !wanted.iter().any(|needle| name.contains(needle)) {
                continue;
            }

            let lib_dir: PathBuf = entry.path().join("lib");
            if lib_dir.is_dir() {
                dirs.insert(lib_dir.to_string_lossy().into_owned());
            }
        }
    }

    Ok(dirs.into_iter().collect())
}

#[cfg(not(target_os = "linux"))]
fn ensure_linux_gui_runtime_env() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn preferred_backend(choice: BackendChoice) -> Option<BackendChoice> {
    match choice {
        BackendChoice::Auto => {
            let has_x11 = std::env::var_os("DISPLAY").is_some();
            let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
            match std::env::var("XDG_SESSION_TYPE").ok().as_deref() {
                Some("wayland") if has_wayland => Some(BackendChoice::Wayland),
                Some("x11") if has_x11 => Some(BackendChoice::X11),
                _ => match (has_wayland, has_x11) {
                    (true, _) => Some(BackendChoice::Wayland),
                    (false, true) => Some(BackendChoice::X11),
                    (false, false) => None,
                },
            }
        }
        explicit => Some(explicit),
    }
}

#[cfg(not(target_os = "linux"))]
fn preferred_backend(choice: BackendChoice) -> Option<BackendChoice> {
    let _ = choice;
    Some(BackendChoice::Auto)
}

fn configure_event_loop(builder: &mut EventLoopBuilder<()>, choice: BackendChoice) {
    #[cfg(target_os = "linux")]
    match preferred_backend(choice) {
        Some(BackendChoice::X11) => {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }
        Some(BackendChoice::Wayland) => {
            use winit::platform::wayland::EventLoopBuilderExtWayland;
            builder.with_wayland();
        }
        Some(BackendChoice::Auto) | None => {}
    }
}
