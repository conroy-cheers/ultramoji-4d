use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use softbuffer::{Context as SoftbufferContext, Surface as SoftbufferSurface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopBuilder};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

mod preview {
    #[allow(dead_code)]
    pub mod common {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui/emoji_preview/common.rs"
        ));
    }

    #[allow(dead_code)]
    pub mod cpu {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui/emoji_preview/cpu.rs"
        ));
    }

    #[allow(dead_code)]
    pub mod gpu {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui/emoji_preview/gpu.rs"
        ));
    }
}

use preview::common::Texture;
use preview::common::{COLOR_SOURCE_ALPHA_THRESHOLD, fill_transparent_rgb_from_nearest};

const VIEWER_COMPOSITE_SHADER: &str = r#"
struct CompositeUniforms {
    output_size: vec2f,
    overlay_origin_px: vec2f,
    overlay_size_px: vec2f,
    _pad: vec2f,
}

@group(0) @binding(0) var billboard_tex: texture_2d<f32>;
@group(0) @binding(1) var billboard_sampler: sampler;
@group(0) @binding(2) var overlay_tex: texture_2d<f32>;
@group(0) @binding(3) var overlay_sampler: sampler;
@group(0) @binding(4) var<uniform> u: CompositeUniforms;

struct VsOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var positions = array<vec2f, 3>(
        vec2f(-1.0, -3.0),
        vec2f(-1.0, 1.0),
        vec2f(3.0, 1.0),
    );
    var uvs = array<vec2f, 3>(
        vec2f(0.0, 2.0),
        vec2f(0.0, 0.0),
        vec2f(2.0, 0.0),
    );

    var out: VsOut;
    out.position = vec4f(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let frag_px = in.uv * u.output_size;
    let bg_t = clamp(length((frag_px - u.output_size * 0.5) / u.output_size), 0.0, 1.0);
    let bg = mix(vec3f(0.14, 0.16, 0.20), vec3f(0.04, 0.05, 0.10), bg_t);

    let bill_uv = vec2f(in.uv.x, 1.0 - in.uv.y);
    let bill = textureSample(billboard_tex, billboard_sampler, bill_uv);
    var color = mix(bg, bill.rgb, bill.a);

    let overlay_rel = frag_px - u.overlay_origin_px;
    if all(overlay_rel >= vec2f(0.0)) && all(overlay_rel < u.overlay_size_px) {
        let overlay_uv = overlay_rel / u.overlay_size_px;
        let overlay = textureSample(overlay_tex, overlay_sampler, overlay_uv);
        color = mix(color, overlay.rgb, overlay.a);
    }

    return vec4f(color, 1.0);
}
"#;

fn main() -> Result<()> {
    ensure_linux_gui_runtime_env()?;

    let args = Cli::parse(std::env::args().skip(1))?;
    if args.print_help {
        print_help();
        return Ok(());
    }

    let image = load_source(&args.source)?;

    if args.smoke_test && preferred_backend(args.backend).is_none() {
        run_headless_smoke_test(&image)?;
        return Ok(());
    }

    let mut builder = EventLoop::builder();
    configure_event_loop(&mut builder, args.backend);

    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(err) if args.smoke_test => {
            eprintln!("GUI startup failed ({err}); falling back to headless smoke test");
            run_headless_smoke_test(&image)?;
            return Ok(());
        }
        Err(err) => {
            return Err(anyhow!(err)).context(
                "failed to start emoji_preview_viewer event loop; use --smoke-test for headless verification",
            );
        }
    };

    let mut app = ViewerApp::new(args, image);
    event_loop.run_app(&mut app)?;

    if let Some(err) = app.exit_error {
        return Err(err);
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendChoice {
    Auto,
    X11,
    Wayland,
}

#[derive(Debug)]
struct Cli {
    source: String,
    backend: BackendChoice,
    smoke_test: bool,
    print_help: bool,
    bg_color: Option<[f32; 3]>,
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut cli = Self {
            source: "demo".into(),
            backend: BackendChoice::Auto,
            smoke_test: false,
            print_help: false,
            bg_color: None,
        };

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--backend" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--backend requires one of: auto, x11, wayland"))?;
                    cli.backend = parse_backend(&value)?;
                }
                "--bg" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--bg requires a hex color (e.g. 1a1a2e)"))?;
                    cli.bg_color = Some(parse_hex_color(&value)?);
                }
                "--smoke-test" => cli.smoke_test = true,
                "--help" | "-h" => cli.print_help = true,
                other if other.starts_with('-') => {
                    bail!("unknown flag: {other}");
                }
                other => {
                    cli.source = other.to_string();
                }
            }
        }

        Ok(cli)
    }
}

fn parse_hex_color(s: &str) -> Result<[f32; 3]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        bail!("expected 6-digit hex color, got '{s}'");
    }
    let r = u8::from_str_radix(&s[0..2], 16)?;
    let g = u8::from_str_radix(&s[2..4], 16)?;
    let b = u8::from_str_radix(&s[4..6], 16)?;
    Ok([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

fn parse_backend(value: &str) -> Result<BackendChoice> {
    match value {
        "auto" => Ok(BackendChoice::Auto),
        "x11" => Ok(BackendChoice::X11),
        "wayland" => Ok(BackendChoice::Wayland),
        _ => bail!("invalid backend '{value}', expected auto, x11, or wayland"),
    }
}

fn print_help() {
    eprintln!("emoji_preview_viewer [--backend auto|x11|wayland] [--bg HEXCOLOR] [--smoke-test] [IMAGE]");
    eprintln!("  IMAGE defaults to 'demo' and may be a PNG/GIF path.");
    eprintln!("  --bg sets the background/floor color (e.g. --bg 1a1a2e)");
}

#[cfg(target_os = "linux")]
fn ensure_linux_gui_runtime_env() -> Result<()> {
    use std::collections::BTreeSet;
    use std::os::unix::process::CommandExt;

    const REEXEC_MARKER: &str = "SLACKSLACK_EMOJI_PREVIEW_REEXEC";
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
    Err(anyhow!(err)).context("failed to re-exec emoji_preview_viewer with GUI library path")
}

#[cfg(target_os = "linux")]
fn linux_gui_library_dirs() -> Result<Vec<String>> {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

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

const NUM_SLIDERS: usize = 13;

struct SliderState {
    active: usize,
    rotation: f32,
    camera_pitch: f32,
    light_azimuth: f32,
    light_elevation: f32,
    light_distance: f32,
    ground_y: f32,
    contrast: f32,
    ssao_strength: f32,
    ssao_depth_thresh: f32,
    ssao_start_dist: f32,
    ssao_step_growth: f32,
    ssao_max_shadow: f32,
    render_scale: f32,
    bg_color: Option<[f32; 3]>,
}

impl SliderState {
    fn new(bg_color: Option<[f32; 3]>) -> Self {
        Self {
            active: 0,
            rotation: 0.0,
            camera_pitch: 0.26,
            light_azimuth: 0.8,
            light_elevation: 0.96,
            light_distance: 4.8,
            ground_y: -1.15,
            contrast: 1.15,
            ssao_strength: 10.0,
            ssao_depth_thresh: 0.0,
            ssao_start_dist: 0.1,
            ssao_step_growth: 1.20,
            ssao_max_shadow: 0.4,
            render_scale: 1.0,
            bg_color,
        }
    }

    fn sliders(&self) -> [(&'static str, f32, f32, f32, f32); NUM_SLIDERS] {
        use std::f32::consts::PI;
        [
            ("ROTATION", self.rotation, -PI, PI, 0.05),
            ("CAM PITCH", self.camera_pitch, -0.8, 0.8, 0.02),
            ("LIGHT AZ", self.light_azimuth, -PI, PI, 0.05),
            ("LIGHT EL", self.light_elevation, 0.1, 1.4, 0.02),
            ("LIGHT DIST", self.light_distance, 1.0, 8.0, 0.1),
            ("GROUND Y", self.ground_y, -3.0, -0.5, 0.05),
            ("CONTRAST", self.contrast, 0.5, 8.0, 0.05),
            ("SS STRENGTH", self.ssao_strength, 0.0, 15.0, 0.2),
            ("SS DEPTH", self.ssao_depth_thresh, 0.001, 1.5, 0.01),
            ("SS START", self.ssao_start_dist, 0.1, 30.0, 0.5),
            ("SS GROWTH", self.ssao_step_growth, 1.01, 3.0, 0.02),
            ("SS MAX", self.ssao_max_shadow, 0.0, 3.0, 0.1),
            ("RENDER SCL", self.render_scale, 1.0, 8.0, 0.25),
        ]
    }

    fn value_mut(&mut self) -> &mut f32 {
        match self.active {
            0 => &mut self.rotation,
            1 => &mut self.camera_pitch,
            2 => &mut self.light_azimuth,
            3 => &mut self.light_elevation,
            4 => &mut self.light_distance,
            5 => &mut self.ground_y,
            6 => &mut self.contrast,
            7 => &mut self.ssao_strength,
            8 => &mut self.ssao_depth_thresh,
            9 => &mut self.ssao_start_dist,
            10 => &mut self.ssao_step_growth,
            11 => &mut self.ssao_max_shadow,
            12 => &mut self.render_scale,
            _ => unreachable!(),
        }
    }

    fn adjust(&mut self, delta: i32) {
        let (_, _, min, max, step) = self.sliders()[self.active];
        let val = self.value_mut();
        *val = (*val + delta as f32 * step).clamp(min, max);
    }

    fn next(&mut self) {
        self.active = (self.active + 1) % NUM_SLIDERS;
    }

    fn prev(&mut self) {
        self.active = (self.active + NUM_SLIDERS - 1) % NUM_SLIDERS;
    }

    fn scene_params(&self) -> preview::gpu::SceneParams {
        preview::gpu::SceneParams {
            rotation: Some(self.rotation),
            camera_pitch: Some(self.camera_pitch),
            light_azimuth: Some(self.light_azimuth),
            light_elevation: Some(self.light_elevation),
            light_distance: Some(self.light_distance),
            ground_y: Some(self.ground_y),
            bob: None,
            fill: None,
            bg_color: self.bg_color,
            sharpen: None,
            contrast: Some(self.contrast),
            dither: None,
            vhs: None,
            jitter: None,
            ssao_strength: Some(self.ssao_strength),
            ssao_depth_threshold: Some(self.ssao_depth_thresh),
            ssao_start_dist: Some(self.ssao_start_dist),
            ssao_step_growth: Some(self.ssao_step_growth),
            ssao_max_shadow: Some(self.ssao_max_shadow),
            supersample: false,
            show_depth: false,
            render_scale: None,
        }
    }
}

struct ViewerApp {
    cli: Cli,
    image: LoadedImage,
    start: Instant,
    window: Option<Arc<Window>>,
    viewer: Option<Viewer>,
    window_id: Option<WindowId>,
    rendered_frames: u32,
    exit_error: Option<anyhow::Error>,
    sliders: SliderState,
}

impl ViewerApp {
    fn new(cli: Cli, image: LoadedImage) -> Self {
        let sliders = SliderState::new(cli.bg_color);
        Self {
            cli,
            image,
            start: Instant::now(),
            window: None,
            viewer: None,
            window_id: None,
            rendered_frames: 0,
            exit_error: None,
            sliders,
        }
    }

    fn fail_or_headless(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        if self.cli.smoke_test {
            eprintln!("GUI viewer failed ({err}); falling back to headless smoke test");
            if let Err(headless_err) = run_headless_smoke_test(&self.image) {
                self.exit_error = Some(headless_err.context(err.to_string()));
            }
        } else {
            self.exit_error = Some(err);
        }
        event_loop.exit();
    }
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);

        if self.window.is_some() {
            return;
        }

        let title = format!("Emoji Billboard Viewer — {}", self.image.label);
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(PhysicalSize::new(960, 720));

        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.fail_or_headless(event_loop, anyhow!(err).context("failed to create window"));
                return;
            }
        };

        let viewer = match pollster::block_on(Viewer::new(window.clone())) {
            Ok(viewer) => viewer,
            Err(err) => {
                self.fail_or_headless(event_loop, err.context("failed to initialize viewer"));
                return;
            }
        };

        self.window_id = Some(window.id());
        self.viewer = Some(viewer);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if Some(window_id) != self.window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state.is_pressed() {
                    if event.logical_key == Key::Named(NamedKey::Escape) {
                        event_loop.exit();
                    } else if event.logical_key == Key::Named(NamedKey::ArrowUp) {
                        self.sliders.prev();
                    } else if event.logical_key == Key::Named(NamedKey::ArrowDown) {
                        self.sliders.next();
                    } else if event.logical_key == Key::Named(NamedKey::ArrowLeft) {
                        self.sliders.adjust(-1);
                    } else if event.logical_key == Key::Named(NamedKey::ArrowRight) {
                        self.sliders.adjust(1);
                    } else if let Some(viewer) = &mut self.viewer {
                        if let Some(text) = &event.text {
                            match text.as_str() {
                                "w" => viewer.toggle_wireframe(),
                                "b" => viewer.toggle_all_white(),
                                "s" => viewer.toggle_stencil_shadow(),
                                "t" => viewer.show_tui_mode = !viewer.show_tui_mode,
                                "d" => viewer.toggle_depth(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(viewer) = &mut self.viewer {
                    viewer.resize(size);
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(viewer) = &mut self.viewer {
                    viewer.resize(viewer.window.inner_size());
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(viewer) = &mut self.viewer else {
                    return;
                };
                if let Err(err) =
                    viewer.render(&self.image, self.start.elapsed().as_millis() as u64, &self.sliders)
                {
                    self.fail_or_headless(event_loop, err.context("render failed"));
                    return;
                }

                self.rendered_frames += 1;
                if self.cli.smoke_test && self.rendered_frames >= 1 {
                    eprintln!("GUI smoke test rendered {} frame", self.rendered_frames);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

struct LoadedImage {
    frames: Vec<Vec<[u8; 4]>>,
    delays: Vec<u32>,
    width: u32,
    height: u32,
    label: String,
}

fn load_source(source: &str) -> Result<LoadedImage> {
    if source == "demo" {
        return Ok(demo_image());
    }

    let data = std::fs::read(source).with_context(|| format!("failed to read {source}"))?;
    let path = Path::new(source);
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source)
        .to_string();

    decode_image_frames(&data, label)
}

fn demo_image() -> LoadedImage {
    let width = 96;
    let height = 96;
    let mut pixels = vec![[0, 0, 0, 0]; width * height];
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let dx = x as f32 / width as f32;
            let dy = y as f32 / height as f32;
            let inside = (x > 12 && x < 84) && (y > 12 && y < 84);
            pixels[idx] = if inside {
                if x < width / 2 {
                    [255, (80.0 + dy * 80.0) as u8, 70, 255]
                } else {
                    [70, (120.0 + dx * 40.0) as u8, 255, 255]
                }
            } else {
                [0, 0, 0, 0]
            };
        }
    }

    LoadedImage {
        frames: vec![pixels],
        delays: vec![0],
        width: width as u32,
        height: height as u32,
        label: "demo".into(),
    }
}

fn decode_image_frames(data: &[u8], label: String) -> Result<LoadedImage> {
    if data.len() >= 6 && &data[0..4] == b"GIF8" {
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data))
            .context("failed to decode gif")?;
        use image::AnimationDecoder;
        let frames: Vec<image::Frame> = decoder.into_frames().collect_frames()?;
        if frames.is_empty() {
            return Err(anyhow!("gif contains no frames"));
        }

        let width = frames[0].buffer().width();
        let height = frames[0].buffer().height();
        let mut rgba_frames = Vec::with_capacity(frames.len());
        let mut delays = Vec::with_capacity(frames.len());
        for frame in frames {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let buffer = frame.into_buffer();
            let resized = if buffer.width() != width || buffer.height() != height {
                image::imageops::resize(
                    &buffer,
                    width,
                    height,
                    image::imageops::FilterType::Nearest,
                )
            } else {
                buffer
            };
            let mut pixels: Vec<[u8; 4]> = resized.pixels().map(|p| p.0).collect();
            fill_transparent_rgb_from_nearest(
                &mut pixels,
                width,
                height,
                COLOR_SOURCE_ALPHA_THRESHOLD,
            );
            rgba_frames.push(pixels);
            delays.push(if denom == 0 { numer } else { numer / denom });
        }

        return Ok(LoadedImage {
            frames: rgba_frames,
            delays,
            width,
            height,
            label,
        });
    }

    let image = image::load_from_memory(data).context("failed to decode image")?;
    let rgba = image.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let mut pixels: Vec<[u8; 4]> = rgba.pixels().map(|p| p.0).collect();
    fill_transparent_rgb_from_nearest(&mut pixels, width, height, COLOR_SOURCE_ALPHA_THRESHOLD);
    Ok(LoadedImage {
        frames: vec![pixels],
        delays: vec![0],
        width,
        height,
        label,
    })
}

fn current_frame(image: &LoadedImage, elapsed_ms: u64) -> usize {
    if image.frames.len() <= 1 {
        return 0;
    }
    let total: u64 = image
        .delays
        .iter()
        .map(|delay| (*delay).max(20) as u64)
        .sum();
    if total == 0 {
        return 0;
    }
    let mut cursor = elapsed_ms % total;
    for (idx, delay) in image.delays.iter().enumerate() {
        let delay = (*delay).max(20) as u64;
        if cursor < delay {
            return idx;
        }
        cursor -= delay;
    }
    image.frames.len() - 1
}

fn run_headless_smoke_test(image: &LoadedImage) -> Result<()> {
    let frame_idx = current_frame(image, 0);
    let texture = Texture {
        pixels: &image.frames[frame_idx],
        width: image.width,
        height: image.height,
    };
    let rgb = match preview::gpu::try_new() {
        Ok(mut gpu) => gpu.render_billboard_rgb(&texture, 960, 720, 0.0),
        Err(_) => preview::cpu::render_billboard_rgb(&texture, 960, 720, 0.0),
    };
    if rgb.is_empty() {
        bail!("headless smoke test produced no pixels");
    }
    eprintln!("Headless smoke test rendered {} pixels", rgb.len());
    Ok(())
}

struct Viewer {
    window: Arc<Window>,
    presenter: Presenter,
    perf: PerfStats,
    frame_size: (u32, u32),
    show_wireframe: bool,
    show_all_white: bool,
    show_stencil_shadow: bool,
    show_tui_mode: bool,
    show_depth: bool,
}

enum Presenter {
    Gpu(GpuPresenter),
    Cpu(CpuPresenter),
}

struct CpuPresenter {
    _context: SoftbufferContext<Arc<Window>>,
    surface: SoftbufferSurface<Arc<Window>, Arc<Window>>,
}

struct GpuPresenter {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: preview::gpu::GpuRenderer,
    composite_pipeline: wgpu::RenderPipeline,
    composite_bind_group_layout: wgpu::BindGroupLayout,
    composite_uniform_buffer: wgpu::Buffer,
    billboard_sampler: wgpu::Sampler,
    overlay_sampler: wgpu::Sampler,
    overlay_texture: wgpu::Texture,
    overlay_view: wgpu::TextureView,
    overlay_size: (u32, u32),
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    output_size: [f32; 2],
    overlay_origin_px: [f32; 2],
    overlay_size_px: [f32; 2],
    _pad: [f32; 2],
}

struct PerfStats {
    last_frame_start: Option<Instant>,
    frame_interval_ms_ema: Option<f64>,
    render_ms_ema: Option<f64>,
    fps_ema: Option<f64>,
    frame_count: u64,
}

impl Viewer {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let presenter = match GpuPresenter::new(window.clone()).await {
            Ok(gpu) => Presenter::Gpu(gpu),
            Err(err) => {
                eprintln!("GPU present unavailable, falling back to CPU: {err}");
                Presenter::Cpu(CpuPresenter::new(window.clone())?)
            }
        };

        Ok(Self {
            window,
            presenter,
            perf: PerfStats {
                last_frame_start: None,
                frame_interval_ms_ema: None,
                render_ms_ema: None,
                fps_ema: None,
                frame_count: 0,
            },
            frame_size: (size.width.max(1), size.height.max(1)),
            show_wireframe: false,
            show_all_white: false,
            show_stencil_shadow: true,
            show_tui_mode: false,
            show_depth: false,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let resized = match &mut self.presenter {
            Presenter::Gpu(gpu) => gpu.resize(size).is_ok(),
            Presenter::Cpu(cpu) => cpu.resize(size).is_ok(),
        };
        if resized {
            self.frame_size = (size.width, size.height);
        }
    }

    fn toggle_wireframe(&mut self) {
        let new_value = !self.show_wireframe;
        if let Presenter::Gpu(gpu) = &mut self.presenter {
            if gpu.renderer.wireframe_supported() {
                gpu.renderer.set_wireframe(new_value);
                self.show_wireframe = new_value;
            }
        }
    }

    fn toggle_all_white(&mut self) {
        let new_value = !self.show_all_white;
        if let Presenter::Gpu(gpu) = &mut self.presenter {
            gpu.renderer.set_all_white(new_value);
        }
        self.show_all_white = new_value;
    }

    fn toggle_depth(&mut self) {
        self.show_depth = !self.show_depth;
    }

    fn toggle_stencil_shadow(&mut self) {
        let new_value = !self.show_stencil_shadow;
        if let Presenter::Gpu(gpu) = &mut self.presenter {
            gpu.renderer.set_stencil_shadow(new_value);
        }
        self.show_stencil_shadow = new_value;
    }

    fn wireframe_supported(&self) -> bool {
        match &self.presenter {
            Presenter::Gpu(gpu) => gpu.renderer.wireframe_supported(),
            Presenter::Cpu(_) => false,
        }
    }

    fn render(&mut self, image: &LoadedImage, elapsed_ms: u64, sliders: &SliderState) -> Result<()> {
        let frame_start = Instant::now();
        if let Some(last) = self.perf.last_frame_start.replace(frame_start) {
            let frame_ms = frame_start.duration_since(last).as_secs_f64() * 1000.0;
            self.perf.frame_interval_ms_ema = Some(ema(self.perf.frame_interval_ms_ema, frame_ms));
            self.perf.fps_ema = Some(ema(self.perf.fps_ema, 1000.0 / frame_ms.max(0.000_1)));
        }

        let (out_w, out_h) = self.frame_size;
        if out_w == 0 || out_h == 0 {
            return Ok(());
        }

        let frame_idx = current_frame(image, elapsed_ms);
        let texture = Texture {
            pixels: &image.frames[frame_idx],
            width: image.width,
            height: image.height,
        };
        let mut scene_params = sliders.scene_params();
        scene_params.show_depth = self.show_depth;

        if self.show_tui_mode {
            let tui_cols = 140usize;
            let tui_rows = (tui_cols as f32 / (out_w as f32 / out_h as f32) / 2.0).max(1.0) as usize;
            let tui_px_w = tui_cols;
            let tui_px_h = tui_rows * 2;

            let tui_params = preview::gpu::SceneParams {
                sharpen: Some(0.1),
                dither: Some(0.3),
                vhs: Some(0.5),
                jitter: Some(0.1),
                supersample: false,
                render_scale: Some(sliders.render_scale),
                ..scene_params
            };

            let time_secs = elapsed_ms as f64 / 1000.0;
            let (renderer_name, render_w, render_h, tui_rgb) = match &mut self.presenter {
                Presenter::Gpu(gpu) => {
                    let rgb = gpu.renderer.readback_offscreen_rgb(
                        &texture,
                        tui_px_w,
                        tui_px_h,
                        time_secs,
                        &tui_params,
                    );
                    ("GPU/TUI", tui_px_w, tui_px_h, rgb)
                }
                Presenter::Cpu(_) => {
                    let rgb = preview::cpu::render_billboard_rgb(
                        &texture, tui_px_w, tui_px_h, time_secs,
                    );
                    ("CPU/TUI", tui_px_w, tui_px_h, rgb)
                }
            };

            let cell_colors = halfblock_cell_colors(&tui_rgb, tui_px_w, tui_px_h, tui_rows);
            let mut fb = vec![(0u8, 0u8, 0u8); out_w as usize * out_h as usize];
            scale_cells_to_fb(
                &cell_colors,
                tui_cols,
                tui_rows,
                &mut fb,
                out_w as usize,
                out_h as usize,
            );

            let render_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
            self.perf.render_ms_ema = Some(ema(self.perf.render_ms_ema, render_ms));
            self.perf.frame_count += 1;

            let overlay = PerfOverlay {
                fps: self.perf.fps_ema.unwrap_or(0.0),
                frame_ms: self.perf.frame_interval_ms_ema.unwrap_or(0.0),
                render_ms: self.perf.render_ms_ema.unwrap_or(0.0),
                frame_count: self.perf.frame_count,
                output_w: out_w as usize,
                output_h: out_h as usize,
                render_w,
                render_h,
                renderer_name,
                show_wireframe: self.show_wireframe,
                wireframe_supported: self.wireframe_supported(),
                show_all_white: self.show_all_white,
                show_stencil_shadow: self.show_stencil_shadow,
                show_tui_mode: self.show_tui_mode,
                show_depth: self.show_depth,
                sliders,
            };
            return match &mut self.presenter {
                Presenter::Gpu(gpu) => gpu.present_rgb_fb(&fb, &overlay, out_w, out_h),
                Presenter::Cpu(cpu) => {
                    draw_perf_overlay(&mut fb, out_w as usize, out_h as usize, &overlay);
                    cpu.present_rgb(&fb)
                }
            };
        }

        let time_secs = elapsed_ms as f64 / 1000.0;
        let (renderer_name, render_w, render_h) = match &mut self.presenter {
            Presenter::Gpu(gpu) => {
                gpu.render_scene(&texture, out_w as usize, out_h as usize, time_secs, &scene_params)?
            }
            Presenter::Cpu(_) => ("CPU", out_w as usize, out_h as usize),
        };
        let render_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        self.perf.render_ms_ema = Some(ema(self.perf.render_ms_ema, render_ms));
        self.perf.frame_count += 1;

        let overlay = PerfOverlay {
            fps: self.perf.fps_ema.unwrap_or(0.0),
            frame_ms: self.perf.frame_interval_ms_ema.unwrap_or(0.0),
            render_ms: self.perf.render_ms_ema.unwrap_or(0.0),
            frame_count: self.perf.frame_count,
            output_w: out_w as usize,
            output_h: out_h as usize,
            render_w,
            render_h,
            renderer_name,
            show_wireframe: self.show_wireframe,
            wireframe_supported: self.wireframe_supported(),
            show_all_white: self.show_all_white,
            show_stencil_shadow: self.show_stencil_shadow,
            show_tui_mode: self.show_tui_mode,
            show_depth: self.show_depth,
            sliders,
        };
        match &mut self.presenter {
            Presenter::Gpu(gpu) => gpu.present(&overlay, out_w, out_h),
            Presenter::Cpu(cpu) => {
                let mut rgb = preview::cpu::render_billboard_rgb(
                    &texture,
                    out_w as usize,
                    out_h as usize,
                    time_secs,
                );
                draw_perf_overlay(&mut rgb, out_w as usize, out_h as usize, &overlay);
                cpu.present_rgb(&rgb)
            }
        }
    }
}

impl CpuPresenter {
    fn new(window: Arc<Window>) -> Result<Self> {
        let context = SoftbufferContext::new(window.clone())
            .map_err(|err| anyhow!(err.to_string()))
            .context("failed to create software rendering context")?;
        let mut surface = SoftbufferSurface::new(&context, window.clone())
            .map_err(|err| anyhow!(err.to_string()))
            .context("failed to create software rendering surface")?;
        resize_softbuffer_surface(&mut surface, window.inner_size())?;
        Ok(Self {
            _context: context,
            surface,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        resize_softbuffer_surface(&mut self.surface, size)
    }

    fn present_rgb(&mut self, rgb: &[(u8, u8, u8)]) -> Result<()> {
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|err| anyhow!(err.to_string()))
            .context("failed to acquire software surface buffer")?;
        for (dst, (r, g, b)) in buffer.iter_mut().zip(rgb.iter().copied()) {
            *dst = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }
        buffer
            .present()
            .map_err(|err| anyhow!(err.to_string()))
            .context("failed to present software-rendered frame")
    }
}

impl GpuPresenter {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .context("failed to create wgpu surface")?;
        let adapter = preview::gpu::request_adapter(&instance, Some(&surface))?;
        let caps = surface.get_capabilities(&adapter);
        let renderer = preview::gpu::from_adapter(adapter)?;

        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoNoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(renderer.device(), &config);

        let device = renderer.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewer_composite_shader"),
            source: wgpu::ShaderSource::Wgsl(VIEWER_COMPOSITE_SHADER.into()),
        });

        let composite_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewer_composite_bgl"),
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
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
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

        let composite_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewer_composite_uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewer_composite_pipeline_layout"),
            bind_group_layouts: &[&composite_bind_group_layout],
            push_constant_ranges: &[],
        });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewer_composite_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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

        let billboard_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let overlay_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let (overlay_texture, overlay_view) = create_overlay_texture(device, 1, 1);

        Ok(Self {
            surface,
            config,
            renderer,
            composite_pipeline,
            composite_bind_group_layout,
            composite_uniform_buffer,
            billboard_sampler,
            overlay_sampler,
            overlay_texture,
            overlay_view,
            overlay_size: (1, 1),
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(self.renderer.device(), &self.config);
        Ok(())
    }

    fn render_scene(
        &mut self,
        texture: &Texture,
        out_w: usize,
        out_h: usize,
        time_secs: f64,
        params: &preview::gpu::SceneParams,
    ) -> Result<(&'static str, usize, usize)> {
        let (render_w, render_h) = capped_render_size(
            out_w,
            out_h,
            self.renderer.max_texture_dimension_2d() as usize,
        );
        self.renderer
            .render_to_offscreen_params(texture, render_w as u32, render_h as u32, time_secs, params)?;
        Ok(("GPU", render_w, render_h))
    }

    fn present(&mut self, overlay: &PerfOverlay<'_>, out_w: u32, out_h: u32) -> Result<()> {
        if self.config.width != out_w || self.config.height != out_h {
            self.resize(PhysicalSize::new(out_w, out_h))?;
        }

        let (overlay_rgba, overlay_w, overlay_h) = build_overlay_rgba(overlay);
        self.ensure_overlay_texture(overlay_w, overlay_h);
        self.renderer.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.overlay_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &overlay_rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(overlay_w * 4),
                rows_per_image: Some(overlay_h),
            },
            wgpu::Extent3d {
                width: overlay_w,
                height: overlay_h,
                depth_or_array_layers: 1,
            },
        );

        let uniforms = CompositeUniforms {
            output_size: [out_w as f32, out_h as f32],
            overlay_origin_px: [8.0, 8.0],
            overlay_size_px: [overlay_w as f32, overlay_h as f32],
            _pad: [0.0; 2],
        };
        self.renderer.queue().write_buffer(
            &self.composite_uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let offscreen_view = self
            .renderer
            .offscreen_view()
            .ok_or_else(|| anyhow!("offscreen billboard view unavailable"))?;
        let bind_group = self
            .renderer
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewer_composite_bg"),
                layout: &self.composite_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(offscreen_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.billboard_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.overlay_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.overlay_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: self.composite_uniform_buffer.as_entire_binding(),
                    },
                ],
            });

        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(self.renderer.device(), &self.config);
                self.surface.get_current_texture()?
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            self.renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewer_composite_encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer_composite_pass"),
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
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.renderer.queue().submit(Some(encoder.finish()));
        output.present();
        Ok(())
    }

    fn present_rgb_fb(&mut self, fb: &[(u8, u8, u8)], overlay: &PerfOverlay<'_>, out_w: u32, out_h: u32) -> Result<()> {
        let w = out_w as usize;
        let h = out_h as usize;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in (0..h).rev() {
            for &(r, g, b) in &fb[y * w..(y + 1) * w] {
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        self.renderer.write_to_postprocess_output(&rgba, out_w, out_h);
        self.present(overlay, out_w, out_h)
    }

    fn ensure_overlay_texture(&mut self, width: u32, height: u32) {
        if self.overlay_size == (width, height) {
            return;
        }
        let (texture, view) = create_overlay_texture(self.renderer.device(), width, height);
        self.overlay_texture = texture;
        self.overlay_view = view;
        self.overlay_size = (width, height);
    }
}

fn resize_softbuffer_surface(
    surface: &mut SoftbufferSurface<Arc<Window>, Arc<Window>>,
    size: PhysicalSize<u32>,
) -> Result<()> {
    let width = NonZeroU32::new(size.width.max(1)).unwrap();
    let height = NonZeroU32::new(size.height.max(1)).unwrap();
    surface
        .resize(width, height)
        .map_err(|err| anyhow!(err.to_string()))
        .context("failed to resize software surface")
}

fn create_overlay_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewer_overlay_texture"),
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

fn halfblock_cell_colors(
    rgb: &[(u8, u8, u8)],
    px_w: usize,
    px_h: usize,
    rows: usize,
) -> Vec<((u8, u8, u8), (u8, u8, u8))> {
    let mut cells = Vec::with_capacity(rows * px_w);
    for row in 0..rows {
        let top_y = row * 2;
        let bot_y = row * 2 + 1;
        for col in 0..px_w {
            let top = if top_y < px_h && top_y * px_w + col < rgb.len() {
                rgb[top_y * px_w + col]
            } else {
                (0, 0, 0)
            };
            let bot = if bot_y < px_h && bot_y * px_w + col < rgb.len() {
                rgb[bot_y * px_w + col]
            } else {
                (0, 0, 0)
            };
            cells.push((top, bot));
        }
    }
    cells
}

fn scale_cells_to_fb(
    cells: &[((u8, u8, u8), (u8, u8, u8))],
    cols: usize,
    rows: usize,
    fb: &mut [(u8, u8, u8)],
    out_w: usize,
    out_h: usize,
) {
    if cols == 0 || rows == 0 {
        return;
    }
    let cell_w = out_w as f32 / cols as f32;
    let cell_h = out_h as f32 / rows as f32;
    let half_h = cell_h / 2.0;

    for py in 0..out_h {
        let cell_row = ((py as f32 / cell_h) as usize).min(rows - 1);
        let is_top = (py as f32 - cell_row as f32 * cell_h) < half_h;
        for px in 0..out_w {
            let cell_col = ((px as f32 / cell_w) as usize).min(cols - 1);
            let cell = cells[cell_row * cols + cell_col];
            fb[py * out_w + px] = if is_top { cell.0 } else { cell.1 };
        }
    }
}

fn capped_render_size(width: usize, height: usize, max_dimension: usize) -> (usize, usize) {
    if width <= max_dimension && height <= max_dimension {
        return (width.max(1), height.max(1));
    }

    let scale = (max_dimension as f64 / width as f64).min(max_dimension as f64 / height as f64);
    let scaled_w = ((width as f64 * scale).floor() as usize).clamp(1, max_dimension);
    let scaled_h = ((height as f64 * scale).floor() as usize).clamp(1, max_dimension);
    (scaled_w, scaled_h)
}

fn ema(current: Option<f64>, sample: f64) -> f64 {
    match current {
        Some(current) => current * 0.88 + sample * 0.12,
        None => sample,
    }
}

struct PerfOverlay<'a> {
    fps: f64,
    frame_ms: f64,
    render_ms: f64,
    frame_count: u64,
    output_w: usize,
    output_h: usize,
    render_w: usize,
    render_h: usize,
    renderer_name: &'a str,
    show_wireframe: bool,
    wireframe_supported: bool,
    show_all_white: bool,
    show_stencil_shadow: bool,
    show_tui_mode: bool,
    show_depth: bool,
    sliders: &'a SliderState,
}

fn perf_overlay_lines(overlay: &PerfOverlay<'_>) -> Vec<String> {
    let mut lines = vec![
        format!("FPS {:.1}", overlay.fps),
        format!("FRAME {:.2}MS", overlay.frame_ms),
        format!("RENDER {:.2}MS", overlay.render_ms),
        format!(
            "{} {}X{}",
            overlay.renderer_name, overlay.render_w, overlay.render_h
        ),
        format!("OUT {}X{}", overlay.output_w, overlay.output_h),
        format!(
            "WIRE {}{}",
            if overlay.show_wireframe { "ON" } else { "OFF" },
            if overlay.wireframe_supported {
                ""
            } else {
                " (N/A)"
            }
        ),
        format!(
            "WHITE {}",
            if overlay.show_all_white { "ON" } else { "OFF" }
        ),
        format!(
            "STENCIL {}",
            if overlay.show_stencil_shadow { "ON" } else { "OFF" }
        ),
        format!(
            "TUI {}",
            if overlay.show_tui_mode { "ON" } else { "OFF" }
        ),
        format!(
            "DEPTH {}",
            if overlay.show_depth { "ON" } else { "OFF" }
        ),
        String::new(),
    ];
    for (i, (name, value, _, _, _)) in overlay.sliders.sliders().iter().enumerate() {
        let marker = if i == overlay.sliders.active { ">" } else { " " };
        lines.push(format!("{}{} {:.2}", marker, name, value));
    }
    lines.push(String::new());
    lines.push("UP/DN SLIDER  </> ADJUST".to_string());
    lines.push("W WIRE B WHITE S STENCIL".to_string());
    lines.push("T TUI D DEPTH".to_string());
    lines.push(format!("#{}", overlay.frame_count));
    lines
}

fn draw_perf_overlay(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    overlay: &PerfOverlay<'_>,
) {
    let lines = perf_overlay_lines(overlay);

    let scale = 2usize;
    let char_w = 4 * scale;
    let char_h = 6 * scale;
    let text_width = lines
        .iter()
        .map(|line| line.chars().count() * char_w)
        .max()
        .unwrap_or(0);
    let box_w = (text_width + 16).min(width);
    let box_h = ((lines.len() * char_h) + 16).min(height);
    darken_rect(fb, width, height, 8, 8, box_w, box_h, 0.28);
    stroke_rect(fb, width, height, 8, 8, box_w, box_h, (80, 180, 255));

    let mut y = 16usize;
    for line in &lines {
        draw_text(fb, width, height, 16, y, line, scale, (235, 245, 255));
        y += char_h;
    }
}

fn build_overlay_rgba(overlay: &PerfOverlay<'_>) -> (Vec<u8>, u32, u32) {
    let lines = perf_overlay_lines(overlay);
    let scale = 2usize;
    let char_w = 4 * scale;
    let char_h = 6 * scale;
    let text_width = lines
        .iter()
        .map(|line| line.chars().count() * char_w)
        .max()
        .unwrap_or(0);
    let width = (text_width + 16).max(1);
    let height = ((lines.len() * char_h) + 16).max(1);
    let mut fb = vec![[0u8; 4]; width * height];

    fill_rect_rgba(
        &mut fb,
        width,
        height,
        0,
        0,
        width,
        height,
        [12, 18, 28, 180],
    );
    stroke_rect_rgba(
        &mut fb,
        width,
        height,
        0,
        0,
        width,
        height,
        [80, 180, 255, 220],
    );

    let mut y = 8usize;
    for line in &lines {
        draw_text_rgba(
            &mut fb,
            width,
            height,
            8,
            y,
            line,
            scale,
            [235, 245, 255, 255],
        );
        y += char_h;
    }

    let rgba = fb.into_iter().flat_map(|px| px).collect();
    (rgba, width as u32, height as u32)
}

fn darken_rect(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    factor: f64,
) {
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for py in y..max_y {
        for px in x..max_x {
            let pixel = &mut fb[py * width + px];
            pixel.0 = (pixel.0 as f64 * factor) as u8;
            pixel.1 = (pixel.1 as f64 * factor) as u8;
            pixel.2 = (pixel.2 as f64 * factor) as u8;
        }
    }
}

fn stroke_rect(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    color: (u8, u8, u8),
) {
    if rect_w == 0 || rect_h == 0 {
        return;
    }
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for px in x..max_x {
        set_pixel(fb, width, height, px, y, color);
        set_pixel(fb, width, height, px, max_y.saturating_sub(1), color);
    }
    for py in y..max_y {
        set_pixel(fb, width, height, x, py, color);
        set_pixel(fb, width, height, max_x.saturating_sub(1), py, color);
    }
}

fn draw_text(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: (u8, u8, u8),
) {
    let mut cursor_x = x;
    for ch in text.chars() {
        draw_glyph(fb, width, height, cursor_x, y, ch, scale, color);
        cursor_x += 4 * scale;
    }
}

fn draw_glyph(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    ch: char,
    scale: usize,
    color: (u8, u8, u8),
) {
    let glyph = glyph_pattern(ch);
    for (row, pattern) in glyph.iter().enumerate() {
        for col in 0..3usize {
            if (pattern >> (2 - col)) & 1 == 0 {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    set_pixel(
                        fb,
                        width,
                        height,
                        x + col * scale + sx,
                        y + row * scale + sy,
                        color,
                    );
                }
            }
        }
    }
}

fn set_pixel(
    fb: &mut [(u8, u8, u8)],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    color: (u8, u8, u8),
) {
    if x >= width || y >= height {
        return;
    }
    fb[y * width + x] = color;
}

fn fill_rect_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    color: [u8; 4],
) {
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for py in y..max_y {
        for px in x..max_x {
            fb[py * width + px] = color;
        }
    }
}

fn stroke_rect_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    color: [u8; 4],
) {
    if rect_w == 0 || rect_h == 0 {
        return;
    }
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for px in x..max_x {
        set_pixel_rgba(fb, width, height, px, y, color);
        set_pixel_rgba(fb, width, height, px, max_y.saturating_sub(1), color);
    }
    for py in y..max_y {
        set_pixel_rgba(fb, width, height, x, py, color);
        set_pixel_rgba(fb, width, height, max_x.saturating_sub(1), py, color);
    }
}

fn draw_text_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: [u8; 4],
) {
    let mut cursor_x = x;
    for ch in text.chars() {
        draw_glyph_rgba(fb, width, height, cursor_x, y, ch, scale, color);
        cursor_x += 4 * scale;
    }
}

fn draw_glyph_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    ch: char,
    scale: usize,
    color: [u8; 4],
) {
    let glyph = glyph_pattern(ch);
    for (row, pattern) in glyph.iter().enumerate() {
        for col in 0..3usize {
            if (pattern >> (2 - col)) & 1 == 0 {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    set_pixel_rgba(
                        fb,
                        width,
                        height,
                        x + col * scale + sx,
                        y + row * scale + sy,
                        color,
                    );
                }
            }
        }
    }
}

fn set_pixel_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    color: [u8; 4],
) {
    if x >= width || y >= height {
        return;
    }
    fb[y * width + x] = color;
}

fn glyph_pattern(ch: char) -> [u8; 5] {
    match ch {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'A' => [0b111, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        '#' => [0b010, 0b111, 0b010, 0b111, 0b010],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b101, 0b010, 0b000, 0b010],
    }
}
