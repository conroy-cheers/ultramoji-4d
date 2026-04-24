use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow};
use softbuffer::{Context as SoftbufferContext, Surface as SoftbufferSurface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
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
}

use preview::common::{COLOR_SOURCE_ALPHA_THRESHOLD, Texture, fill_transparent_rgb_from_nearest};

fn main() -> Result<()> {
    ensure_linux_gui_runtime_env()?;

    let source = std::env::args().nth(1).unwrap_or("demo".into());
    let image = load_source(&source)?;

    let event_loop = {
        let mut builder = EventLoop::builder();
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::EventLoopBuilderExtWayland;
            builder.with_wayland();
        }
        builder.build()?
    };
    let mut app = App::new(image);
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

    const REEXEC_MARKER: &str = "SLACKSLACK_CONTOUR_VIEWER_REEXEC";
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
    Err(anyhow!(err)).context("failed to re-exec contour_viewer with GUI library path")
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

struct LoadedImage {
    pixels: Vec<[u8; 4]>,
    width: u32,
    height: u32,
    label: String,
}

fn load_source(source: &str) -> Result<LoadedImage> {
    if source == "demo" {
        return Ok(demo_image());
    }
    let data = std::fs::read(source).with_context(|| format!("failed to read {source}"))?;
    let img = image::load_from_memory(&data).context("failed to decode image")?;
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    let mut pixels: Vec<[u8; 4]> = rgba.pixels().map(|p| p.0).collect();
    fill_transparent_rgb_from_nearest(&mut pixels, w, h, COLOR_SOURCE_ALPHA_THRESHOLD);
    let label = Path::new(source)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(source)
        .to_string();
    Ok(LoadedImage {
        pixels,
        width: w,
        height: h,
        label,
    })
}

fn demo_image() -> LoadedImage {
    let (w, h) = (96usize, 96usize);
    let mut pixels = vec![[0u8, 0, 0, 0]; w * h];
    for y in 0..h {
        for x in 0..w {
            let inside = x > 12 && x < 84 && y > 12 && y < 84;
            pixels[y * w + x] = if inside {
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
    LoadedImage {
        pixels,
        width: w as u32,
        height: h as u32,
        label: "demo".into(),
    }
}

fn extract_contours(
    image: &LoadedImage,
    max_cells: usize,
    alpha_threshold: u8,
) -> Vec<Vec<[f64; 2]>> {
    let texture = Texture {
        pixels: &image.pixels,
        width: image.width,
        height: image.height,
    };

    let max_cells = max_cells.max(4);
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

    let mut field = vec![0.0f64; cols * rows];
    for gy in 0..rows {
        let v = gy as f64 / (rows - 1).max(1) as f64;
        for gx in 0..cols {
            let u = gx as f64 / (cols - 1).max(1) as f64;
            field[gy * cols + gx] = texture.sample(u, v)[3] as f64 / 255.0;
        }
    }

    let threshold = alpha_threshold as f64 / 255.0;
    let builder = contour::ContourBuilder::new(cols, rows, true)
        .x_step(1.0 / grid_w as f64)
        .y_step(1.0 / grid_h as f64);

    let contours = match builder.contours(&field, &[threshold]) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut rings = Vec::new();
    for contour in contours {
        let (multi_polygon, _) = contour.into_inner();
        for polygon in &multi_polygon.0 {
            let ext: Vec<[f64; 2]> = polygon.exterior().coords().map(|c| [c.x, c.y]).collect();
            if ext.len() >= 3 {
                rings.push(ext);
            }
            for interior in polygon.interiors() {
                let hole: Vec<[f64; 2]> = interior.coords().map(|c| [c.x, c.y]).collect();
                if hole.len() >= 3 {
                    rings.push(hole);
                }
            }
        }
    }
    rings
}

fn rasterize_contour_mask(contours: &[Vec<[f64; 2]>], width: usize, height: usize) -> Vec<bool> {
    let mut mask = vec![false; width * height];
    for py in 0..height {
        let y = py as f64 / height as f64;
        // Collect all winding-number crossings for this scanline, then fill spans
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for ring in contours {
            let n = ring.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let j = (i + 1) % n;
                let [xi, yi] = ring[i];
                let [xj, yj] = ring[j];
                if (yi <= y && yj > y) || (yj <= y && yi > y) {
                    let t = (y - yi) / (yj - yi);
                    let x_cross = xi + t * (xj - xi);
                    let dir = if yi < yj { 1 } else { -1 };
                    crossings.push((x_cross, dir));
                }
            }
        }
        crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut winding = 0i32;
        let mut ci = 0;
        for px in 0..width {
            let x = px as f64 / width as f64;
            while ci < crossings.len() && crossings[ci].0 <= x {
                winding += crossings[ci].1;
                ci += 1;
            }
            if winding != 0 {
                mask[py * width + px] = true;
            }
        }
    }
    mask
}

fn render_frame(
    image: &LoadedImage,
    contours: &[Vec<[f64; 2]>],
    mask: &[bool],
    mask_size: (usize, usize),
    out_w: usize,
    out_h: usize,
    show_contour: bool,
    show_bfs_fill: bool,
    alpha_threshold: u8,
    params: &ContourParams,
    computing: bool,
) -> Vec<u32> {
    let mut fb = vec![0u32; out_w * out_h];

    let img_aspect = image.width as f64 / image.height as f64;
    let out_aspect = out_w as f64 / out_h as f64;
    let (draw_w, draw_h) = if img_aspect > out_aspect {
        let dw = (out_w as f64 * 0.9) as usize;
        let dh = (dw as f64 / img_aspect) as usize;
        (dw, dh)
    } else {
        let dh = (out_h as f64 * 0.9) as usize;
        let dw = (dh as f64 * img_aspect) as usize;
        (dw, dh)
    };
    let draw_w = draw_w.max(1);
    let draw_h = draw_h.max(1);
    let ox = (out_w.saturating_sub(draw_w)) / 2;
    let oy = (out_h.saturating_sub(draw_h)) / 2;

    let (mask_w, mask_h) = mask_size;

    for py in 0..draw_h {
        let v = py as f64 / draw_h as f64;
        let ty = ((v * image.height as f64) as u32).min(image.height - 1);
        let my = ((v * mask_h as f64) as usize).min(mask_h.saturating_sub(1));
        for px_x in 0..draw_w {
            let u = px_x as f64 / draw_w as f64;
            let mx = ((u * mask_w as f64) as usize).min(mask_w.saturating_sub(1));
            let inside = mask_w > 0 && mask_h > 0 && mask[my * mask_w + mx];
            let out_idx = (oy + py) * out_w + (ox + px_x);

            if !inside {
                continue;
            }

            let tx = ((u * image.width as f64) as u32).min(image.width - 1);
            let idx = (ty * image.width + tx) as usize;
            let [r, g, b, a] = image.pixels[idx];

            let (fr, fg, fb_c) = if a < alpha_threshold {
                if show_bfs_fill {
                    (r, g, b)
                } else {
                    (255u8, 0u8, 200u8)
                }
            } else if show_bfs_fill {
                (r, g, b)
            } else {
                let alpha = a as f64 / 255.0;
                let inv = 1.0 - alpha;
                (
                    (r as f64 * alpha + 255.0 * inv) as u8,
                    (g as f64 * alpha + 0.0 * inv) as u8,
                    (b as f64 * alpha + 200.0 * inv) as u8,
                )
            };

            fb[out_idx] = ((fr as u32) << 16) | ((fg as u32) << 8) | (fb_c as u32);
        }
    }

    if show_contour {
        for ring in contours {
            for i in 0..ring.len() {
                let j = (i + 1) % ring.len();
                let [u0, v0] = ring[i];
                let [u1, v1] = ring[j];
                let x0 = ox as f64 + u0 * draw_w as f64;
                let y0 = oy as f64 + v0 * draw_h as f64;
                let x1 = ox as f64 + u1 * draw_w as f64;
                let y1 = oy as f64 + v1 * draw_h as f64;
                draw_line(&mut fb, out_w, out_h, x0, y0, x1, y1, 0x00FF00);
            }

            for &[u, v] in ring {
                let cx = (ox as f64 + u * draw_w as f64) as isize;
                let cy = (oy as f64 + v * draw_h as f64) as isize;
                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        let px = cx + dx;
                        let py = cy + dy;
                        if px >= 0 && (px as usize) < out_w && py >= 0 && (py as usize) < out_h {
                            fb[py as usize * out_w + px as usize] = 0xFFFF00;
                        }
                    }
                }
            }
        }
    }

    // Slider UI
    let slider_y = out_h.saturating_sub(80);
    draw_slider(
        &mut fb,
        out_w,
        out_h,
        20,
        slider_y,
        out_w.saturating_sub(40),
        "GRID CELLS",
        params.max_cells as f64,
        4.0,
        1024.0,
        params.active_slider == 0,
    );
    draw_slider(
        &mut fb,
        out_w,
        out_h,
        20,
        slider_y + 30,
        out_w.saturating_sub(40),
        "ALPHA THRESH",
        params.alpha_threshold as f64,
        1.0,
        255.0,
        params.active_slider == 1,
    );

    // HUD
    let status = if computing { "  COMPUTING..." } else { "" };
    let hud = [
        format!("Image: {} ({}x{})", image.label, image.width, image.height),
        format!(
            "Rings: {}  Verts: {}{}",
            contours.len(),
            contours.iter().map(|r| r.len()).sum::<usize>(),
            status,
        ),
        format!(
            "Grid: {}  Threshold: {}",
            params.max_cells, params.alpha_threshold
        ),
        format!(
            "[c] contour: {}  [b] bfs fill: {}  [Tab] slider  [</>] adjust",
            if show_contour { "ON" } else { "OFF" },
            if show_bfs_fill { "ON" } else { "OFF" },
        ),
    ];
    for (i, line) in hud.iter().enumerate() {
        let color = if i == 1 && computing {
            0xFFAA00
        } else {
            0xFFFFFF
        };
        draw_text_simple(&mut fb, out_w, out_h, 10, 10 + i * 14, line, color);
    }

    fb
}

fn draw_slider(
    fb: &mut [u32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    slider_w: usize,
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    active: bool,
) {
    let label_w = label.len() * 8 + 8;
    let val_str = format!("{}", value as u32);
    let val_w = val_str.len() * 8 + 8;
    let track_x = x + label_w;
    let track_w = slider_w.saturating_sub(label_w + val_w);
    let track_y = y + 4;

    let label_color = if active { 0x00FFAA } else { 0xAAAAAA };
    draw_text_simple(fb, w, h, x, y, label, label_color);
    draw_text_simple(fb, w, h, track_x + track_w + 8, y, &val_str, 0xFFFFFF);

    // Track background
    let track_color = if active { 0x444444 } else { 0x333333 };
    for py in track_y..=(track_y + 3).min(h - 1) {
        for px in track_x..(track_x + track_w).min(w) {
            fb[py * w + px] = track_color;
        }
    }

    // Thumb
    let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let thumb_x = track_x + (t * (track_w.saturating_sub(1)) as f64) as usize;
    let thumb_color = if active { 0x00FF88 } else { 0x888888 };
    for py in (track_y.saturating_sub(2))..=(track_y + 5).min(h - 1) {
        for px in thumb_x.saturating_sub(3)..(thumb_x + 4).min(w) {
            fb[py * w + px] = thumb_color;
        }
    }
}

fn draw_line(fb: &mut [u32], w: usize, h: usize, x0: f64, y0: f64, x1: f64, y1: f64, color: u32) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).ceil() as usize;
    if steps == 0 {
        return;
    }
    for s in 0..=steps {
        let t = s as f64 / steps as f64;
        let px = (x0 + dx * t).round() as isize;
        let py = (y0 + dy * t).round() as isize;
        if px >= 0 && (px as usize) < w && py >= 0 && (py as usize) < h {
            fb[py as usize * w + px as usize] = color;
        }
    }
}

fn draw_text_simple(
    fb: &mut [u32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    text: &str,
    color: u32,
) {
    let mut cx = x;
    for ch in text.chars() {
        let glyph = glyph_pattern(ch);
        for (row, &pattern) in glyph.iter().enumerate() {
            for col in 0..3usize {
                if (pattern >> (2 - col)) & 1 != 0 {
                    for sy in 0..2usize {
                        for sx in 0..2usize {
                            let px = cx + col * 2 + sx;
                            let py = y + row * 2 + sy;
                            if px < w && py < h {
                                fb[py * w + px] = color;
                            }
                        }
                    }
                }
            }
        }
        cx += 8;
    }
}

fn glyph_pattern(ch: char) -> [u8; 5] {
    match ch.to_ascii_uppercase() {
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
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '(' => [0b010, 0b100, 0b100, 0b100, 0b010],
        ')' => [0b010, 0b001, 0b001, 0b001, 0b010],
        '[' => [0b110, 0b100, 0b100, 0b100, 0b110],
        ']' => [0b011, 0b001, 0b001, 0b001, 0b011],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b101, 0b010, 0b000, 0b010],
    }
}

struct ContourParams {
    max_cells: usize,
    alpha_threshold: u8,
    active_slider: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ContourRequest {
    max_cells: usize,
    alpha_threshold: u8,
}

const MASK_SIZE: usize = 512;

struct ContourResult {
    request: ContourRequest,
    rings: Vec<Vec<[f64; 2]>>,
    mask: Vec<bool>,
    mask_w: usize,
    mask_h: usize,
}

struct ContourWorker {
    request_tx: mpsc::Sender<ContourRequest>,
    result_rx: mpsc::Receiver<ContourResult>,
}

impl ContourWorker {
    fn spawn(image: Arc<LoadedImage>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<ContourRequest>();
        let (result_tx, result_rx) = mpsc::channel();

        std::thread::spawn(move || {
            while let Ok(req) = request_rx.recv() {
                let mut latest = req;
                while let Ok(newer) = request_rx.try_recv() {
                    latest = newer;
                }
                let rings = extract_contours(&image, latest.max_cells, latest.alpha_threshold);
                let aspect = image.width as f64 / image.height.max(1) as f64;
                let (mask_w, mask_h) = if aspect >= 1.0 {
                    (MASK_SIZE, (MASK_SIZE as f64 / aspect).max(1.0) as usize)
                } else {
                    ((MASK_SIZE as f64 * aspect).max(1.0) as usize, MASK_SIZE)
                };
                let mask = rasterize_contour_mask(&rings, mask_w, mask_h);
                if result_tx
                    .send(ContourResult {
                        request: latest,
                        rings,
                        mask,
                        mask_w,
                        mask_h,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            request_tx,
            result_rx,
        }
    }

    fn request(&self, req: ContourRequest) {
        let _ = self.request_tx.send(req);
    }

    fn poll(&self) -> Option<ContourResult> {
        let mut latest = None;
        while let Ok(result) = self.result_rx.try_recv() {
            latest = Some(result);
        }
        latest
    }
}

struct App {
    image: Arc<LoadedImage>,
    contours: Vec<Vec<[f64; 2]>>,
    mask: Vec<bool>,
    mask_size: (usize, usize),
    contour_request: ContourRequest,
    computing: bool,
    params: ContourParams,
    worker: ContourWorker,
    window: Option<Arc<Window>>,
    surface: Option<(
        SoftbufferContext<Arc<Window>>,
        SoftbufferSurface<Arc<Window>, Arc<Window>>,
    )>,
    window_id: Option<WindowId>,
    show_contour: bool,
    show_bfs_fill: bool,
    exit_error: Option<anyhow::Error>,
}

impl App {
    fn new(image: LoadedImage) -> Self {
        let image = Arc::new(image);
        let params = ContourParams {
            max_cells: 256,
            alpha_threshold: 160,
            active_slider: 0,
        };
        let initial_req = ContourRequest {
            max_cells: params.max_cells,
            alpha_threshold: params.alpha_threshold,
        };
        let worker = ContourWorker::spawn(Arc::clone(&image));
        worker.request(initial_req);
        Self {
            image,
            contours: Vec::new(),
            mask: Vec::new(),
            mask_size: (0, 0),
            contour_request: initial_req,
            computing: true,
            params,
            worker,
            window: None,
            surface: None,
            window_id: None,
            show_contour: true,
            show_bfs_fill: false,
            exit_error: None,
        }
    }

    fn request_recompute(&mut self) {
        let req = ContourRequest {
            max_cells: self.params.max_cells,
            alpha_threshold: self.params.alpha_threshold,
        };
        if req != self.contour_request {
            self.contour_request = req;
            self.computing = true;
            self.worker.request(req);
        }
    }

    fn adjust_active_slider(&mut self, delta: i32) {
        match self.params.active_slider {
            0 => {
                let new = (self.params.max_cells as i32 + delta).clamp(4, 1024) as usize;
                self.params.max_cells = new;
            }
            1 => {
                let new = (self.params.alpha_threshold as i32 + delta).clamp(1, 255) as u8;
                self.params.alpha_threshold = new;
            }
            _ => {}
        }
        self.request_recompute();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);
        if self.window.is_some() {
            return;
        }

        let title = format!("Contour Viewer — {}", self.image.label);
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(PhysicalSize::new(800, 800));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.exit_error = Some(anyhow!(e));
                event_loop.exit();
                return;
            }
        };

        let ctx = match SoftbufferContext::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                self.exit_error = Some(anyhow!(e.to_string()));
                event_loop.exit();
                return;
            }
        };
        let mut surface = match SoftbufferSurface::new(&ctx, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.exit_error = Some(anyhow!(e.to_string()));
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let sw = NonZeroU32::new(size.width.max(1)).unwrap();
        let sh = NonZeroU32::new(size.height.max(1)).unwrap();
        let _ = surface.resize(sw, sh);

        self.window_id = Some(window.id());
        self.surface = Some((ctx, surface));
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
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                let mut redraw = false;
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                } else if event.logical_key == Key::Named(NamedKey::Tab) {
                    self.params.active_slider = (self.params.active_slider + 1) % 2;
                    redraw = true;
                } else if event.logical_key == Key::Named(NamedKey::ArrowLeft) {
                    let step = if event.repeat { 4 } else { 1 };
                    self.adjust_active_slider(-step);
                    redraw = true;
                } else if event.logical_key == Key::Named(NamedKey::ArrowRight) {
                    let step = if event.repeat { 4 } else { 1 };
                    self.adjust_active_slider(step);
                    redraw = true;
                } else if let Some(text) = &event.text {
                    match text.as_str() {
                        "c" => {
                            self.show_contour = !self.show_contour;
                            redraw = true;
                        }
                        "b" => {
                            self.show_bfs_fill = !self.show_bfs_fill;
                            redraw = true;
                        }
                        _ => {}
                    }
                }
                if redraw {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some((_, surface)) = &mut self.surface {
                    let w = NonZeroU32::new(size.width.max(1)).unwrap();
                    let h = NonZeroU32::new(size.height.max(1)).unwrap();
                    let _ = surface.resize(w, h);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(result) = self.worker.poll() {
                    self.computing = result.request != self.contour_request;
                    self.contours = result.rings;
                    self.mask = result.mask;
                    self.mask_size = (result.mask_w, result.mask_h);
                }

                let Some(window) = &self.window else { return };
                let Some((_, surface)) = &mut self.surface else {
                    return;
                };
                let size = window.inner_size();
                let (w, h) = (size.width as usize, size.height as usize);
                if w == 0 || h == 0 {
                    return;
                }
                let pixels = render_frame(
                    &self.image,
                    &self.contours,
                    &self.mask,
                    self.mask_size,
                    w,
                    h,
                    self.show_contour,
                    self.show_bfs_fill,
                    self.params.alpha_threshold,
                    &self.params,
                    self.computing,
                );
                let Ok(mut buffer) = surface.buffer_mut() else {
                    return;
                };
                buffer.copy_from_slice(&pixels);
                let _ = buffer.present();
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
