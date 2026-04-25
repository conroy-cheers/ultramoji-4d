struct Uniforms {
    mvp: mat4x4f,
    normal_rot: mat4x4f,
    shadow_mvp: mat4x4f,
    ground_mvp: mat4x4f,
    light_mvp: mat4x4f,
    light_dir: vec4f,
    bg_color: vec4f,
    camera_pos: vec4f,
    shadow_map_params: vec4f,
    precomputed_shadow_params: vec4f,
    ambient_light_params: vec4f,
    contact_shadow_params: vec4f,
    contact_shadow_march: vec4f,
    contact_shadow_screen: vec4f,
    contact_shadow_bounds: vec4f,
    ground_y: f32,
    debug_flags: u32,
    near: f32,
    far: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(1) @binding(2) var<uniform> edge_color: vec4f;
@group(2) @binding(0) var shadow_map_tex: texture_depth_2d;
@group(2) @binding(1) var shadow_map_sampler: sampler;
@group(3) @binding(0) var precomputed_shadow_tex: texture_2d_array<f32>;
@group(3) @binding(1) var precomputed_shadow_sampler: sampler;
@group(3) @binding(2) var contact_depth_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) normal: vec3f,
    @location(1) uv: vec2f,
    @location(2) @interpolate(flat) face_type: u32,
    @location(3) light_position: vec4f,
}

struct VertexInput {
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @location(2) uv: vec2f,
    @location(3) face_type: u32,
}

struct SceneOutput {
    @location(0) color: vec4f,
    @location(1) depth: f32,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let world_pos = u.mvp * vec4f(in.position, 1.0);
    let rotated_normal = (u.normal_rot * vec4f(in.normal, 0.0)).xyz;

    var out: VertexOutput;
    out.position = world_pos;
    out.normal = rotated_normal;
    out.uv = in.uv;
    out.face_type = in.face_type;
    out.light_position = u.light_mvp * vec4f(in.position, 1.0);
    return out;
}

fn luminance(c: vec3f) -> f32 {
    return dot(c, vec3f(0.299, 0.587, 0.114));
}

fn ambient_light_color(bg_color: vec3f, ambient_light_params: vec4f) -> vec3f {
    let tint = mix(vec3f(1.0), bg_color, ambient_light_params.y);
    let tint_luma = max(luminance(tint), 0.001);
    return tint * (ambient_light_params.x / tint_luma);
}

fn perturb_normal(geom_n: vec3f, uv: vec2f, face_type: u32) -> vec3f {
    let tex_size = vec2f(textureDimensions(tex));
    let texel = 1.0 / tex_size;

    let h_l = luminance(textureSample(tex, tex_sampler, uv + vec2f(-texel.x, 0.0)).rgb);
    let h_r = luminance(textureSample(tex, tex_sampler, uv + vec2f( texel.x, 0.0)).rgb);
    let h_d = luminance(textureSample(tex, tex_sampler, uv + vec2f(0.0, -texel.y)).rgb);
    let h_u = luminance(textureSample(tex, tex_sampler, uv + vec2f(0.0,  texel.y)).rgb);

    let du = (h_l - h_r) * 0.5;
    let dv = (h_d - h_u) * 0.5;
    let strength = 3.0;

    // Tangent-space perturbation: front face tangent = +X, bitangent = -Y
    // Back face mirrors U, so flip the tangent
    var tu = du * strength;
    if face_type == 1u {
        tu = -tu;
    }
    let tv = -dv * strength;

    let up = select(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), abs(geom_n.y) < 0.99);
    let tangent = normalize(cross(up, geom_n));
    let bitangent = cross(geom_n, tangent);

    return normalize(geom_n + tangent * tu + bitangent * tv);
}

// WEBGPU_SHADOW_MAP_AMOUNT_START
fn shadow_map_amount(light_position: vec4f) -> f32 {
    if light_position.w <= 0.0 {
        return 0.0;
    }
    let ndc = light_position.xyz / light_position.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 0.0;
    }
    let shadow_uv = vec2f(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let shadow_dims = vec2i(textureDimensions(shadow_map_tex));
    let shadow_px = clamp(vec2i(shadow_uv * vec2f(shadow_dims)), vec2i(0), shadow_dims - vec2i(1));
    var hits = 0.0;
    var samples = 0.0;
    for (var y: i32 = -1; y <= 1; y++) {
        for (var x: i32 = -1; x <= 1; x++) {
            let px = clamp(shadow_px + vec2i(x, y), vec2i(0), shadow_dims - vec2i(1));
            let shadow_depth = textureLoad(shadow_map_tex, px, 0);
            if ndc.z - u.shadow_map_params.w > shadow_depth {
                hits += 1.0;
            }
            samples += 1.0;
        }
    }
    return hits / samples;
}
// WEBGPU_SHADOW_MAP_AMOUNT_END

fn shadow_map_factor(light_position: vec4f) -> f32 {
    if u.shadow_map_params.x < 0.5 || u.shadow_map_params.x >= 1.5 {
        return 1.0;
    }
    let shadow = shadow_map_amount(light_position);
    let shadow_amount = clamp(u.shadow_map_params.y * u.shadow_map_params.z, 0.0, 1.0);
    return 1.0 - shadow * shadow_amount;
}

fn local_light_dir() -> vec3f {
    let world_light = normalize(u.light_dir.xyz);
    return vec3f(
        dot(world_light, u.normal_rot[0].xyz),
        dot(world_light, u.normal_rot[1].xyz),
        dot(world_light, u.normal_rot[2].xyz),
    );
}

fn precomputed_shadow_factor(uv: vec2f, face_type: u32) -> f32 {
    if u.precomputed_shadow_params.x < 0.5 {
        return 1.0;
    }
    if face_type > 1u {
        return 1.0;
    }
    if face_type == 0u && u.precomputed_shadow_params.z < 0.5 {
        return 1.0;
    }
    if face_type == 1u && u.precomputed_shadow_params.w < 0.5 {
        return 1.0;
    }
    let layer_count = i32(textureNumLayers(precomputed_shadow_tex));
    let layer = clamp(i32(u.precomputed_shadow_params.y) + i32(face_type), 0, layer_count - 1);
    let dims = vec2i(textureDimensions(precomputed_shadow_tex));
    let px = clamp(vec2i(uv * vec2f(dims)), vec2i(0), dims - vec2i(1));
    let shadow = textureLoad(precomputed_shadow_tex, px, layer, 0).r;
    let shadow_amount = clamp(u.shadow_map_params.y * u.shadow_map_params.z, 0.0, 1.0);
    return 1.0 - shadow * shadow_amount;
}

fn camera_linear_depth(d: f32) -> f32 {
    return u.near * u.far / (u.far - d * (u.far - u.near));
}

fn contact_shadow_factor(frag_coord: vec4f, direct_facing: f32) -> f32 {
    let contact_jitter = max(u.contact_shadow_screen.w - 1.0, 0.0);
    if u.contact_shadow_screen.w < 0.5 || direct_facing <= 0.0 {
        return 1.0;
    }

    let base_dir = u.contact_shadow_screen.xy;
    if dot(base_dir, base_dir) < 0.001 {
        return 1.0;
    }

    let dims_i = vec2i(textureDimensions(contact_depth_tex));
    let dims = vec2f(dims_i);
    let bounds = u.contact_shadow_bounds;
    let strength = clamp(u.contact_shadow_params.x * u.contact_shadow_params.y, 0.0, 1.0);
    if strength <= 0.0 {
        return 1.0;
    }

    let raw_depth = frag_coord.z;
    let dz_ndc_per_px = u.contact_shadow_screen.z;
    let depth_threshold = u.contact_shadow_params.z;
    let max_depth_delta = u.contact_shadow_params.w;
    let step_count = u32(clamp(u.contact_shadow_march.w, 1.0, 128.0));
    let step_dist = max(u.contact_shadow_march.y, 0.25);
    let max_trace_dist = max(u.contact_shadow_march.z, u.contact_shadow_march.x + step_dist);

    let ign = fract(52.9829189 * fract(0.06711056 * frag_coord.x + 0.00583715 * frag_coord.y));
    let perp_dir = vec2f(-base_dir.y, base_dir.x);
    let march_dir = normalize(base_dir + perp_dir * ((ign - 0.5) * contact_jitter));

    var dist = u.contact_shadow_march.x;
    for (var i = 0u; i < step_count; i++) {
        if dist > max_trace_dist {
            break;
        }
        let sample_pos = frag_coord.xy + march_dir * dist;
        if sample_pos.x < 0.0 || sample_pos.y < 0.0
            || sample_pos.x >= dims.x || sample_pos.y >= dims.y {
            break;
        }
        if sample_pos.x < bounds.x || sample_pos.y < bounds.y
            || sample_pos.x > bounds.z || sample_pos.y > bounds.w {
            dist += step_dist;
            continue;
        }

        let sample_px = clamp(vec2i(sample_pos), vec2i(0), dims_i - vec2i(1));
        let scene_ndc = textureLoad(contact_depth_tex, sample_px, 0).r;
        if scene_ndc < 0.999 {
            let ray_ndc = clamp(raw_depth + dz_ndc_per_px * dist, 0.0, 0.999);
            let ray_lin = camera_linear_depth(ray_ndc);
            let scene_lin = camera_linear_depth(scene_ndc);
            let diff = ray_lin - scene_lin;
            if diff > depth_threshold && diff < max_depth_delta {
                let distance_fade = 1.0 - smoothstep(max_trace_dist * 0.45, max_trace_dist, dist);
                let thickness_fade = 1.0 - smoothstep(max_depth_delta * 0.45, max_depth_delta, diff);
                return 1.0 - strength * distance_fade * thickness_fade;
            }
        }

        dist += step_dist;
    }

    return 1.0;
}

fn texture_space_alpha_shadow_dir(uv: vec2f, uv_dir: vec2f) -> f32 {
    let tex_dims = vec2i(textureDimensions(tex));
    if dot(uv_dir, uv_dir) < 0.0001 {
        return 0.0;
    }
    let tex_extent = vec2f(f32(max(tex_dims.x, 1)), f32(max(tex_dims.y, 1)));
    let step_uv = normalize(uv_dir) / tex_extent;
    var saw_gap = false;
    for (var i: i32 = 2; i <= 96; i++) {
        let sample_uv = uv + step_uv * f32(i);
        if sample_uv.x < 0.0 || sample_uv.y < 0.0 || sample_uv.x > 1.0 || sample_uv.y > 1.0 {
            break;
        }
        let px = clamp(vec2i(sample_uv * vec2f(tex_dims)), vec2i(0), tex_dims - vec2i(1));
        let alpha = textureLoad(tex, px, 0).a;
        if alpha < 0.1 {
            saw_gap = true;
        } else if saw_gap {
            return 1.0;
        }
    }
    return 0.0;
}

fn texture_space_alpha_shadow(uv: vec2f) -> f32 {
    let local_light = local_light_dir();
    let uv_dir = vec2f(local_light.x, -local_light.y);
    // The baked atlas is indexed by texture space rather than screen space.
    // Try both projected-light signs so the mask is robust to front/back UV
    // orientation and rotation-bin rounding; this produces a deliberately
    // strong candidate mask that runtime controls can attenuate.
    return max(texture_space_alpha_shadow_dir(uv, uv_dir), texture_space_alpha_shadow_dir(uv, -uv_dir));
}

@fragment
fn fs_main(in: VertexOutput) -> SceneOutput {
    let light = normalize(u.light_dir.xyz);
    let geom_n = normalize(in.normal);

    var base_color: vec4f;
    var diff_strength: f32;

    let sample = textureSample(tex, tex_sampler, in.uv);
    base_color = vec4f(sample.rgb, 1.0);

    if in.face_type <= 1u {
        diff_strength = 0.65;
    } else {
        diff_strength = 0.45;
    }

    if (u.debug_flags & 1u) != 0u {
        base_color = vec4f(vec3f(1.0), base_color.a);
    }

    let n = normalize(perturb_normal(geom_n, in.uv, in.face_type));

    let direct_facing = select(0.0, 1.0, dot(geom_n, light) > 0.0);
    let ndotl = max(dot(n, light), 0.0) * direct_facing;
    let diffuse = ndotl * diff_strength;

    let reflect = 2.0 * dot(n, light) * n - light;
    let view = normalize(u.camera_pos.xyz);
    let lum = luminance(base_color.rgb);
    let spec = pow(max(dot(reflect, view), 0.0), 32.0) * 0.2 * smoothstep(0.3, 0.8, lum) * direct_facing;

    var dark_factor = 1.0;
    if in.face_type >= 2u {
        dark_factor = 0.7;
    }

    let direct_visibility = shadow_map_factor(in.light_position)
        * contact_shadow_factor(in.position, direct_facing)
        * precomputed_shadow_factor(in.uv, in.face_type);
    let ambient_rgb = ambient_light_color(u.bg_color.rgb, u.ambient_light_params);
    let ambient_contribution = base_color.rgb * ambient_rgb;
    let direct_contribution = base_color.rgb * dark_factor * vec3f(diffuse * direct_visibility)
        + vec3f(spec * direct_visibility);
    let rgb = ambient_contribution + direct_contribution;
    var out: SceneOutput;
    out.color = vec4f(clamp(rgb, vec3f(0.0), vec3f(1.0)), base_color.a);
    out.depth = in.position.z;
    return out;
}

struct ShadowVertexOutput {
    @builtin(position) position: vec4f,
}

@vertex
fn vs_contact_depth(in: VertexInput) -> ShadowVertexOutput {
    var out: ShadowVertexOutput;
    out.position = u.mvp * vec4f(in.position, 1.0);
    return out;
}

@fragment
fn fs_contact_depth(in: ShadowVertexOutput) -> @location(0) f32 {
    return in.position.z;
}

@vertex
fn vs_shadow(in: VertexInput) -> ShadowVertexOutput {
    var out: ShadowVertexOutput;
    out.position = u.shadow_mvp * vec4f(in.position, 1.0);
    return out;
}

@vertex
fn vs_shadow_map(in: VertexInput) -> ShadowVertexOutput {
    var out: ShadowVertexOutput;
    out.position = u.light_mvp * vec4f(in.position, 1.0);
    return out;
}

struct PrecomputedShadowVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) light_position: vec4f,
    @location(2) @interpolate(flat) face_type: u32,
}

@vertex
fn vs_precomputed_shadow(in: VertexInput) -> PrecomputedShadowVertexOutput {
    var out: PrecomputedShadowVertexOutput;
    out.position = vec4f(in.uv.x * 2.0 - 1.0, (1.0 - in.uv.y) * 2.0 - 1.0, 0.0, 1.0);
    out.uv = in.uv;
    out.light_position = u.light_mvp * vec4f(in.position, 1.0);
    out.face_type = in.face_type;
    return out;
}

@fragment
fn fs_precomputed_shadow(in: PrecomputedShadowVertexOutput) -> @location(0) vec4f {
    let sample = textureSample(tex, tex_sampler, in.uv);
    if in.face_type != u32(round(u.precomputed_shadow_params.w)) {
        discard;
    }
    if sample.a < 0.01 {
        discard;
    }
    let shadow = max(shadow_map_amount(in.light_position), texture_space_alpha_shadow(in.uv));
    return vec4f(vec3f(shadow), 1.0);
}

@fragment
fn fs_shadow(@builtin(position) pos: vec4f) -> SceneOutput {
    var out: SceneOutput;
    out.color = vec4f(0.0, 0.0, 0.0, 0.45);
    out.depth = 1.0;
    return out;
}

@fragment
fn fs_shadow_color(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    return vec4f(0.0, 0.0, 0.0, 0.45);
}

struct GroundVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) world_xz: vec2f,
}

@vertex
fn vs_ground(@builtin(vertex_index) index: u32) -> GroundVertexOutput {
    let extent = 500.0;
    var corners = array<vec2f, 4>(
        vec2f(-extent, -extent),
        vec2f( extent, -extent),
        vec2f( extent,  extent),
        vec2f(-extent,  extent),
    );
    var indices = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let vi = indices[index];
    let xz = corners[vi];
    let world = vec4f(xz.x, u.ground_y, xz.y, 1.0);

    var out: GroundVertexOutput;
    out.position = u.ground_mvp * world;
    out.world_xz = xz;
    return out;
}

@fragment
fn fs_ground(in: GroundVertexOutput) -> SceneOutput {
    let dist = length(in.world_xz);
    let t = clamp(dist / 4.0, 0.0, 1.0);
    let center = u.bg_color.rgb * 2.3;
    let edge = u.bg_color.rgb;
    var out: SceneOutput;
    out.color = vec4f(mix(center, edge, t), 1.0);
    out.depth = 1.0;
    return out;
}

struct SsaoParams {
    strength: f32,
    depth_threshold: f32,
    start_dist: f32,
    step_growth: f32,
    max_shadow: f32,
    jitter_spread: f32,
    max_depth_delta: f32,
    bbox_padding: f32,
    steps: u32,
    empty_depth_mode: u32,
    shadow_mode: u32,
    _pad0: u32,
    object_bbox_min: vec2f,
    object_bbox_max: vec2f,
    screen_dir: vec2f,
    dz_ndc_per_px: f32,
    _pad1: f32,
}

fn shadow_map_ambient_color() -> vec3f {
    return ambient_light_color(ssao_u.bg_color.rgb, ssao_u.ambient_light_params);
}

@group(0) @binding(0) var<uniform> ssao_u: Uniforms;
@group(0) @binding(1) var ssao_color: texture_2d<f32>;
@group(0) @binding(2) var ssao_linear_depth: texture_2d<f32>;
@group(0) @binding(3) var<uniform> ssao_params: SsaoParams;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    var positions = array<vec2f, 3>(
        vec2f(-1.0, -3.0),
        vec2f(-1.0, 1.0),
        vec2f(3.0, 1.0),
    );
    return vec4f(positions[index], 0.0, 1.0);
}

fn linearize_depth(d: f32) -> f32 {
    // Reverse the perspective depth: z_clip = (far * z_eye + near * far) / (-z_eye * (far - near))
    // Solving for z_eye (positive distance from camera):
    return ssao_u.near * ssao_u.far / (ssao_u.far - d * (ssao_u.far - ssao_u.near));
}

@fragment
fn fs_ssao(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let px = vec2i(frag_coord.xy);
    let color = textureLoad(ssao_color, px, 0);
    let raw_depth = textureLoad(ssao_linear_depth, px, 0).r;

    if (ssao_u.debug_flags & 2u) != 0u {
        if raw_depth >= 0.999 {
            return vec4f(0.0, 0.0, 0.0, 1.0);
        }
        let lin = linearize_depth(raw_depth);
        let v = clamp(1.0 - lin / ssao_u.far, 0.0, 1.0);
        return vec4f(v, v, v, 1.0);
    }

    if raw_depth >= 0.999 {
        return color;
    }
    if ssao_params.shadow_mode == 1u {
        return color;
    }

    let base_dir = ssao_params.screen_dir;
    if dot(base_dir, base_dir) < 0.001 {
        return color;
    }
    let dz_ndc_per_px = ssao_params.dz_ndc_per_px;

    // Per-pixel jitter via interleaved gradient noise (Jimenez 2014). Keep this
    // trig-free; this shader runs over the whole receiver surface every frame.
    let ign = fract(52.9829189 * fract(0.06711056 * frag_coord.x + 0.00583715 * frag_coord.y));
    let perp_dir = vec2f(-base_dir.y, base_dir.x);
    let march_dir = normalize(base_dir + perp_dir * ((ign - 0.5) * ssao_params.jitter_spread));

    var shadow_hit = false;
    var dist = ssao_params.start_dist;
    let growth = ssao_params.step_growth;
    let bbox_min = ssao_params.object_bbox_min;
    let bbox_max = ssao_params.object_bbox_max;
    let dims = vec2f(textureDimensions(ssao_linear_depth));
    for (var i = 0u; i < ssao_params.steps; i++) {
        let offset = march_dir * dist;
        let sample_pos = frag_coord.xy + offset;
        if sample_pos.x < 0.0 || sample_pos.y < 0.0
            || sample_pos.x >= dims.x || sample_pos.y >= dims.y {
            break;
        }
        if sample_pos.x < bbox_min.x || sample_pos.y < bbox_min.y
            || sample_pos.x > bbox_max.x || sample_pos.y > bbox_max.y {
            dist *= growth;
            continue;
        }
        let sample_px = vec2i(sample_pos);
        let scene_ndc = textureLoad(ssao_linear_depth, sample_px, 0).r;
        if scene_ndc >= 0.999 {
            if ssao_params.empty_depth_mode == 1u {
                break;
            }
            if ssao_params.empty_depth_mode == 2u {
                shadow_hit = true;
                break;
            }
            dist *= growth;
            continue;
        }
        let ray_ndc = clamp(raw_depth + dz_ndc_per_px * dist, 0.0, 0.999);
        let ray_lin = linearize_depth(ray_ndc);
        let scene_lin = linearize_depth(scene_ndc);

        let diff = ray_lin - scene_lin;
        if diff > ssao_params.depth_threshold && diff < ssao_params.max_depth_delta {
            shadow_hit = true;
            break;
        }

        dist *= growth;
    }

    let shadow = select(0.0, 1.0, shadow_hit);
    let shadow_amount = clamp(ssao_params.strength * ssao_params.max_shadow, 0.0, 1.0);
    let shadowed_rgb = color.rgb * shadow_map_ambient_color();
    return vec4f(mix(color.rgb, shadowed_rgb, shadow * shadow_amount), color.a);
}

struct PostprocessUniforms {
    contrast: f32,
    sharpen: f32,
    dither: f32,
    frame: f32,
    vhs: f32,
    _pp_pad0: f32,
    _pp_pad1: f32,
    _pp_pad2: f32,
}

@group(0) @binding(0) var<uniform> pp: PostprocessUniforms;
@group(0) @binding(1) var pp_input: texture_2d<f32>;

fn hash_noise(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

fn tri_dither(coord: vec2f, frame: f32) -> vec3f {
    let n0 = hash_noise(coord + vec2f(frame * 1.37, frame * 0.71));
    let n1 = hash_noise(coord + vec2f(frame * 2.13, frame * 1.93) + vec2f(0.5));
    return vec3f(n0 + n1 - 1.0);
}

fn apply_contrast(c: vec3f) -> vec3f {
    return clamp((c - vec3f(0.5)) * pp.contrast + vec3f(0.5), vec3f(0.0), vec3f(1.0));
}

fn load_pp(coord: vec2i, dims: vec2i) -> vec3f {
    return apply_contrast(textureLoad(pp_input, clamp(coord, vec2i(0), dims - 1), 0).rgb);
}

@fragment
fn fs_postprocess(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let px = vec2i(frag_coord.xy);
    let dims = vec2i(textureDimensions(pp_input));
    var color = load_pp(px, dims);

    if pp.sharpen > 0.0 {
        let l = load_pp(px + vec2i(-1, 0), dims);
        let r = load_pp(px + vec2i( 1, 0), dims);
        let t = load_pp(px + vec2i( 0,-1), dims);
        let b = load_pp(px + vec2i( 0, 1), dims);
        let avg = (l + r + t + b) * 0.25;
        color = clamp(color + (color - avg) * pp.sharpen, vec3f(0.0), vec3f(1.0));
    }

    if pp.vhs > 0.0 {
        let row = f32(px.y);
        let time = pp.frame;

        let row_noise = hash_noise(vec2f(row * 0.37, time * 0.13));
        let blur_w = pp.vhs * (1.0 + row_noise * 2.0);
        let kernel = i32(ceil(blur_w));
        var blurred = vec3f(0.0);
        var total_w = 0.0;
        for (var dx = -kernel; dx <= kernel; dx++) {
            let w = max(0.0, 1.0 - abs(f32(dx)) / blur_w);
            blurred += load_pp(px + vec2i(dx, 0), dims) * w;
            total_w += w;
        }
        blurred /= total_w;

        let fringe = i32(ceil(pp.vhs * 1.8));
        let cr = load_pp(px + vec2i(fringe, 0), dims).r;
        let cb = load_pp(px + vec2i(-fringe, 0), dims).b;
        blurred = vec3f(
            mix(blurred.r, cr, pp.vhs * 0.55),
            blurred.g,
            mix(blurred.b, cb, pp.vhs * 0.55),
        );

        let odd = f32(px.y % 2);
        let scanline_gap = 1.0 - pp.vhs * 0.20 * odd;
        let scanline_wave = 1.0 - pp.vhs * 0.02 * (1.0 + sin(row * 0.15 + time * 0.3));

        color = clamp(blurred * scanline_gap * scanline_wave, vec3f(0.0), vec3f(1.0));
    }

    if pp.dither > 0.0 {
        let noise = tri_dither(frag_coord.xy, pp.frame) * pp.dither / 32.0;
        color = clamp(color + noise, vec3f(0.0), vec3f(1.0));
    }

    return vec4f(color, 1.0);
}

@group(0) @binding(0) var downsample_input: texture_2d<f32>;

@fragment
fn fs_downsample(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let dst = vec2i(frag_coord.xy);
    let src = dst * 2;
    let dims = vec2i(textureDimensions(downsample_input));
    let a = textureLoad(downsample_input, clamp(src, vec2i(0), dims - 1), 0).rgb;
    let b = textureLoad(downsample_input, clamp(src + vec2i(1, 0), vec2i(0), dims - 1), 0).rgb;
    let c = textureLoad(downsample_input, clamp(src + vec2i(0, 1), vec2i(0), dims - 1), 0).rgb;
    let d = textureLoad(downsample_input, clamp(src + vec2i(1, 1), vec2i(0), dims - 1), 0).rgb;
    return vec4f((a + b + c + d) * 0.25, 1.0);
}
