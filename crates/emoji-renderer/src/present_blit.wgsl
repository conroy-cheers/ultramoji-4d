struct Uniforms {
    output_size: vec2f,
    opacity: f32,
    apply_transfer: f32,
    dest_rect: vec4f,
    uv_rect: vec4f,
    transfer_tuning: vec4f,
    extra_params: vec4f,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) screen_uv: vec2f,
}

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

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var quad = array<vec2f, 6>(
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
        vec2f(0.0, 0.0),
        vec2f(1.0, 1.0),
        vec2f(0.0, 1.0),
    );
    let p = quad[index];
    let pixel = u.dest_rect.xy + p * u.dest_rect.zw;
    let ndc = vec2f(
        (pixel.x / u.output_size.x) * 2.0 - 1.0,
        1.0 - (pixel.y / u.output_size.y) * 2.0,
    );
    let src_p = vec2f(p.x, select(p.y, 1.0 - p.y, u.extra_params.x > 0.5));
    var out: VsOut;
    out.position = vec4f(ndc, 0.0, 1.0);
    out.uv = u.uv_rect.xy + src_p * u.uv_rect.zw;
    out.screen_uv = pixel / u.output_size;
    return out;
}

fn hash12(p: vec2f) -> f32 {
    let h = dot(p, vec2f(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let channel = u.extra_params.y;
    let channel_dir = select(-1.0, 1.0, u.extra_params.z >= 0.0);
    let time_secs = u.extra_params.w;
    let wobble =
        (sin(in.screen_uv.y * 180.0 + time_secs * 44.0) * 0.018 +
         sin(in.screen_uv.y * 37.0 - time_secs * 19.0) * 0.008 +
         channel_dir * (in.screen_uv.y - 0.5) * 0.03) * channel;
    let sample_uv = vec2f(clamp(in.uv.x + wobble, 0.0, 1.0), clamp(in.uv.y, 0.0, 1.0));
    let sample = textureSampleLevel(src_tex, src_sampler, sample_uv, 0.0);
    let color = select(sample.rgb, apply_transfer_tuning(sample.rgb), u.apply_transfer > 0.5);
    let noise_coord = floor(vec2f(in.screen_uv.x * u.output_size.x * 0.65, in.screen_uv.y * u.output_size.y * 0.35) + vec2f(time_secs * 120.0, time_secs * 53.0));
    let noise = hash12(noise_coord);
    let burst = smoothstep(0.25, 1.0, sin(in.screen_uv.y * 96.0 - time_secs * 31.0) * 0.5 + 0.5);
    let monochrome = vec3f(dot(color, vec3f(0.2126, 0.7152, 0.0722)));
    var final_color = mix(color, monochrome, channel * 0.45);
    final_color = mix(final_color, vec3f(noise), channel * (0.17 + burst * 0.15));
    final_color *= 1.0 - channel * 0.08 + burst * channel * 0.22;
    return vec4f(clamp(final_color, vec3f(0.0), vec3f(1.0)), sample.a * u.opacity);
}
