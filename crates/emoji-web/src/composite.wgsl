struct Uniforms {
    output_size: vec2f,
    time_secs: f32,
    preview_mix: f32,
    terminal_rect: vec4f,
    overlay_uv_rect: vec4f,
    billboard_rect: vec4f,
    terminal_grid: vec4f,
    transfer_tuning: vec4f,
    perf_toggles: vec4f,
    channel_switch: vec4f,
}

@group(0) @binding(0) var overlay_tex: texture_2d<f32>;
@group(0) @binding(1) var overlay_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;
@group(0) @binding(3) var billboard_tex: texture_2d<f32>;
@group(0) @binding(4) var billboard_sampler: sampler;

fn quantize_u8(c: f32) -> f32 {
    return floor(clamp(c, 0.0, 1.0) * 255.0 + 0.5) / 255.0;
}

fn linear_to_srgb_channel_exact(c: f32) -> f32 {
    let q = quantize_u8(c);
    var s = q * 12.92;
    if q > 0.0031308 {
        s = 1.055 * pow(max(q, 0.0), 1.0 / 2.4) - 0.055;
    }
    return quantize_u8(s);
}

fn linear_to_srgb(c: vec3f) -> vec3f {
    return vec3f(
        linear_to_srgb_channel_exact(c.r),
        linear_to_srgb_channel_exact(c.g),
        linear_to_srgb_channel_exact(c.b),
    );
}

fn apply_transfer_tuning(c: vec3f) -> vec3f {
    let gain = max(u.transfer_tuning.x, 0.0);
    let gamma = max(u.transfer_tuning.y, 0.01);
    let lift = u.transfer_tuning.z;
    let saturation = max(u.transfer_tuning.w, 0.0);
    let gained = clamp(c * gain, vec3f(0.0), vec3f(1.0));
    let encoded = linear_to_srgb(gained);
    let lifted = clamp(encoded + vec3f(lift), vec3f(0.0), vec3f(1.0));
    let gamma_corrected = clamp(vec3f(
        pow(lifted.r, 1.0 / gamma),
        pow(lifted.g, 1.0 / gamma),
        pow(lifted.b, 1.0 / gamma),
    ), vec3f(0.0), vec3f(1.0));
    let luma = dot(gamma_corrected, vec3f(0.2126, 0.7152, 0.0722));
    return clamp(mix(vec3f(luma), gamma_corrected, saturation), vec3f(0.0), vec3f(1.0));
}

fn sample_filtered(
    tex: texture_2d<f32>,
    samp: sampler,
    uv: vec2f,
    texel: vec2f,
) -> vec4f {
    let center = textureSampleLevel(tex, samp, uv, 0.0) * 0.40;
    let horiz =
        textureSampleLevel(tex, samp, uv + vec2f(texel.x, 0.0), 0.0) * 0.15 +
        textureSampleLevel(tex, samp, uv - vec2f(texel.x, 0.0), 0.0) * 0.15;
    let vert =
        textureSampleLevel(tex, samp, uv + vec2f(0.0, texel.y), 0.0) * 0.15 +
        textureSampleLevel(tex, samp, uv - vec2f(0.0, texel.y), 0.0) * 0.15;
    return center + horiz + vert;
}

fn sample_billboard_scene(uv: vec2f) -> vec3f {
    return textureSampleLevel(
        billboard_tex,
        billboard_sampler,
        clamp(uv, vec2f(0.0), vec2f(1.0)),
        0.0,
    ).rgb;
}

fn hash12(p: vec2f) -> f32 {
    let h = dot(p, vec2f(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

fn analog_tv_static(uv: vec2f, pane_px: vec2f) -> vec3f {
    let frame = floor(u.time_secs * 60.0);
    let px = floor(pane_px);
    let pair_cell = vec2f(floor(px.x * 0.5), px.y);
    let pair_seed = hash12(pair_cell + vec2f(frame * 19.0, frame * 7.0));
    let pair_side = fract(px.x * 0.5) >= 0.5;
    let paired_snow = select(pair_seed, 1.0 - pair_seed, pair_side);
    let hard_snow = select(0.04, 0.96, paired_snow > 0.50);
    let edge = smoothstep(0.0, 0.018, uv.x) *
        smoothstep(0.0, 0.018, uv.y) *
        smoothstep(0.0, 0.018, 1.0 - uv.x) *
        smoothstep(0.0, 0.018, 1.0 - uv.y);
    let shade = mix(paired_snow, hard_snow, 0.88);
    let triad = select(
        vec3f(1.08, 0.94, 0.94),
        select(vec3f(0.94, 1.08, 0.94), vec3f(0.94, 0.94, 1.08), fract(px.x / 3.0) > 0.66),
        fract(px.x / 3.0) > 0.33,
    );
    let vignette = mix(0.78, 1.0, edge);
    return clamp(vec3f(shade) * mix(vec3f(1.0), triad, 0.08) * vignette, vec3f(0.0), vec3f(1.0));
}

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
fn fs_screen(in: VsOut) -> @location(0) vec4f {
    let term_uv = clamp(in.uv, vec2f(0.0), vec2f(1.0));
    let overlay_texel = 1.0 / vec2f(textureDimensions(overlay_tex));
    var overlay = textureSampleLevel(overlay_tex, overlay_sampler, term_uv, 0.0);
    if u.perf_toggles.z > 0.5 {
        overlay = sample_filtered(overlay_tex, overlay_sampler, term_uv, overlay_texel * 0.65);
    }

    var color = overlay.rgb;

    color = clamp(color, vec3f(0.0), vec3f(1.0));
    if u.perf_toggles.y > 0.5 {
        color = apply_transfer_tuning(color);
    }

    return vec4f(color, overlay.a);
}

@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4f {
    let frag_px = in.uv * u.output_size;
    let channel = u.channel_switch.x;
    let channel_dir = select(-1.0, 1.0, u.channel_switch.y >= 0.0);
    let use_preview_source = u.preview_mix >= 0.5;
    let load_error = u.channel_switch.z > 0.5 && use_preview_source;
    let bg_t = clamp(length((frag_px - u.output_size * 0.5) / u.output_size), 0.0, 1.0);
    let gallery_bg = vec3f(0.0, 0.0, 0.0);
    let preview_bg = mix(vec3f(0.015, 0.015, 0.02), vec3f(0.0, 0.0, 0.0), bg_t);
    var gallery_color = gallery_bg;
    var preview_color = preview_bg;

    let row_wobble =
        (sin(in.uv.y * 180.0 + u.time_secs * 44.0) * 0.018 +
         sin(in.uv.y * 37.0 - u.time_secs * 19.0) * 0.008 +
         channel_dir * (in.uv.y - 0.5) * 0.03) * channel;

    let local = vec2f(frag_px.x + row_wobble * u.billboard_rect.z, frag_px.y) - u.billboard_rect.xy;
    let bb_uv = vec2f(
        clamp(local.x / max(u.billboard_rect.z, 1.0), 0.0, 1.0),
        clamp(local.y / max(u.billboard_rect.w, 1.0), 0.0, 1.0),
    );
    let inside = u.billboard_rect.z > 0.0 && u.billboard_rect.w > 0.0
        && local.x >= 0.0 && local.y >= 0.0
        && local.x < u.billboard_rect.z && local.y < u.billboard_rect.w;
    if inside && !load_error {
        let bb = sample_billboard_scene(bb_uv);
        preview_color = bb;
    }

    let term_local = vec2f(frag_px.x + row_wobble * u.terminal_rect.z, frag_px.y) - u.terminal_rect.xy;
    let term_local_uv = vec2f(
        clamp(term_local.x / max(u.terminal_rect.z, 1.0), 0.0, 1.0),
        clamp(term_local.y / max(u.terminal_rect.w, 1.0), 0.0, 1.0),
    );
    let term_uv = u.overlay_uv_rect.xy + term_local_uv * u.overlay_uv_rect.zw;
    let in_term = term_local.x >= 0.0 && term_local.y >= 0.0
        && term_local.x < u.terminal_rect.z && term_local.y < u.terminal_rect.w;
    if in_term {
        if load_error {
            preview_color = analog_tv_static(term_local_uv, term_local);
        }
        let screen = textureSampleLevel(overlay_tex, overlay_sampler, term_uv, 0.0);
        gallery_color = mix(gallery_color, screen.rgb, screen.a);
        preview_color = mix(preview_color, screen.rgb, screen.a);
    }

    var color = select(gallery_color, preview_color, use_preview_source);

    if u.perf_toggles.x > 0.5 {
        let switch_phase = pow(1.0 - abs(u.preview_mix * 2.0 - 1.0), 1.35);
        let switching = f32(u.preview_mix > 0.001 && u.preview_mix < 0.999);
        let base_crt_strength = select(1.0, 0.0, use_preview_source && !load_error);
        let crt_strength = max(base_crt_strength, switch_phase);
        let collapse = pow(switch_phase, 1.10);
        let collapse_narrow = pow(collapse, 3.10);
        let collapse_wobble =
            sin(u.time_secs * 31.0 + in.uv.y * 46.0) * 0.0045 * collapse +
            sin(u.time_secs * 19.0 - in.uv.y * 83.0) * 0.0025 * collapse;
        let band_dist = abs((in.uv.y - 0.5) + collapse_wobble);
        let aperture = mix(1.12, 0.007, collapse_narrow);
        let edge_softness = mix(0.042, 0.010, collapse_narrow);
        let visible = 1.0 - smoothstep(aperture, aperture + edge_softness, band_dist);
        let center_hold = 1.0 - smoothstep(0.0, 0.16, band_dist);
        let h_center_dist = abs(in.uv.x - 0.5);
        let horiz_glow = 0.56 + 0.44 * exp(-h_center_dist * 4.2);
        let horiz_core = 0.30 + 0.70 * exp(-h_center_dist * 9.5);
        let line_glow =
            exp(-band_dist * mix(16.0, 250.0, collapse_narrow)) *
            mix(0.14, 1.10, collapse) *
            (0.85 + center_hold * 0.25) *
            horiz_glow;
        let line_core =
            exp(-band_dist * mix(26.0, 760.0, collapse_narrow)) *
            (0.35 + collapse * 0.95) *
            (0.85 + center_hold * 0.45) *
            horiz_core;
        let retrace =
            smoothstep(0.68, 1.0, sin(in.uv.y * 170.0 - u.time_secs * 42.0) * 0.5 + 0.5) *
            collapse * 0.10;
        let edge_vignette =
            smoothstep(0.0, 0.10, in.uv.x) *
            smoothstep(0.0, 0.10, in.uv.y) *
            smoothstep(0.0, 0.10, 1.0 - in.uv.x) *
            smoothstep(0.0, 0.10, 1.0 - in.uv.y);
        let switched =
            color * visible * (0.90 + edge_vignette * 0.10) * (1.0 - retrace) +
            vec3f(1.0, 0.97, 0.84) * (line_glow * 0.28 + line_core * 0.55);
        color = mix(color, switched, switching);

        let source_row_pos = in.uv.y * max(u.output_size.y * 0.5, 1.0);
        let row_phase = fract(source_row_pos);
        let row_center_dist = abs(row_phase - 0.5);
        let row_core = 1.0 - smoothstep(0.10, 0.46, row_center_dist);
        let scan = 1.0 - crt_strength * (0.34 - row_core * 0.34);
        let beam = 1.0 - crt_strength * 0.06
            + crt_strength * 0.06 * sin((floor(source_row_pos) + 0.5) * 0.9 + u.time_secs * 0.9);
        let phosphor_gain = 1.0 + row_core * 0.16 * crt_strength;
        let flicker = 1.0 - crt_strength * 0.02 + crt_strength * 0.02 * sin(u.time_secs * 37.0 + in.uv.y * 11.0);
        let triad_phase = fract((frag_px.x + frag_px.y * 0.25) / 3.0);
        let triad_mask =
            select(
                vec3f(1.03, 0.985, 0.985),
                select(vec3f(0.985, 1.03, 0.985), vec3f(0.985, 0.985, 1.03), triad_phase > 0.666),
                triad_phase > 0.333,
            );
        let edge =
            smoothstep(0.0, 0.08, in.uv.x) *
            smoothstep(0.0, 0.08, in.uv.y) *
            smoothstep(0.0, 0.08, 1.0 - in.uv.x) *
            smoothstep(0.0, 0.08, 1.0 - in.uv.y);
        color *= scan * beam * flicker * phosphor_gain;
        color *= mix(vec3f(1.0), triad_mask, crt_strength * 0.06);
        color = mix(color * (1.0 - 0.12 * crt_strength), color, edge);
    }

    if channel > 0.0 {
        let noise_coord = floor(vec2f(in.uv.x * u.output_size.x * 0.65, in.uv.y * u.output_size.y * 0.35) + vec2f(u.time_secs * 120.0, u.time_secs * 53.0));
        let noise = hash12(noise_coord);
        let burst = smoothstep(0.25, 1.0, sin(in.uv.y * 96.0 - u.time_secs * 31.0) * 0.5 + 0.5);
        let static_mix = channel * (0.30 + burst * 0.28);
        let monochrome = vec3f(dot(color, vec3f(0.2126, 0.7152, 0.0722)));
        color = mix(color, monochrome, channel * 0.45);
        color = mix(color, vec3f(noise), static_mix * 0.55);
        color *= 1.0 - channel * 0.08 + burst * channel * 0.22;
    }

    return vec4f(clamp(color, vec3f(0.0), vec3f(1.0)), 1.0);
}
