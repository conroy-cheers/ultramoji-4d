#[derive(Clone, Copy, Debug, Default)]
pub struct PerfPanelData {
    pub smoothed_fps: f32,
    pub smoothed_frame_cpu_ms: f32,
    pub smoothed_frame_interval_ms: f32,
    pub smoothed_surface_acquire_ms: f32,
    pub smoothed_terminal_ms: f32,
    pub smoothed_screen_ms: f32,
    pub smoothed_scene_ms: f32,
    pub smoothed_egui_ms: f32,
    pub smoothed_composite_ms: f32,
    pub window_width: u32,
    pub window_height: u32,
    pub surface_width: u32,
    pub surface_height: u32,
    pub terminal_width: u32,
    pub terminal_height: u32,
    pub scale_factor: f32,
    pub preview_mix: f32,
    pub egui_paint_jobs: u32,
    pub egui_textures_delta: u32,
    pub last_screen_redrew: bool,
    pub last_previewing: bool,
    pub last_uses_billboard: bool,
    pub offscreen_stats: Option<emoji_renderer::gpu::OffscreenPerfStats>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PanelActions {
    pub sign_out_requested: bool,
}

pub fn show_controls_panel(
    ctx: &egui::Context,
    transfer: &mut TransferTuning,
    render_config: &mut RenderConfig,
    perf: PerfPanelData,
) -> PanelActions {
    use egui::{Align2, CollapsingHeader, ComboBox, RichText, Slider};

    let actions = PanelActions::default();

    egui::Window::new("Controls")
        .default_pos([10.0, 10.0])
        .resizable(false)
        .show(ctx, |ui| {
            CollapsingHeader::new("Panel")
                .default_open(false)
                .show(ui, |ui| {
                    ui.heading("Transfer");
                    ui.add(Slider::new(&mut transfer.linear_gain, 0.70..=1.40).text("Linear Gain"));
                    ui.add(Slider::new(&mut transfer.gamma, 0.70..=1.30).text("Gamma"));
                    ui.add(Slider::new(&mut transfer.lift, -0.08..=0.08).text("Lift"));
                    ui.add(Slider::new(&mut transfer.saturation, 0.50..=1.80).text("Saturation"));

                    ui.separator();
                    ui.heading("Render");
                    ComboBox::from_label("Display Scaling")
                        .selected_text(if render_config.display_pixelated {
                            "Pixelated"
                        } else {
                            "Smooth"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut render_config.display_pixelated, false, "Smooth");
                            ui.selectable_value(&mut render_config.display_pixelated, true, "Pixelated");
                        });
                    ComboBox::from_label("Terminal Sampling")
                        .selected_text(if render_config.overlay_filter {
                            "Filtered"
                        } else {
                            "Nearest"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut render_config.overlay_filter, true, "Filtered");
                            ui.selectable_value(&mut render_config.overlay_filter, false, "Nearest");
                        });
                    ui.add(Slider::new(&mut render_config.gallery_canvas_scale, 0.25..=1.50).text("Gallery Canvas Res"));
                    ui.add(Slider::new(&mut render_config.preview_canvas_scale, 0.25..=1.50).text("Preview Canvas Res"));
                    ui.add(
                        Slider::new(&mut render_config.preview_max_dim, 96..=640)
                            .step_by(4.0)
                            .text("Preview Res"),
                    );
                    ui.add(
                        Slider::new(&mut render_config.preview_render_scale, 1.0..=3.0)
                            .step_by(0.25)
                            .text("Preview SSAA"),
                    );

                    ui.separator();
                    ui.heading("Perf");
                    ui.monospace(format!(
                        "{:>3} FPS\nFRAME {:>4.1} ms\nINTVL {:>4.1} ms\nACQ   {:>4.1} ms\nTERM  {:>4.1} ms\nSCREEN {:>4.1} ms {}\n3D    {:>4.1} ms\nEGUI  {:>4.1} ms\nCOMP  {:>4.1} ms",
                        perf.smoothed_fps.round().clamp(0.0, 999.0) as u32,
                        perf.smoothed_frame_cpu_ms,
                        perf.smoothed_frame_interval_ms,
                        perf.smoothed_surface_acquire_ms,
                        perf.smoothed_terminal_ms,
                        perf.smoothed_screen_ms,
                        if perf.last_screen_redrew { "*" } else { "-" },
                        perf.smoothed_scene_ms,
                        perf.smoothed_egui_ms,
                        perf.smoothed_composite_ms,
                    ));
                    ui.separator();
                    ui.monospace(format!(
                        "WIN  {}x{} @ {:.2}\nSURF {}x{}\nTERM {}x{}\nMODE {} mix {:.2}\nBILL {}\nEGUI jobs {} tex {}",
                        perf.window_width,
                        perf.window_height,
                        perf.scale_factor,
                        perf.surface_width,
                        perf.surface_height,
                        perf.terminal_width,
                        perf.terminal_height,
                        if perf.last_previewing { "preview" } else { "gallery" },
                        perf.preview_mix,
                        if perf.last_uses_billboard { "on" } else { "off" },
                        perf.egui_paint_jobs,
                        perf.egui_textures_delta,
                    ));
                    if let Some(stats) = perf.offscreen_stats {
                        ui.separator();
                        ui.monospace(format!(
                            "3D scene {}x{}\n3D out  {}x{}\n3D passes {}\n3D draws  {}\n3D downsample {}",
                            stats.scene_width,
                            stats.scene_height,
                            stats.output_width,
                            stats.output_height,
                            stats.pass_count,
                            stats.draw_call_count,
                            if stats.has_downsample { "yes" } else { "no" },
                        ));
                    }
                });
        });

    egui::Area::new("fps_overlay".into())
        .anchor(Align2::RIGHT_BOTTOM, [-12.0, -12.0])
        .show(ctx, |ui| {
            let fps_label = perf.smoothed_fps.round().clamp(0.0, 999.0) as u32;
            ui.label(
                RichText::new(format!(
                    "{fps_label:>3} FPS\nFRAME {:>4.1} MS\nSCREEN {}\n3D {:>4.1} MS\nCOMP {:>4.1} MS",
                    perf.smoothed_frame_cpu_ms,
                    if perf.last_screen_redrew { "*" } else { "-" },
                    perf.smoothed_scene_ms,
                    perf.smoothed_composite_ms,
                ))
                .monospace(),
            );
        });

    actions
}
