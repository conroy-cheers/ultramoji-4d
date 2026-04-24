use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use egui_wgpu::{Renderer as EguiRenderer, ScreenDescriptor};
use emoji_renderer::decode::decode_emoji_frames;
use emoji_renderer::gpu::{GpuRenderer, emoji_preview_scene_params};
use emoji_renderer::texture::{COLOR_SOURCE_ALPHA_THRESHOLD, fill_transparent_rgb_from_nearest};
use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, MouseEvent, WheelEvent};

mod gallery;
mod egui_panel {
    use super::{RenderConfig, TransferTuning};

    include!("egui_panel.rs");
}
mod terminal_renderer;

use terminal_renderer::{TERM_COLS, TERM_ROWS, TerminalGrid, TerminalRenderer};

const COMPOSITE_SHADER: &str = include_str!("composite.wgsl");
const PREVIEW_DRAG_ROTATION_PER_PIXEL: f32 = -0.006;
const PREVIEW_DRAG_MOMENTUM_DECAY_PER_SEC: f32 = 3.8;
const PREVIEW_DRAG_MIN_VELOCITY: f32 = 0.02;
const PREVIEW_DRAG_RELEASE_MIN_VELOCITY: f32 = 0.12;
const PREVIEW_DRAG_RELEASE_MAX_STILL_SECS: f64 = 0.05;
const PREVIEW_DRAG_VELOCITY_BLEND: f32 = 0.35;

#[derive(Clone, Copy, PartialEq)]
struct TransferTuning {
    linear_gain: f32,
    gamma: f32,
    lift: f32,
    saturation: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct PerfToggles {
    crt: bool,
    transfer: bool,
    overlay_filter: bool,
    billboard: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct RenderConfig {
    gallery_canvas_scale: f32,
    preview_canvas_scale: f32,
    preview_max_dim: u32,
    preview_render_scale: f32,
    display_pixelated: bool,
    overlay_filter: bool,
    ambient_light_tint: f32,
    ambient_light_brightness: f32,
    shadow_strength: f32,
    shadow_depth_threshold: f32,
    shadow_start_dist: f32,
    shadow_step_growth: f32,
    shadow_max: f32,
    shadow_jitter_spread: f32,
    shadow_max_depth_delta: f32,
    shadow_bbox_padding: f32,
    shadow_steps: u32,
    shadow_empty_depth_mode: u32,
    contact_shadows: bool,
    contact_shadow_depth_threshold: f32,
    contact_shadow_start_dist: f32,
    contact_shadow_step_dist: f32,
    contact_shadow_max_dist: f32,
    contact_shadow_max_depth_delta: f32,
    contact_shadow_jitter_spread: f32,
    contact_shadow_steps: u32,
    shadow_mode: u32,
    precomputed_shadow_bins: u32,
    precomputed_shadow_resolution: u32,
}

#[derive(Clone)]
struct DecodedPreviewAsset {
    name: String,
    frames: Vec<Vec<[u8; 4]>>,
    delays_ms: Vec<u32>,
    width: u32,
    height: u32,
}

struct PendingRouteState {
    search: String,
    preview_name: String,
}

enum PendingPreviewAsset {
    Clear,
    Error(String),
    Replace(DecodedPreviewAsset),
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

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            gallery_canvas_scale: 1.0,
            preview_canvas_scale: 1.0,
            preview_max_dim: 320,
            preview_render_scale: 2.0,
            display_pixelated: false,
            overlay_filter: false,
            ambient_light_tint: 0.67,
            ambient_light_brightness: 0.11,
            shadow_strength: 1.0,
            shadow_depth_threshold: 0.0,
            shadow_start_dist: 0.1,
            shadow_step_growth: 1.20,
            shadow_max: 1.0,
            shadow_jitter_spread: 0.35,
            shadow_max_depth_delta: 0.5,
            shadow_bbox_padding: 0.1,
            shadow_steps: 32,
            shadow_empty_depth_mode: 0,
            contact_shadows: true,
            contact_shadow_depth_threshold: 0.003,
            contact_shadow_start_dist: 0.75,
            contact_shadow_step_dist: 0.85,
            contact_shadow_max_dist: 12.0,
            contact_shadow_max_depth_delta: 0.1,
            contact_shadow_jitter_spread: 0.08,
            contact_shadow_steps: 16,
            shadow_mode: 1,
            precomputed_shadow_bins: 96,
            precomputed_shadow_resolution: 256,
        }
    }
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

thread_local! {
    static TRANSFER_TUNING: RefCell<TransferTuning> = RefCell::new(TransferTuning::default());
    static PERF_TOGGLES: RefCell<PerfToggles> = RefCell::new(PerfToggles::default());
    static RENDER_CONFIG: RefCell<RenderConfig> = RefCell::new(RenderConfig::default());
    static PENDING_GALLERY_ENTRIES: RefCell<Option<Vec<String>>> = RefCell::new(None);
    static PENDING_PREVIEW_ASSET: RefCell<Option<PendingPreviewAsset>> = RefCell::new(None);
    static DECODED_PREVIEW_CACHE: RefCell<DecodedPreviewCache> = RefCell::new(DecodedPreviewCache::new(12));
    static PENDING_ROUTE_STATE: RefCell<Option<PendingRouteState>> = RefCell::new(None);
    static CURRENT_EMOJI_NAME: RefCell<String> = RefCell::new(String::new());
    static CURRENT_PREVIEW_EMOJI_NAME: RefCell<String> = RefCell::new(String::new());
    static CURRENT_PREVIEW_NEIGHBORS: RefCell<(String, String)> = RefCell::new((String::new(), String::new()));
    static CURRENT_SEARCH_QUERY: RefCell<String> = RefCell::new(String::new());
    static PENDING_HOSTED_AUTH_STATE: RefCell<Option<gallery::HostedAuthState>> = RefCell::new(None);
    static LOGIN_REQUEST_NONCE: RefCell<u32> = const { RefCell::new(0) };
    static PENDING_SETTINGS_TOGGLE: RefCell<bool> = const { RefCell::new(false) };
    static SIGN_OUT_REQUEST_NONCE: RefCell<u32> = const { RefCell::new(0) };
}

struct DecodedPreviewCache {
    capacity: usize,
    entries: HashMap<String, DecodedPreviewAsset>,
    order: VecDeque<String>,
}

impl DecodedPreviewCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, name: &str) -> Option<DecodedPreviewAsset> {
        let asset = self.entries.get(name)?.clone();
        self.touch(name);
        Some(asset)
    }

    fn insert(&mut self, asset: DecodedPreviewAsset) {
        let name = asset.name.clone();
        self.entries.insert(name.clone(), asset);
        self.touch(&name);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.order.iter().any(|name| name == &oldest) {
                continue;
            }
            self.entries.remove(&oldest);
        }
    }

    fn touch(&mut self, name: &str) {
        self.order.push_back(name.to_owned());
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[wasm_bindgen]
pub fn set_transfer_tuning(linear_gain: f32, gamma: f32, lift: f32, saturation: f32) {
    TRANSFER_TUNING.with(|t| {
        *t.borrow_mut() = TransferTuning {
            linear_gain,
            gamma,
            lift,
            saturation,
        };
    });
}

#[wasm_bindgen]
pub fn set_perf_toggles(
    crt_enabled: bool,
    transfer_enabled: bool,
    overlay_filter_enabled: bool,
    billboard_enabled: bool,
) {
    PERF_TOGGLES.with(|t| {
        *t.borrow_mut() = PerfToggles {
            crt: crt_enabled,
            transfer: transfer_enabled,
            overlay_filter: overlay_filter_enabled,
            billboard: billboard_enabled,
        };
    });
}

#[wasm_bindgen]
pub fn set_render_config(
    gallery_canvas_scale: f32,
    preview_canvas_scale: f32,
    preview_max_dim: u32,
    preview_render_scale: f32,
    display_pixelated: bool,
    overlay_filter_enabled: bool,
) {
    RENDER_CONFIG.with(|cfg| {
        let current = *cfg.borrow();
        *cfg.borrow_mut() = RenderConfig {
            gallery_canvas_scale: gallery_canvas_scale.clamp(0.25, 2.0),
            preview_canvas_scale: preview_canvas_scale.clamp(0.25, 2.0),
            preview_max_dim: preview_max_dim.max(1),
            preview_render_scale: preview_render_scale.clamp(0.5, 4.0),
            display_pixelated,
            overlay_filter: overlay_filter_enabled,
            ..current
        };
    });
}

#[wasm_bindgen]
pub fn set_ambient_light_config(ambient_light_tint: f32, ambient_light_brightness: f32) {
    RENDER_CONFIG.with(|cfg| {
        let mut current = *cfg.borrow();
        current.ambient_light_tint = ambient_light_tint.clamp(0.0, 1.0);
        current.ambient_light_brightness = ambient_light_brightness.clamp(0.0, 1.0);
        *cfg.borrow_mut() = current;
    });
}

#[wasm_bindgen]
pub fn set_shadow_render_config(
    shadow_mode: u32,
    shadow_strength: f32,
    shadow_max: f32,
    contact_shadows: bool,
    precomputed_shadow_bins: u32,
    precomputed_shadow_resolution: u32,
) {
    RENDER_CONFIG.with(|cfg| {
        let mut current = *cfg.borrow();
        current.shadow_mode = shadow_mode.min(2);
        current.shadow_strength = shadow_strength.clamp(0.0, 1.0);
        current.shadow_max = shadow_max.clamp(0.0, 1.0);
        current.contact_shadows = contact_shadows;
        current.precomputed_shadow_bins = precomputed_shadow_bins.clamp(8, 256);
        current.precomputed_shadow_resolution = precomputed_shadow_resolution.clamp(32, 1024);
        *cfg.borrow_mut() = current;
    });
}

#[wasm_bindgen]
pub fn set_gallery_entries(entries_text: String) {
    let entries = entries_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    PENDING_GALLERY_ENTRIES.with(|pending| {
        *pending.borrow_mut() = Some(entries);
    });
}

#[wasm_bindgen]
pub fn clear_active_emoji_texture() {
    PENDING_PREVIEW_ASSET.with(|pending| {
        *pending.borrow_mut() = Some(PendingPreviewAsset::Clear);
    });
}

#[wasm_bindgen]
pub fn clear_decoded_emoji_texture_cache() {
    DECODED_PREVIEW_CACHE.with(|cache| cache.borrow_mut().clear());
}

#[wasm_bindgen]
pub fn set_active_emoji_texture_error(name: String) {
    PENDING_PREVIEW_ASSET.with(|pending| {
        *pending.borrow_mut() = Some(PendingPreviewAsset::Error(name));
    });
}

#[wasm_bindgen]
pub fn set_active_emoji_texture_bytes(name: String, bytes: Vec<u8>) -> bool {
    let Some(asset) = decode_preview_asset(name, &bytes) else {
        return false;
    };
    DECODED_PREVIEW_CACHE.with(|cache| {
        cache.borrow_mut().insert(asset.clone());
    });
    PENDING_PREVIEW_ASSET.with(|pending| {
        *pending.borrow_mut() = Some(PendingPreviewAsset::Replace(asset));
    });
    true
}

#[wasm_bindgen]
pub fn preload_emoji_texture_bytes(name: String, bytes: Vec<u8>) -> bool {
    let Some(asset) = decode_preview_asset(name, &bytes) else {
        return false;
    };
    DECODED_PREVIEW_CACHE.with(|cache| {
        cache.borrow_mut().insert(asset);
    });
    true
}

#[wasm_bindgen]
pub fn set_active_emoji_texture_from_cache(name: String) -> bool {
    let asset = DECODED_PREVIEW_CACHE.with(|cache| cache.borrow_mut().get(&name));
    let Some(asset) = asset else {
        return false;
    };
    PENDING_PREVIEW_ASSET.with(|pending| {
        *pending.borrow_mut() = Some(PendingPreviewAsset::Replace(asset));
    });
    true
}

fn decode_preview_asset(name: String, bytes: &[u8]) -> Option<DecodedPreviewAsset> {
    let (frames, delays_ms, width, height) = decode_emoji_frames(bytes)?;
    Some(DecodedPreviewAsset {
        name,
        frames,
        delays_ms,
        width,
        height,
    })
}

#[wasm_bindgen]
pub fn current_emoji_name() -> String {
    CURRENT_EMOJI_NAME.with(|name| name.borrow().clone())
}

#[wasm_bindgen]
pub fn current_preview_emoji_name() -> String {
    CURRENT_PREVIEW_EMOJI_NAME.with(|name| name.borrow().clone())
}

#[wasm_bindgen]
pub fn current_search_query() -> String {
    CURRENT_SEARCH_QUERY.with(|query| query.borrow().clone())
}

#[wasm_bindgen]
pub fn previous_preview_emoji_name() -> String {
    CURRENT_PREVIEW_NEIGHBORS.with(|neighbors| neighbors.borrow().0.clone())
}

#[wasm_bindgen]
pub fn next_preview_emoji_name() -> String {
    CURRENT_PREVIEW_NEIGHBORS.with(|neighbors| neighbors.borrow().1.clone())
}

#[wasm_bindgen]
pub fn set_route_state(search: String, preview_name: String) {
    PENDING_ROUTE_STATE.with(|pending| {
        *pending.borrow_mut() = Some(PendingRouteState {
            search,
            preview_name,
        });
    });
}

#[wasm_bindgen]
pub fn set_hosted_auth_state(
    status: String,
    workspace: String,
    hint: String,
    signed_in: bool,
    busy: bool,
    auth_configured: bool,
    catalog_ready: bool,
) {
    let auth_prompt = if !signed_in && !busy && auth_configured {
        gallery::HostedAuthPrompt::OpenLogin
    } else {
        gallery::HostedAuthPrompt::None
    };
    PENDING_HOSTED_AUTH_STATE.with(|pending| {
        *pending.borrow_mut() = Some(gallery::HostedAuthState {
            status,
            workspace,
            hint,
            signed_in,
            busy,
            auth_configured,
            catalog_ready,
            auth_prompt,
        });
    });
}

#[wasm_bindgen]
pub fn login_request_nonce() -> u32 {
    LOGIN_REQUEST_NONCE.with(|nonce| *nonce.borrow())
}

#[wasm_bindgen]
pub fn toggle_settings_menu() {
    PENDING_SETTINGS_TOGGLE.with(|pending| {
        *pending.borrow_mut() = true;
    });
}

#[wasm_bindgen]
pub fn sign_out_request_nonce() -> u32 {
    SIGN_OUT_REQUEST_NONCE.with(|nonce| *nonce.borrow())
}

#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    wasm_bindgen_futures::spawn_local(run());
}

fn debug_log(message: &str) {
    web_sys::console::log_1(&format!("[ultramoji-viewer-4d-rs] {message}").into());
}

fn required_limits_for_adapter(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let adapter_limits = adapter.limits();
    let base_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    base_limits
        .using_resolution(adapter_limits.clone())
        .using_alignment(adapter_limits)
}

async fn request_surface_adapter(
    canvas: &HtmlCanvasElement,
    backends: wgpu::Backends,
    power_preference: wgpu::PowerPreference,
) -> anyhow::Result<Option<(wgpu::Surface<'static>, wgpu::Adapter)>> {
    let instance_desc = wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    };
    let instance = wgpu::util::new_instance_with_webgpu_detection(&instance_desc).await;
    let surface = instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        })
        .await;
    Ok(adapter.map(|adapter| (surface, adapter)))
}

fn show_startup_error(message: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };

    let overlay = document.get_element_by_id("loading").unwrap_or_else(|| {
        let element = document
            .create_element("div")
            .expect("failed to create startup error overlay");
        element
            .set_attribute("id", "loading")
            .expect("failed to set startup error overlay id");
        body.append_child(&element)
            .expect("failed to append startup error overlay");
        element
    });

    overlay.set_text_content(Some(message));
    let _ = overlay.set_attribute(
        "style",
        concat!(
            "position:absolute;",
            "inset:0;",
            "display:flex;",
            "align-items:center;",
            "justify-content:center;",
            "padding:24px;",
            "background:#0c121c;",
            "color:#ff8a65;",
            "font:16px/1.5 monospace;",
            "text-align:left;",
            "white-space:pre-wrap;",
            "z-index:10;"
        ),
    );
}

async fn run() {
    let app = match App::init().await {
        Ok(app) => app,
        Err(err) => {
            web_sys::console::error_1(&format!("init failed: {err}").into());
            show_startup_error(&format!(
                "Browser GPU initialization failed.\n\
                 {err}\n\n\
                 WebGPU was attempted first, with WebGL2 fallback when available.\n\
                 If you want to force Chromium's Linux WebGPU path, relaunch with:\n\
                 chromium --enable-unsafe-webgpu --ignore-gpu-blocklist \\\n\
                   --enable-features=Vulkan,UseSkiaRenderer --use-angle=vulkan"
            ));
            return;
        }
    };

    let app = std::rc::Rc::new(std::cell::RefCell::new(app));
    let window = web_sys::window().unwrap();

    {
        let app = app.clone();
        let keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
            move |event: web_sys::KeyboardEvent| {
                let action = match event.key().as_str() {
                    "ArrowUp" => Some(gallery::KeyAction::Up),
                    "ArrowDown" => Some(gallery::KeyAction::Down),
                    "ArrowLeft" => Some(gallery::KeyAction::Left),
                    "ArrowRight" => Some(gallery::KeyAction::Right),
                    "PageUp" => Some(gallery::KeyAction::PageUp),
                    "PageDown" => Some(gallery::KeyAction::PageDown),
                    "F2" => Some(gallery::KeyAction::F2),
                    "F8" => Some(gallery::KeyAction::F8),
                    "Enter" => Some(gallery::KeyAction::Enter),
                    "Escape" => Some(gallery::KeyAction::Escape),
                    "Backspace" if event.ctrl_key() || event.alt_key() => {
                        Some(gallery::KeyAction::BackspaceWord)
                    }
                    "Backspace" => Some(gallery::KeyAction::Backspace),
                    key if key.len() == 1 => {
                        let ch = key.chars().next().unwrap();
                        if ch.is_ascii_graphic() || ch == ' ' {
                            Some(gallery::KeyAction::Char(ch))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    event.prevent_default();
                    if let Ok(mut app) = app.try_borrow_mut() {
                        app.handle_key(action);
                    }
                }
            },
        ));
        window
            .add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())
            .unwrap();
        keydown.forget();
    }

    {
        let app_ref = app.clone();
        let canvas = app.borrow().canvas.clone();
        let mousemove =
            Closure::<dyn FnMut(MouseEvent)>::wrap(Box::new(move |event: MouseEvent| {
                if let Ok(mut app) = app_ref.try_borrow_mut() {
                    app.handle_mouse_move(event);
                }
            }));
        canvas
            .add_event_listener_with_callback("mousemove", mousemove.as_ref().unchecked_ref())
            .unwrap();
        mousemove.forget();
    }

    {
        let app_ref = app.clone();
        let canvas = app.borrow().canvas.clone();
        let mouseleave = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |_| {
            if let Ok(mut app) = app_ref.try_borrow_mut() {
                app.handle_mouse_leave();
            }
        }));
        canvas
            .add_event_listener_with_callback("mouseleave", mouseleave.as_ref().unchecked_ref())
            .unwrap();
        mouseleave.forget();
    }

    {
        let app_ref = app.clone();
        let canvas = app.borrow().canvas.clone();
        let mousedown =
            Closure::<dyn FnMut(MouseEvent)>::wrap(Box::new(move |event: MouseEvent| {
                event.prevent_default();
                if let Ok(mut app) = app_ref.try_borrow_mut() {
                    app.handle_mouse_down(event);
                }
            }));
        canvas
            .add_event_listener_with_callback("mousedown", mousedown.as_ref().unchecked_ref())
            .unwrap();
        mousedown.forget();
    }

    {
        let app = app.clone();
        let mouseup = Closure::<dyn FnMut(MouseEvent)>::wrap(Box::new(move |event: MouseEvent| {
            if let Ok(mut app) = app.try_borrow_mut() {
                app.handle_mouse_up(event);
            }
        }));
        window
            .add_event_listener_with_callback("mouseup", mouseup.as_ref().unchecked_ref())
            .unwrap();
        mouseup.forget();
    }

    {
        let app_ref = app.clone();
        let canvas = app.borrow().canvas.clone();
        let wheel = Closure::<dyn FnMut(WheelEvent)>::wrap(Box::new(move |event: WheelEvent| {
            event.prevent_default();
            if let Ok(mut app) = app_ref.try_borrow_mut() {
                app.handle_wheel(event);
            }
        }));
        canvas
            .add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())
            .unwrap();
        wheel.forget();
    }

    let cb: std::rc::Rc<std::cell::RefCell<Option<Closure<dyn FnMut()>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let cb_clone = cb.clone();

    *cb_clone.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        {
            let mut app = app.borrow_mut();
            if let Err(err) = app.frame() {
                web_sys::console::error_1(&format!("frame error: {err}").into());
                return;
            }
        }
        window
            .request_animation_frame(cb.borrow().as_ref().unwrap().as_ref().unchecked_ref())
            .unwrap();
    }) as Box<dyn FnMut()>));

    let window2 = web_sys::window().unwrap();
    window2
        .request_animation_frame(cb_clone.borrow().as_ref().unwrap().as_ref().unchecked_ref())
        .unwrap();
}

struct App {
    renderer: GpuRenderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    canvas: HtmlCanvasElement,
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
    overlay_sampler_linear: wgpu::Sampler,
    overlay_sampler_nearest: wgpu::Sampler,
    start_time: f64,
    placeholder_pixels: Vec<[u8; 4]>,
    placeholder_w: u32,
    placeholder_h: u32,
    preview_asset_name: Option<String>,
    preview_error_name: Option<String>,
    preview_asset_revision: u64,
    preview_frames: Vec<Vec<[u8; 4]>>,
    preview_frame_delays_ms: Vec<u32>,
    preview_w: u32,
    preview_h: u32,
    gallery: gallery::Gallery,
    terminal_renderer: TerminalRenderer,
    terminal_grid: TerminalGrid,
    terminal_dirty: bool,
    billboard_sampler_linear: wgpu::Sampler,
    billboard_sampler_nearest: wgpu::Sampler,
    placeholder_billboard_view: wgpu::TextureView,
    egui_ctx: egui::Context,
    egui_renderer: EguiRenderer,
    egui_pointer_pos: Option<egui::Pos2>,
    egui_pointer_moved: bool,
    egui_pointer_down: bool,
    egui_pointer_pressed: bool,
    egui_pointer_released: bool,
    egui_scroll_delta: egui::Vec2,
    egui_cached_paint_jobs: Vec<egui::ClippedPrimitive>,
    egui_buffers_dirty: bool,
    egui_last_screen_size: [u32; 2],
    egui_last_pixels_per_point: f32,
    last_time_secs: f64,
    smoothed_fps: f32,
    last_fps_label: u32,
    last_blink_on: bool,
    last_preview_overlay_visible: bool,
    preview_scene_time_origin_secs: f64,
    last_preview_reset_nonce: u32,
    preview_manual_rotation: Option<f32>,
    preview_dragging: bool,
    preview_drag_last_x: f32,
    preview_drag_last_time_secs: f64,
    preview_drag_last_movement_time_secs: f64,
    preview_drag_anchor_rotation: f32,
    preview_rotation_velocity: f32,
    last_billboard_canvas_rect: [f32; 4],
    smoothed_frame_cpu_ms: f32,
    smoothed_frame_interval_ms: f32,
    smoothed_surface_acquire_ms: f32,
    smoothed_terminal_ms: f32,
    smoothed_screen_ms: f32,
    smoothed_scene_ms: f32,
    smoothed_egui_ms: f32,
    smoothed_composite_ms: f32,
    egui_paint_jobs: u32,
    egui_textures_delta: u32,
    frame_counter: u32,
    last_effective_dpr: f64,
    settings_visible: bool,
    sign_out_request_nonce: u32,
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

impl App {
    fn apply_pending_runtime_updates(&mut self) {
        let mut gallery_updated = false;
        PENDING_HOSTED_AUTH_STATE.with(|pending| {
            if let Some(auth) = pending.borrow_mut().take() {
                self.gallery.set_hosted_auth_state(auth);
                gallery_updated = true;
            }
        });
        PENDING_GALLERY_ENTRIES.with(|pending| {
            if let Some(entries) = pending.borrow_mut().take() {
                self.gallery.set_entries(entries);
                gallery_updated = true;
            }
        });
        PENDING_ROUTE_STATE.with(|pending| {
            if let Some(route) = pending.borrow_mut().take() {
                if self
                    .gallery
                    .apply_route_state(route.search, route.preview_name)
                {
                    gallery_updated = true;
                }
            }
        });
        if gallery_updated {
            self.terminal_dirty = true;
            self.screen_dirty = true;
        }
        PENDING_SETTINGS_TOGGLE.with(|pending| {
            if *pending.borrow() {
                *pending.borrow_mut() = false;
            }
        });

        let current_name = self.gallery.current_entry_name().map(str::to_owned);
        let previous_name = CURRENT_EMOJI_NAME.with(|name| name.borrow().clone());
        if previous_name != current_name.clone().unwrap_or_default() {
            debug_log(&format!(
                "current_emoji_name changed: prev={previous_name:?} next={:?} previewing={} preview_mix={:.2}",
                current_name,
                self.gallery.is_previewing(),
                self.gallery.preview_mix()
            ));
        }
        if self.preview_asset_name.as_deref() != current_name.as_deref() {
            if self.preview_asset_name.is_some() {
                debug_log(&format!(
                    "clearing preview asset due to name mismatch: asset={:?} current={:?}",
                    self.preview_asset_name, current_name
                ));
            }
            self.preview_asset_name = None;
            self.preview_asset_revision = self.preview_asset_revision.wrapping_add(1);
            self.preview_frames.clear();
            self.preview_frame_delays_ms.clear();
            self.preview_w = 0;
            self.preview_h = 0;
        }
        if self.preview_error_name.as_deref() != current_name.as_deref() {
            self.preview_error_name = None;
        }
        PENDING_PREVIEW_ASSET.with(|pending| {
            if let Some(update) = pending.borrow_mut().take() {
                match update {
                    PendingPreviewAsset::Clear => {
                        debug_log("received PendingPreviewAsset::Clear");
                        self.preview_asset_name = None;
                        self.preview_error_name = None;
                        self.preview_asset_revision = self.preview_asset_revision.wrapping_add(1);
                        self.preview_frames.clear();
                        self.preview_frame_delays_ms.clear();
                        self.preview_w = 0;
                        self.preview_h = 0;
                    }
                    PendingPreviewAsset::Error(name) => {
                        if current_name.as_deref() == Some(name.as_str()) {
                            debug_log(&format!("accepting preview load error: name={name}"));
                            self.preview_asset_name = None;
                            self.preview_error_name = Some(name);
                            self.preview_asset_revision = self.preview_asset_revision.wrapping_add(1);
                            self.preview_frames.clear();
                            self.preview_frame_delays_ms.clear();
                            self.preview_w = 0;
                            self.preview_h = 0;
                        } else {
                            debug_log(&format!(
                                "discarding preview load error due to selection mismatch: asset={} current={:?}",
                                name,
                                current_name
                            ));
                        }
                    }
                    PendingPreviewAsset::Replace(asset) => {
                        if current_name.as_deref() == Some(asset.name.as_str()) {
                            debug_log(&format!(
                                "accepting preview asset: name={} frames={} size={}x{}",
                                asset.name,
                                asset.frames.len(),
                                asset.width,
                                asset.height
                            ));
                            self.preview_asset_name = Some(asset.name);
                            self.preview_error_name = None;
                            self.preview_asset_revision = self.preview_asset_revision.wrapping_add(1);
                            self.preview_frames = asset.frames;
                            self.preview_frame_delays_ms = asset.delays_ms;
                            self.preview_w = asset.width;
                            self.preview_h = asset.height;
                        } else {
                            debug_log(&format!(
                                "discarding preview asset due to selection mismatch: asset={} current={:?}",
                                asset.name,
                                current_name
                            ));
                        }
                    }
                }
            }
        });
        let preview_loading = self.gallery.is_previewing()
            && self.preview_error_name.as_deref() != current_name.as_deref()
            && (self.preview_asset_name.as_deref() != current_name.as_deref()
                || self.preview_frames.is_empty()
                || self.preview_w == 0
                || self.preview_h == 0);
        self.gallery.set_channel_switch_loading(preview_loading);
        if self
            .gallery
            .set_preview_error(self.preview_error_name.as_deref() == current_name.as_deref())
        {
            self.terminal_dirty = true;
            self.screen_dirty = true;
        }
        CURRENT_EMOJI_NAME.with(|name| {
            *name.borrow_mut() = current_name.unwrap_or_default();
        });
        CURRENT_PREVIEW_EMOJI_NAME.with(|name| {
            *name.borrow_mut() = self
                .gallery
                .preview_entry_name()
                .map(str::to_owned)
                .unwrap_or_default();
        });
        CURRENT_PREVIEW_NEIGHBORS.with(|neighbors| {
            *neighbors.borrow_mut() = (
                self.gallery
                    .previous_preview_entry_name()
                    .map(str::to_owned)
                    .unwrap_or_default(),
                self.gallery
                    .next_preview_entry_name()
                    .map(str::to_owned)
                    .unwrap_or_default(),
            );
        });
        CURRENT_SEARCH_QUERY.with(|query| {
            *query.borrow_mut() = self.gallery.search_query().to_owned();
        });
        LOGIN_REQUEST_NONCE.with(|nonce| {
            *nonce.borrow_mut() = self.gallery.login_request_nonce();
        });
        SIGN_OUT_REQUEST_NONCE.with(|nonce| {
            *nonce.borrow_mut() = self.gallery.sign_out_request_nonce();
        });
    }

    fn reset_preview_drag_state(&mut self) {
        self.preview_manual_rotation = None;
        self.preview_dragging = false;
        self.preview_drag_last_x = 0.0;
        self.preview_drag_last_time_secs = 0.0;
        self.preview_drag_last_movement_time_secs = 0.0;
        self.preview_drag_anchor_rotation = 0.0;
        self.preview_rotation_velocity = 0.0;
    }

    fn preview_scene_time_secs_at(&self, time_secs: f64) -> f64 {
        if self.gallery.is_previewing() {
            (time_secs - self.preview_scene_time_origin_secs).max(0.0)
        } else {
            time_secs
        }
    }

    fn current_preview_scene_time_secs(&self) -> f64 {
        let now = web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now())
            .unwrap_or(0.0);
        self.preview_scene_time_secs_at((now - self.start_time) / 1000.0)
    }

    fn automatic_preview_rotation(scene_time_secs: f64) -> f32 {
        let phase = scene_time_secs * 0.8;
        (phase - 0.6 * phase.sin()) as f32
    }

    fn event_time_secs(event: &MouseEvent) -> f64 {
        event.time_stamp() / 1000.0
    }

    fn decay_preview_rotation_velocity(&mut self, dt_secs: f32) {
        if self.preview_rotation_velocity.abs() <= PREVIEW_DRAG_MIN_VELOCITY {
            self.preview_rotation_velocity = 0.0;
            return;
        }
        self.preview_rotation_velocity *= (-PREVIEW_DRAG_MOMENTUM_DECAY_PER_SEC * dt_secs).exp();
        if self.preview_rotation_velocity.abs() <= PREVIEW_DRAG_MIN_VELOCITY {
            self.preview_rotation_velocity = 0.0;
        }
    }

    fn record_preview_drag_sample(&mut self, surface_x: f32, event_time_secs: f64) {
        let dx = surface_x - self.preview_drag_last_x;
        let dt_secs = (event_time_secs - self.preview_drag_last_time_secs).max(0.001);
        self.preview_drag_last_x = surface_x;
        self.preview_drag_last_time_secs = event_time_secs;
        if dx.abs() <= f32::EPSILON {
            return;
        }

        let rotation_delta = dx * PREVIEW_DRAG_ROTATION_PER_PIXEL;
        let rotation = self
            .preview_manual_rotation
            .unwrap_or(self.preview_drag_anchor_rotation);
        self.preview_manual_rotation = Some(rotation + rotation_delta);

        let sample_velocity = rotation_delta / dt_secs as f32;
        self.preview_rotation_velocity = if self.preview_rotation_velocity.abs() <= f32::EPSILON {
            sample_velocity
        } else {
            self.preview_rotation_velocity
                + (sample_velocity - self.preview_rotation_velocity) * PREVIEW_DRAG_VELOCITY_BLEND
        };
        self.preview_drag_last_movement_time_secs = event_time_secs;
    }

    fn pointer_surface_pos(&self, event: &MouseEvent) -> [f32; 2] {
        let client_w = self.canvas.client_width().max(1) as f32;
        let client_h = self.canvas.client_height().max(1) as f32;
        [
            event.offset_x() as f32 / client_w * self.config.width.max(1) as f32,
            event.offset_y() as f32 / client_h * self.config.height.max(1) as f32,
        ]
    }

    fn point_in_rect(point: [f32; 2], rect: [f32; 4]) -> bool {
        rect[2] > 0.0
            && rect[3] > 0.0
            && point[0] >= rect[0]
            && point[0] <= rect[0] + rect[2]
            && point[1] >= rect[1]
            && point[1] <= rect[1] + rect[3]
    }

    fn current_preview_frame_index(&self, scene_time_secs: f64) -> Option<usize> {
        if self.preview_frames.is_empty() || self.preview_w == 0 || self.preview_h == 0 {
            return None;
        }
        if self.preview_frames.len() == 1 {
            return Some(0);
        }

        let total_duration_ms: u64 = self
            .preview_frame_delays_ms
            .iter()
            .copied()
            .map(|delay| delay.max(16) as u64)
            .sum();
        if total_duration_ms == 0 {
            return Some(0);
        }

        let mut elapsed_ms = ((scene_time_secs * 1000.0).max(0.0)) as u64 % total_duration_ms;
        for (index, delay_ms) in self.preview_frame_delays_ms.iter().copied().enumerate() {
            let duration_ms = delay_ms.max(16) as u64;
            if elapsed_ms < duration_ms {
                return Some(index);
            }
            elapsed_ms = elapsed_ms.saturating_sub(duration_ms);
        }
        Some(self.preview_frames.len().saturating_sub(1))
    }

    async fn init() -> anyhow::Result<Self> {
        let window = web_sys::window().unwrap();
        let document = window.document().unwrap();
        let canvas: HtmlCanvasElement = document
            .get_element_by_id("emoji-canvas")
            .expect("no #emoji-canvas element")
            .dyn_into()
            .unwrap();
        let render_config = RENDER_CONFIG.with(|cfg| *cfg.borrow());

        let (surface, adapter) = if let Some(pair) = request_surface_adapter(
            &canvas,
            wgpu::Backends::BROWSER_WEBGPU,
            wgpu::PowerPreference::HighPerformance,
        )
        .await?
        {
            pair
        } else if let Some(pair) =
            request_surface_adapter(&canvas, wgpu::Backends::GL, wgpu::PowerPreference::LowPower)
                .await?
        {
            pair
        } else {
            return Err(anyhow::anyhow!("no WebGPU or WebGL2 adapter"));
        };

        let adapter_info = adapter.get_info();
        let downlevel_caps = adapter.get_downlevel_capabilities();
        debug_log(&format!(
            "adapter selected: backend={:?} name={} webgpu_compliant={}",
            adapter_info.backend,
            adapter_info.name,
            downlevel_caps.is_webgpu_compliant()
        ));

        let required_limits = required_limits_for_adapter(&adapter);
        let features = wgpu::Features::empty();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("emoji_web"),
                    required_features: features,
                    required_limits,
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
        let independent_blend_supported = downlevel_caps
            .flags
            .contains(wgpu::DownlevelFlags::INDEPENDENT_BLEND);

        let renderer = GpuRenderer::from_device_queue(
            device,
            queue,
            features,
            linear_depth_format,
            independent_blend_supported,
        )?;

        let caps = surface.get_capabilities(&adapter);
        let format = preferred_surface_format(&caps.formats);

        let dpr = window.device_pixel_ratio().max(0.1);
        let w = ((canvas.client_width().max(1) as f64)
            * dpr
            * render_config.gallery_canvas_scale as f64)
            .round()
            .max(1.0) as u32;
        let h = ((canvas.client_height().max(1) as f64)
            * dpr
            * render_config.gallery_canvas_scale as f64)
            .round()
            .max(1.0) as u32;
        canvas.set_width(w);
        canvas.set_height(h);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(renderer.device(), &config);
        let egui_ctx = egui::Context::default();
        let egui_renderer = EguiRenderer::new(renderer.device(), config.format, None, 1, false);

        let shader = renderer
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("composite_shader"),
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

        let composite_bind_group_layout =
            renderer
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("composite_bgl"),
                    entries: &[
                        bgl_texture(0),
                        bgl_sampler(1),
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
                        bgl_texture(3),
                        bgl_sampler(4),
                    ],
                });

        let composite_uniform_buffer = renderer.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite_uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline_layout =
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
                    layout: Some(&pipeline_layout),
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

        let overlay_sampler_linear = renderer.device().create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let overlay_sampler_nearest = renderer.device().create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let billboard_sampler_nearest =
            renderer.device().create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            });
        let billboard_sampler_linear = renderer.device().create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let terminal_renderer = TerminalRenderer::new(renderer.device(), renderer.queue())?;
        let terminal_grid = TerminalGrid::new();
        let (_placeholder_billboard_tex, placeholder_billboard_view) =
            create_rgba_texture(renderer.device(), 1, 1);
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
        let (placeholder_pixels, placeholder_w, placeholder_h) = demo_texture();
        let gallery = gallery::Gallery::with_entries(Vec::<String>::new());
        let start_time = web_sys::window().unwrap().performance().unwrap().now();

        Ok(Self {
            renderer,
            surface,
            config,
            canvas,
            screen_pipeline,
            screen_bind_group_layout,
            screen_bind_group,
            screen_overlay_filter: true,
            screen_uniform_buffer,
            screen_texture,
            screen_texture_view,
            screen_dirty: true,
            last_screen_transfer: TransferTuning::default(),
            last_screen_perf_toggles: PerfToggles::default(),
            last_screen_render_config: RenderConfig::default(),
            last_screen_preview_mix: -1.0,
            composite_pipeline,
            composite_bind_group_layout,
            composite_bind_group,
            composite_uses_billboard: false,
            composite_display_pixelated: false,
            composite_billboard_generation: 0,
            composite_uniform_buffer,
            overlay_sampler_linear,
            overlay_sampler_nearest,
            start_time,
            placeholder_pixels,
            placeholder_w,
            placeholder_h,
            preview_asset_name: None,
            preview_error_name: None,
            preview_asset_revision: 0,
            preview_frames: Vec::new(),
            preview_frame_delays_ms: Vec::new(),
            preview_w: 0,
            preview_h: 0,
            gallery,
            terminal_renderer,
            terminal_grid,
            terminal_dirty: true,
            billboard_sampler_linear,
            billboard_sampler_nearest,
            placeholder_billboard_view,
            egui_ctx,
            egui_renderer,
            egui_pointer_pos: None,
            egui_pointer_moved: false,
            egui_pointer_down: false,
            egui_pointer_pressed: false,
            egui_pointer_released: false,
            egui_scroll_delta: egui::Vec2::ZERO,
            egui_cached_paint_jobs: Vec::new(),
            egui_buffers_dirty: false,
            egui_last_screen_size: [0, 0],
            egui_last_pixels_per_point: 0.0,
            last_time_secs: 0.0,
            smoothed_fps: 60.0,
            last_fps_label: 60,
            last_blink_on: true,
            last_preview_overlay_visible: false,
            preview_scene_time_origin_secs: 0.0,
            last_preview_reset_nonce: 0,
            preview_manual_rotation: None,
            preview_dragging: false,
            preview_drag_last_x: 0.0,
            preview_drag_last_time_secs: 0.0,
            preview_drag_last_movement_time_secs: 0.0,
            preview_drag_anchor_rotation: 0.0,
            preview_rotation_velocity: 0.0,
            last_billboard_canvas_rect: [0.0; 4],
            smoothed_frame_cpu_ms: 0.0,
            smoothed_frame_interval_ms: 0.0,
            smoothed_surface_acquire_ms: 0.0,
            smoothed_terminal_ms: 0.0,
            smoothed_screen_ms: 0.0,
            smoothed_scene_ms: 0.0,
            smoothed_egui_ms: 0.0,
            smoothed_composite_ms: 0.0,
            egui_paint_jobs: 0,
            egui_textures_delta: 0,
            frame_counter: 0,
            last_effective_dpr: dpr,
            settings_visible: false,
            sign_out_request_nonce: 0,
        })
    }

    fn handle_key(&mut self, action: gallery::KeyAction) {
        let action_name = match &action {
            gallery::KeyAction::Up => "Up",
            gallery::KeyAction::Down => "Down",
            gallery::KeyAction::Left => "Left",
            gallery::KeyAction::Right => "Right",
            gallery::KeyAction::PageUp => "PageUp",
            gallery::KeyAction::PageDown => "PageDown",
            gallery::KeyAction::F2 => "F2",
            gallery::KeyAction::F8 => "F8",
            gallery::KeyAction::Enter => "Enter",
            gallery::KeyAction::Escape => "Escape",
            gallery::KeyAction::Char(_) => "Char",
            gallery::KeyAction::Backspace => "Backspace",
            gallery::KeyAction::BackspaceWord => "BackspaceWord",
        };
        if matches!(action, gallery::KeyAction::F8) {
            self.settings_visible = !self.settings_visible;
            self.egui_pointer_pressed = false;
            self.egui_pointer_released = false;
            self.egui_pointer_down = false;
            self.egui_scroll_delta = egui::Vec2::ZERO;
            self.egui_pointer_moved = true;
            self.egui_cached_paint_jobs.clear();
            self.egui_buffers_dirty = true;
            self.terminal_dirty = true;
            self.screen_dirty = true;
            return;
        }
        self.gallery.handle_key(action);
        if matches!(
            action,
            gallery::KeyAction::Enter
                | gallery::KeyAction::Escape
                | gallery::KeyAction::PageUp
                | gallery::KeyAction::PageDown
        ) {
            debug_log(&format!(
                "handle_key {action_name}: current={:?} previewing={} preview_mix={:.2}",
                self.gallery.current_entry_name(),
                self.gallery.is_previewing(),
                self.gallery.preview_mix()
            ));
        }
        self.terminal_dirty = true;
        self.screen_dirty = true;
    }

    fn handle_mouse_move(&mut self, event: MouseEvent) {
        let pos = egui::pos2(event.offset_x() as f32, event.offset_y() as f32);
        if self.egui_pointer_pos != Some(pos) {
            self.egui_pointer_pos = Some(pos);
            self.egui_pointer_moved = true;
        }
        if self.preview_dragging {
            event.prevent_default();
            let surface_pos = self.pointer_surface_pos(&event);
            let event_time_secs = Self::event_time_secs(&event);
            self.record_preview_drag_sample(surface_pos[0], event_time_secs);
        }
    }

    fn handle_mouse_leave(&mut self) {
        self.egui_pointer_pos = None;
        self.egui_pointer_moved = true;
    }

    fn handle_mouse_down(&mut self, event: MouseEvent) {
        self.egui_pointer_pos = Some(egui::pos2(event.offset_x() as f32, event.offset_y() as f32));
        self.egui_pointer_moved = true;
        self.egui_pointer_down = true;
        self.egui_pointer_pressed = true;
        if !self.settings_visible && event.button() == 0 && self.gallery.is_previewing() {
            let surface_pos = self.pointer_surface_pos(&event);
            if Self::point_in_rect(surface_pos, self.last_billboard_canvas_rect) {
                self.preview_dragging = true;
                self.preview_drag_last_x = surface_pos[0];
                self.preview_drag_last_time_secs = Self::event_time_secs(&event);
                self.preview_drag_last_movement_time_secs = self.preview_drag_last_time_secs;
                self.preview_rotation_velocity = 0.0;
                self.preview_drag_anchor_rotation =
                    self.preview_manual_rotation.unwrap_or_else(|| {
                        Self::automatic_preview_rotation(self.current_preview_scene_time_secs())
                    });
            }
        }
    }

    fn handle_mouse_up(&mut self, event: MouseEvent) {
        self.egui_pointer_pos = Some(egui::pos2(event.offset_x() as f32, event.offset_y() as f32));
        self.egui_pointer_moved = true;
        self.egui_pointer_down = false;
        self.egui_pointer_released = true;
        if self.preview_dragging {
            let surface_pos = self.pointer_surface_pos(&event);
            let event_time_secs = Self::event_time_secs(&event);
            self.record_preview_drag_sample(surface_pos[0], event_time_secs);
            let release_was_still = event_time_secs - self.preview_drag_last_movement_time_secs
                > PREVIEW_DRAG_RELEASE_MAX_STILL_SECS;
            if release_was_still
                || self.preview_rotation_velocity.abs() < PREVIEW_DRAG_RELEASE_MIN_VELOCITY
            {
                self.preview_rotation_velocity = 0.0;
            }
        }
        self.preview_dragging = false;
    }

    fn handle_wheel(&mut self, event: WheelEvent) {
        self.egui_scroll_delta += egui::vec2(-(event.delta_x() as f32), -(event.delta_y() as f32));
    }

    fn frame(&mut self) -> anyhow::Result<()> {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let perf = web_sys::window().unwrap().performance().unwrap();
        let now = perf.now();
        let frame_start = now;
        let elapsed_ms = now - self.start_time;
        let time_secs = elapsed_ms / 1000.0;
        let dt_secs = (time_secs - self.last_time_secs).max(0.0);
        self.last_time_secs = time_secs;
        self.apply_pending_runtime_updates();
        self.gallery.tick(dt_secs as f32);

        if dt_secs > 0.0 {
            let fps = (1.0 / dt_secs) as f32;
            self.smoothed_fps = self.smoothed_fps * 0.9 + fps * 0.1;
        }
        let fps_label = self.smoothed_fps.round().clamp(0.0, 999.0) as u32;
        if fps_label != self.last_fps_label {
            self.last_fps_label = fps_label;
        }

        let window = web_sys::window().unwrap();
        let effective_dpr = window.device_pixel_ratio().max(0.1);
        self.last_effective_dpr = effective_dpr;
        let client_w = self.canvas.client_width().max(1) as u32;
        let client_h = self.canvas.client_height().max(1) as u32;
        let mut transfer = TRANSFER_TUNING.with(|t| *t.borrow());
        let mut render_config = RENDER_CONFIG.with(|cfg| *cfg.borrow());
        let mut egui_events = Vec::new();
        if self.settings_visible {
            if self.egui_pointer_moved {
                if let Some(pos) = self.egui_pointer_pos {
                    egui_events.push(egui::Event::PointerMoved(pos));
                }
            } else if self.egui_pointer_pressed || self.egui_pointer_released {
                let pos = self.egui_pointer_pos.unwrap_or(egui::Pos2::ZERO);
                egui_events.push(egui::Event::PointerMoved(pos));
            }
            if self.egui_pointer_pressed {
                egui_events.push(egui::Event::PointerButton {
                    pos: self.egui_pointer_pos.unwrap_or(egui::Pos2::ZERO),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            if self.egui_pointer_released {
                egui_events.push(egui::Event::PointerButton {
                    pos: self.egui_pointer_pos.unwrap_or(egui::Pos2::ZERO),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            if self.egui_scroll_delta != egui::Vec2::ZERO {
                egui_events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: self.egui_scroll_delta,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        let egui_input_dirty = !egui_events.is_empty() || self.egui_pointer_down;
        let egui_raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(client_w as f32, client_h as f32),
            )),
            time: Some(time_secs),
            predicted_dt: dt_secs as f32,
            events: egui_events,
            ..Default::default()
        };
        let mut panel_actions = egui_panel::PanelActions::default();
        let mut egui_shapes = None;
        let mut egui_texture_free_ids = Vec::new();
        let egui_start = perf.now();
        let egui_periodic_refresh = self.frame_counter % 15 == 0;
        let run_egui = self.settings_visible
            && (self.egui_cached_paint_jobs.is_empty()
                || self.egui_buffers_dirty
                || egui_input_dirty
                || egui_periodic_refresh);
        if run_egui {
            let full_perf = egui_panel::PerfPanelData {
                smoothed_fps: self.smoothed_fps,
                smoothed_frame_cpu_ms: self.smoothed_frame_cpu_ms,
                smoothed_frame_interval_ms: self.smoothed_frame_interval_ms,
                smoothed_surface_acquire_ms: self.smoothed_surface_acquire_ms,
                smoothed_terminal_ms: self.smoothed_terminal_ms,
                smoothed_screen_ms: self.smoothed_screen_ms,
                smoothed_scene_ms: self.smoothed_scene_ms,
                smoothed_egui_ms: self.smoothed_egui_ms,
                smoothed_composite_ms: self.smoothed_composite_ms,
                window_width: client_w,
                window_height: client_h,
                surface_width: self.config.width,
                surface_height: self.config.height,
                terminal_width: self.terminal_renderer.pixel_width(),
                terminal_height: self.terminal_renderer.pixel_height(),
                scale_factor: effective_dpr as f32,
                preview_mix: self.gallery.preview_mix(),
                egui_paint_jobs: self.egui_paint_jobs,
                egui_textures_delta: self.egui_textures_delta,
                last_screen_redrew: self.screen_dirty,
                last_previewing: self.gallery.is_previewing(),
                last_uses_billboard: self.gallery.is_previewing()
                    && self.gallery.preview_mix() > 0.0,
                offscreen_stats: self.renderer.offscreen_perf_stats(),
            };
            let panel_response = self.egui_ctx.run(egui_raw_input, |ctx| {
                panel_actions = egui_panel::show_controls_panel(
                    ctx,
                    &mut transfer,
                    &mut render_config,
                    full_perf,
                );
            });
            let textures_delta_set_count = panel_response.textures_delta.set.len();
            let textures_delta_free_count = panel_response.textures_delta.free.len();
            for (id, delta) in &panel_response.textures_delta.set {
                self.egui_renderer.update_texture(
                    self.renderer.device(),
                    self.renderer.queue(),
                    *id,
                    delta,
                );
            }
            self.egui_textures_delta =
                (textures_delta_set_count + textures_delta_free_count) as u32;
            egui_texture_free_ids = panel_response.textures_delta.free;
            egui_shapes = Some(panel_response.shapes);
            self.smoothed_egui_ms =
                self.smoothed_egui_ms * 0.85 + ((perf.now() - egui_start) as f32) * 0.15;
        } else if !self.settings_visible {
            self.egui_paint_jobs = 0;
            self.egui_textures_delta = 0;
            self.egui_cached_paint_jobs.clear();
            self.egui_buffers_dirty = false;
            self.smoothed_egui_ms *= 0.85;
        } else {
            self.egui_textures_delta = 0;
            self.smoothed_egui_ms *= 0.85;
        }
        if panel_actions.sign_out_requested {
            self.sign_out_request_nonce = self.sign_out_request_nonce.wrapping_add(1);
        }
        TRANSFER_TUNING.with(|t| *t.borrow_mut() = transfer);
        RENDER_CONFIG.with(|cfg| *cfg.borrow_mut() = render_config);
        self.egui_pointer_pressed = false;
        self.egui_pointer_released = false;
        self.egui_pointer_moved = false;
        self.egui_scroll_delta = egui::Vec2::ZERO;
        let active_canvas_scale = if self.gallery.is_previewing() {
            render_config.preview_canvas_scale
        } else {
            render_config.gallery_canvas_scale
        };
        let w = ((client_w as f64) * effective_dpr * active_canvas_scale as f64)
            .round()
            .max(1.0) as u32;
        let h = ((client_h as f64) * effective_dpr * active_canvas_scale as f64)
            .round()
            .max(1.0) as u32;
        if w != self.config.width || h != self.config.height {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(self.renderer.device(), &self.config);
        }
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: effective_dpr as f32,
        };
        if self.settings_visible {
            let egui_screen_changed = self.egui_last_screen_size
                != screen_descriptor.size_in_pixels
                || (self.egui_last_pixels_per_point - screen_descriptor.pixels_per_point).abs()
                    > f32::EPSILON;
            if let Some(shapes) = egui_shapes {
                self.egui_cached_paint_jobs = self
                    .egui_ctx
                    .tessellate(shapes, screen_descriptor.pixels_per_point);
                self.egui_buffers_dirty = true;
                self.egui_last_screen_size = screen_descriptor.size_in_pixels;
                self.egui_last_pixels_per_point = screen_descriptor.pixels_per_point;
            } else if egui_screen_changed {
                self.egui_cached_paint_jobs.clear();
                self.egui_buffers_dirty = true;
                self.egui_last_screen_size = screen_descriptor.size_in_pixels;
                self.egui_last_pixels_per_point = screen_descriptor.pixels_per_point;
            }
            self.egui_paint_jobs = self.egui_cached_paint_jobs.len() as u32;
        } else {
            self.egui_paint_jobs = 0;
        }

        let blink_on = gallery::cursor_blink_on(time_secs);
        let preview_overlay_visible = gallery::show_preview_overlay(&self.gallery);
        if blink_on != self.last_blink_on
            || preview_overlay_visible != self.last_preview_overlay_visible
        {
            self.last_blink_on = blink_on;
            self.last_preview_overlay_visible = preview_overlay_visible;
            self.terminal_dirty = true;
            self.screen_dirty = true;
        }

        let terminal_redrew = self.terminal_dirty;
        let term_start = perf.now();
        if self.terminal_dirty {
            gallery::render_to_grid(&mut self.terminal_grid, &self.gallery, time_secs);
            self.terminal_renderer.render(
                self.renderer.device(),
                self.renderer.queue(),
                &self.terminal_grid,
            );
            self.terminal_dirty = false;
            self.screen_dirty = true;
        }
        let terminal_ms = (perf.now() - term_start) as f32;

        let previewing = self.gallery.is_previewing();
        let preview_error = previewing && self.gallery.preview_error();
        let preview_mix = self.gallery.preview_mix();
        let preview_reset_nonce = self.gallery.preview_reset_nonce();
        if previewing && preview_reset_nonce != self.last_preview_reset_nonce {
            self.preview_scene_time_origin_secs = time_secs;
            self.last_preview_reset_nonce = preview_reset_nonce;
            self.reset_preview_drag_state();
        }
        let preview_scene_time_secs = if previewing {
            self.preview_scene_time_secs_at(time_secs)
        } else {
            time_secs
        };
        if previewing && self.preview_manual_rotation.is_some() {
            if !self.preview_dragging
                && self.preview_rotation_velocity.abs() > PREVIEW_DRAG_MIN_VELOCITY
            {
                let rotation = self.preview_manual_rotation.unwrap();
                self.preview_manual_rotation =
                    Some(rotation + self.preview_rotation_velocity * dt_secs as f32);
                self.decay_preview_rotation_velocity(dt_secs as f32);
            } else if !self.preview_dragging {
                self.preview_rotation_velocity = 0.0;
            }
        }
        let terminal_cols = TERM_COLS as f32;
        let terminal_rows = TERM_ROWS as f32;
        let transfer = TRANSFER_TUNING.with(|t| *t.borrow());
        let perf_toggles = PERF_TOGGLES.with(|t| *t.borrow());
        if self.last_screen_transfer != transfer
            || self.last_screen_perf_toggles != perf_toggles
            || self.last_screen_render_config.overlay_filter != render_config.overlay_filter
            || (self.last_screen_preview_mix - preview_mix).abs() > 0.001
        {
            self.screen_dirty = true;
        }

        let overlay_w = self.terminal_renderer.pixel_width();
        let overlay_h = self.terminal_renderer.pixel_height();
        let mut frame_encoder =
            self.renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("web_frame_encoder"),
                });
        let scene_start = perf.now();
        let billboard_enabled = previewing && perf_toggles.billboard && !preview_error;
        let billboard_pixel_rect: [f32; 4] = if billboard_enabled {
            if let Some(cell_rect) = self.gallery.billboard_cell_rect(TERM_COLS, TERM_ROWS) {
                if overlay_w > 4 && overlay_h > 4 {
                    let native_w = cell_rect.width as f32;
                    let native_h = (cell_rect.height as f32) * 2.0;
                    let target_max_dim = render_config.preview_max_dim as f32;
                    let native_max = native_w.max(native_h).max(1.0);
                    let scale = target_max_dim / native_max;
                    let render_w = (native_w * scale).round().max(1.0) as u32;
                    let render_h = (native_h * scale).round().max(1.0) as u32;

                    let mut params = emoji_preview_scene_params();
                    params.sharpen = Some(0.1);
                    params.dither = Some(0.3);
                    params.vhs = Some(0.5);
                    params.jitter = Some(0.1);
                    params.supersample = render_config.preview_render_scale > 1.01;
                    params.render_scale = Some(render_config.preview_render_scale);
                    params.rotation = self.preview_manual_rotation;
                    params.ambient_light_tint = Some(render_config.ambient_light_tint);
                    params.ambient_light_brightness = Some(render_config.ambient_light_brightness);
                    params.ssao_strength = Some(render_config.shadow_strength);
                    params.ssao_depth_threshold = Some(render_config.shadow_depth_threshold);
                    params.ssao_start_dist = Some(render_config.shadow_start_dist);
                    params.ssao_step_growth = Some(render_config.shadow_step_growth);
                    params.ssao_max_shadow = Some(render_config.shadow_max);
                    params.ssao_jitter_spread = Some(render_config.shadow_jitter_spread);
                    params.ssao_max_depth_delta = Some(render_config.shadow_max_depth_delta);
                    params.ssao_bbox_padding = Some(render_config.shadow_bbox_padding);
                    params.ssao_steps = Some(render_config.shadow_steps);
                    params.ssao_empty_depth_mode = Some(render_config.shadow_empty_depth_mode);
                    params.contact_shadows = Some(render_config.contact_shadows);
                    params.contact_shadow_depth_threshold =
                        Some(render_config.contact_shadow_depth_threshold);
                    params.contact_shadow_start_dist =
                        Some(render_config.contact_shadow_start_dist);
                    params.contact_shadow_step_dist = Some(render_config.contact_shadow_step_dist);
                    params.contact_shadow_max_dist = Some(render_config.contact_shadow_max_dist);
                    params.contact_shadow_max_depth_delta =
                        Some(render_config.contact_shadow_max_depth_delta);
                    params.contact_shadow_jitter_spread =
                        Some(render_config.contact_shadow_jitter_spread);
                    params.contact_shadow_steps = Some(render_config.contact_shadow_steps);
                    params.shadow_mode = Some(render_config.shadow_mode);
                    params.precomputed_shadow_bins = Some(render_config.precomputed_shadow_bins);
                    params.precomputed_shadow_resolution =
                        Some(render_config.precomputed_shadow_resolution);
                    if let Some(index) = self.current_preview_frame_index(preview_scene_time_secs) {
                        self.renderer
                            .render_animated_frame_to_offscreen_params_into(
                                &mut frame_encoder,
                                self.preview_asset_revision,
                                &self.preview_frames,
                                index,
                                self.preview_w,
                                self.preview_h,
                                render_w,
                                render_h,
                                preview_scene_time_secs,
                                &params,
                            )?;
                    }
                }

                let cell_px_w = overlay_w as f32 / TERM_COLS as f32;
                let cell_px_h = overlay_h as f32 / TERM_ROWS as f32;
                [
                    cell_rect.x as f32 * cell_px_w,
                    cell_rect.y as f32 * cell_px_h,
                    cell_rect.width as f32 * cell_px_w,
                    cell_rect.height as f32 * cell_px_h,
                ]
            } else {
                [0.0; 4]
            }
        } else {
            [0.0; 4]
        };
        let scene_ms = (perf.now() - scene_start) as f32;

        let screen_redrew = self.screen_dirty;
        let mut screen_encoded = false;
        let screen_start = perf.now();
        if self.screen_dirty {
            let screen_uniforms = CompositeUniforms {
                output_size: [overlay_w as f32, overlay_h as f32],
                time_secs: time_secs as f32,
                preview_mix,
                terminal_rect: [0.0; 4],
                overlay_uv_rect: [0.0, 0.0, 1.0, 1.0],
                billboard_rect: [0.0; 4],
                terminal_grid: [
                    terminal_cols,
                    terminal_rows,
                    overlay_w as f32 / terminal_cols,
                    overlay_h as f32 / terminal_rows,
                ],
                transfer_tuning: [
                    transfer.linear_gain,
                    transfer.gamma,
                    transfer.lift,
                    transfer.saturation,
                ],
                perf_toggles: [
                    if perf_toggles.crt { 1.0 } else { 0.0 },
                    if perf_toggles.transfer { 1.0 } else { 0.0 },
                    if perf_toggles.overlay_filter && render_config.overlay_filter {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                ],
                channel_switch: [
                    self.gallery.channel_switch(),
                    self.gallery.channel_switch_dir(),
                    if preview_error { 1.0 } else { 0.0 },
                    0.0,
                ],
            };
            self.renderer.queue().write_buffer(
                &self.screen_uniform_buffer,
                0,
                bytemuck::bytes_of(&screen_uniforms),
            );
            self.ensure_screen_bind_group(render_config.overlay_filter);
            {
                let mut pass = frame_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
            screen_encoded = true;
        }
        let screen_ms = (perf.now() - screen_start) as f32;

        let term_aspect = overlay_w as f32 / overlay_h as f32;
        let canvas_aspect = w as f32 / h as f32;
        let (term_w, term_h) = if canvas_aspect > term_aspect {
            let th = h as f32;
            (th * term_aspect, th)
        } else {
            let tw = w as f32;
            (tw, tw / term_aspect)
        };
        let term_x = (w as f32 - term_w) * 0.5;
        let term_y = (h as f32 - term_h) * 0.5;
        let terminal_rect = [term_x, term_y, term_w, term_h];

        let sx = term_w / overlay_w as f32;
        let sy = term_h / overlay_h as f32;
        let billboard_canvas_rect = [
            term_x + billboard_pixel_rect[0] * sx,
            term_y + billboard_pixel_rect[1] * sy,
            billboard_pixel_rect[2] * sx,
            billboard_pixel_rect[3] * sy,
        ];
        self.last_billboard_canvas_rect = billboard_canvas_rect;

        let uniforms = CompositeUniforms {
            output_size: [w as f32, h as f32],
            time_secs: time_secs as f32,
            preview_mix,
            terminal_rect,
            overlay_uv_rect: [0.0, 0.0, 1.0, 1.0],
            billboard_rect: billboard_canvas_rect,
            terminal_grid: [
                terminal_cols,
                terminal_rows,
                term_w / terminal_cols,
                term_h / terminal_rows,
            ],
            transfer_tuning: [
                transfer.linear_gain,
                transfer.gamma,
                transfer.lift,
                transfer.saturation,
            ],
            perf_toggles: [
                if perf_toggles.crt { 1.0 } else { 0.0 },
                if perf_toggles.transfer { 1.0 } else { 0.0 },
                if perf_toggles.overlay_filter && render_config.overlay_filter {
                    1.0
                } else {
                    0.0
                },
                if billboard_enabled { 1.0 } else { 0.0 },
            ],
            channel_switch: [
                self.gallery.channel_switch(),
                self.gallery.channel_switch_dir(),
                if preview_error { 1.0 } else { 0.0 },
                0.0,
            ],
        };
        self.renderer.queue().write_buffer(
            &self.composite_uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let uses_billboard =
            billboard_enabled && preview_mix > 0.0 && billboard_canvas_rect[2] > 0.0;
        let billboard_generation = self.renderer.render_target_generation();
        self.ensure_composite_bind_group(
            uses_billboard,
            render_config.display_pixelated,
            billboard_generation,
        );

        let acquire_start = perf.now();
        let output = self.surface.get_current_texture()?;
        let acquire_ms = (perf.now() - acquire_start) as f32;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let composite_start = perf.now();
        let has_egui_paint = self.settings_visible && !self.egui_cached_paint_jobs.is_empty();
        if has_egui_paint && self.egui_buffers_dirty {
            self.egui_renderer.update_buffers(
                self.renderer.device(),
                self.renderer.queue(),
                &mut frame_encoder,
                &self.egui_cached_paint_jobs,
                &screen_descriptor,
            );
            self.egui_buffers_dirty = false;
        }
        {
            let mut pass = frame_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &self.composite_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        if has_egui_paint {
            let mut pass = frame_encoder
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
            self.egui_renderer
                .render(&mut pass, &self.egui_cached_paint_jobs, &screen_descriptor);
        }

        let submit_start = perf.now();
        self.renderer.queue().submit(Some(frame_encoder.finish()));
        let submit_ms = (perf.now() - submit_start) as f32;
        if screen_encoded {
            self.screen_dirty = false;
            self.last_screen_transfer = transfer;
            self.last_screen_perf_toggles = perf_toggles;
            self.last_screen_render_config = render_config;
            self.last_screen_preview_mix = preview_mix;
        }
        for id in &egui_texture_free_ids {
            self.egui_renderer.free_texture(id);
        }
        let present_start = perf.now();
        output.present();
        let present_ms = (perf.now() - present_start) as f32;
        let composite_ms = (perf.now() - composite_start) as f32;
        let frame_cpu_ms = (perf.now() - frame_start) as f32;
        let frame_interval_ms = (dt_secs * 1000.0) as f32;
        let estimated_idle_ms = (frame_interval_ms - frame_cpu_ms).max(0.0);

        self.smoothed_frame_cpu_ms = self.smoothed_frame_cpu_ms * 0.85 + frame_cpu_ms * 0.15;
        self.smoothed_frame_interval_ms =
            self.smoothed_frame_interval_ms * 0.85 + frame_interval_ms * 0.15;
        self.smoothed_surface_acquire_ms =
            self.smoothed_surface_acquire_ms * 0.85 + acquire_ms * 0.15;
        self.smoothed_terminal_ms = self.smoothed_terminal_ms * 0.85 + terminal_ms * 0.15;
        self.smoothed_screen_ms = self.smoothed_screen_ms * 0.85 + screen_ms * 0.15;
        self.smoothed_scene_ms = self.smoothed_scene_ms * 0.85 + scene_ms * 0.15;
        self.smoothed_composite_ms = self.smoothed_composite_ms * 0.85 + composite_ms * 0.15;
        if self.settings_visible {
            self.publish_perf_metrics(
                fps_label,
                previewing,
                terminal_redrew,
                screen_redrew,
                overlay_w,
                overlay_h,
                terminal_ms,
                scene_ms,
                composite_ms,
                acquire_ms,
                submit_ms,
                present_ms,
                frame_cpu_ms,
                frame_interval_ms,
                estimated_idle_ms,
                client_w,
                client_h,
                perf_toggles,
            );
        }

        Ok(())
    }

    fn publish_perf_metrics(
        &self,
        fps_label: u32,
        previewing: bool,
        terminal_redrew: bool,
        screen_redrew: bool,
        overlay_w: u32,
        overlay_h: u32,
        terminal_ms: f32,
        scene_ms: f32,
        composite_ms: f32,
        acquire_ms: f32,
        submit_ms: f32,
        present_ms: f32,
        frame_cpu_ms: f32,
        frame_interval_ms: f32,
        estimated_idle_ms: f32,
        client_w: u32,
        client_h: u32,
        perf_toggles: PerfToggles,
    ) {
        let render_config = RENDER_CONFIG.with(|cfg| *cfg.borrow());
        let perf = Object::new();
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("frame"),
            &JsValue::from_f64(self.frame_counter as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("fps"),
            &JsValue::from_f64(self.smoothed_fps as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("fpsLabel"),
            &JsValue::from_f64(fps_label as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("previewing"),
            &JsValue::from_bool(previewing),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalRedrew"),
            &JsValue::from_bool(terminal_redrew),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("screenRedrew"),
            &JsValue::from_bool(screen_redrew),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalMs"),
            &JsValue::from_f64(terminal_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("sceneMs"),
            &JsValue::from_f64(scene_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("compositeMs"),
            &JsValue::from_f64(composite_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedTerminalMs"),
            &JsValue::from_f64(self.smoothed_terminal_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedSceneMs"),
            &JsValue::from_f64(self.smoothed_scene_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedScreenMs"),
            &JsValue::from_f64(self.smoothed_screen_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedFrameCpuMs"),
            &JsValue::from_f64(self.smoothed_frame_cpu_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedFrameIntervalMs"),
            &JsValue::from_f64(self.smoothed_frame_interval_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedSurfaceAcquireMs"),
            &JsValue::from_f64(self.smoothed_surface_acquire_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedEguiMs"),
            &JsValue::from_f64(self.smoothed_egui_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("smoothedCompositeMs"),
            &JsValue::from_f64(self.smoothed_composite_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("canvasWidth"),
            &JsValue::from_f64(self.config.width as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("canvasHeight"),
            &JsValue::from_f64(self.config.height as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalWidth"),
            &JsValue::from_f64(overlay_w as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalHeight"),
            &JsValue::from_f64(overlay_h as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("canvasClientWidth"),
            &JsValue::from_f64(client_w as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("canvasClientHeight"),
            &JsValue::from_f64(client_h as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("devicePixelRatio"),
            &JsValue::from_f64(self.last_effective_dpr),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("surfaceAcquireMs"),
            &JsValue::from_f64(acquire_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("queueSubmitMs"),
            &JsValue::from_f64(submit_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("presentMs"),
            &JsValue::from_f64(present_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("frameCpuMs"),
            &JsValue::from_f64(frame_cpu_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("frameIntervalMs"),
            &JsValue::from_f64(frame_interval_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("estimatedIdleMs"),
            &JsValue::from_f64(estimated_idle_ms as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalDrawCalls"),
            &JsValue::from_f64(if terminal_redrew { 1.0 } else { 0.0 }),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalCellInstances"),
            &JsValue::from_f64((TERM_COLS as u32 * TERM_ROWS as u32) as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("compositeDrawCalls"),
            &JsValue::from_f64(1.0),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("eguiPaintJobs"),
            &JsValue::from_f64(self.egui_paint_jobs as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("eguiTexturesDelta"),
            &JsValue::from_f64(self.egui_textures_delta as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("surfacePixels"),
            &JsValue::from_f64((self.config.width as u64 * self.config.height as u64) as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("terminalPixels"),
            &JsValue::from_f64((overlay_w as u64 * overlay_h as u64) as f64),
        );
        if let Some(scene_stats) = self.renderer.offscreen_perf_stats() {
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneWidth"),
                &JsValue::from_f64(scene_stats.scene_width as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneHeight"),
                &JsValue::from_f64(scene_stats.scene_height as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneOutputWidth"),
                &JsValue::from_f64(scene_stats.output_width as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneOutputHeight"),
                &JsValue::from_f64(scene_stats.output_height as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("scenePassCount"),
                &JsValue::from_f64(scene_stats.pass_count as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneDrawCalls"),
                &JsValue::from_f64(scene_stats.draw_call_count as f64),
            );
            let _ = Reflect::set(
                &perf,
                &JsValue::from_str("sceneHasDownsample"),
                &JsValue::from_bool(scene_stats.has_downsample),
            );
        }
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("crtEnabled"),
            &JsValue::from_bool(perf_toggles.crt),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("transferEnabled"),
            &JsValue::from_bool(perf_toggles.transfer),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("overlayFilterEnabled"),
            &JsValue::from_bool(perf_toggles.overlay_filter && render_config.overlay_filter),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("displayPixelated"),
            &JsValue::from_bool(render_config.display_pixelated),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("billboardEnabled"),
            &JsValue::from_bool(perf_toggles.billboard),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("previewMaxDim"),
            &JsValue::from_f64(render_config.preview_max_dim as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("galleryCanvasScale"),
            &JsValue::from_f64(render_config.gallery_canvas_scale as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("previewCanvasScale"),
            &JsValue::from_f64(render_config.preview_canvas_scale as f64),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("activeCanvasScale"),
            &JsValue::from_f64(if previewing {
                render_config.preview_canvas_scale as f64
            } else {
                render_config.gallery_canvas_scale as f64
            }),
        );
        let _ = Reflect::set(
            &perf,
            &JsValue::from_str("previewRenderScale"),
            &JsValue::from_f64(render_config.preview_render_scale as f64),
        );

        if let Some(window) = web_sys::window() {
            let _ = Reflect::set(window.as_ref(), &JsValue::from_str("__emojiPerf"), &perf);
        }
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
                    label: Some("composite_bg"),
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
                    label: Some("screen_bg"),
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
}

fn create_rgba_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("overlay_texture"),
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

    for format in formats {
        match format {
            wgpu::TextureFormat::Bgra8UnormSrgb => return wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb => return wgpu::TextureFormat::Rgba8Unorm,
            _ => {}
        }
    }

    formats[0]
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

fn demo_texture() -> (Vec<[u8; 4]>, u32, u32) {
    let w = 96u32;
    let h = 96u32;
    let mut pixels = vec![[0u8, 0, 0, 0]; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let inside = x > 12 && x < 84 && y > 12 && y < 84;
            pixels[idx] = if inside {
                let dx = x as f32 / w as f32;
                let dy = y as f32 / h as f32;
                if x < w / 2 {
                    [255, (80.0 + dy * 80.0) as u8, 70, 255]
                } else {
                    [70, (120.0 + dx * 40.0) as u8, 255, 255]
                }
            } else {
                [0, 0, 0, 0]
            };
        }
    }
    fill_transparent_rgb_from_nearest(&mut pixels, w, h, COLOR_SOURCE_ALPHA_THRESHOLD);
    (pixels, w, h)
}

mod console_error_panic_hook {
    use std::panic;

    pub fn hook(info: &panic::PanicHookInfo<'_>) {
        web_sys::console::error_1(&format!("{info}").into());
    }
}
