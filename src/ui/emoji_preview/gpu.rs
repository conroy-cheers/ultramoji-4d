pub use emoji_renderer::gpu::*;

use anyhow::Result;
use ratatui::text::Line;

use super::common::fb_to_lines;

pub fn try_new() -> Result<GpuRenderer> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let adapter = request_adapter(&instance, None)?;
    from_adapter(adapter)
}

pub fn request_adapter(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter> {
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface,
    }))
    .ok_or_else(|| anyhow::anyhow!("no wgpu adapter available"))
}

pub fn from_adapter(adapter: wgpu::Adapter) -> Result<GpuRenderer> {
    tracing::info!("wgpu adapter: {:?}", adapter.get_info().name);
    let adapter_limits = adapter.limits();

    let mut features = wgpu::Features::empty();
    if adapter
        .features()
        .contains(wgpu::Features::POLYGON_MODE_LINE)
    {
        features |= wgpu::Features::POLYGON_MODE_LINE;
    }

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("emoji_preview"),
            required_features: features,
            required_limits: adapter_limits,
            ..Default::default()
        },
        None,
    ))?;

    let linear_depth_format = if adapter
        .get_texture_format_features(wgpu::TextureFormat::R32Float)
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
    {
        wgpu::TextureFormat::R32Float
    } else {
        tracing::warn!(
            "R32Float is not renderable on this adapter; falling back to R16Float for linear depth"
        );
        wgpu::TextureFormat::R16Float
    };

    let independent_blend_supported = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::INDEPENDENT_BLEND);

    GpuRenderer::from_device_queue(
        device,
        queue,
        features,
        linear_depth_format,
        independent_blend_supported,
    )
}

pub fn render_billboard(
    gpu: &mut GpuRenderer,
    frame_idx: usize,
    tex_w: u32,
    tex_h: u32,
    width: usize,
    height: usize,
    time_secs: f64,
) -> Vec<Line<'static>> {
    let px_w = width;
    let px_h = height * 2;
    let mut params = emoji_preview_scene_params();
    params.sharpen = Some(0.1);
    params.dither = Some(0.3);
    params.vhs = Some(0.5);
    params.jitter = Some(0.1);
    params.supersample = true;
    let fb =
        gpu.readback_offscreen_animated(frame_idx, tex_w, tex_h, px_w, px_h, time_secs, &params);
    fb_to_lines(&fb, px_w, px_h, height)
}
