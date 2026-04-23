use crate::texture::*;

pub fn render_billboard_rgb(
    texture: &Texture,
    px_w: usize,
    px_h: usize,
    time_secs: f64,
) -> Vec<(u8, u8, u8)> {
    if px_w == 0 || px_h == 0 {
        return Vec::new();
    }

    let tex_aspect = if texture.height > 0 {
        texture.width as f64 / texture.height as f64
    } else {
        1.0
    };

    let (board_w, board_h) = billboard_size(tex_aspect, px_w, px_h);
    let board_depth = board_w * 0.1;
    let panel_color = texture.edge_color();

    let half_w = board_w / 2.0;
    let half_h = board_h / 2.0;
    let half_d = board_depth / 2.0;

    let spin = time_secs * 0.8;
    let bob = (time_secs * 0.7).sin() * px_h as f64 * 0.03;
    let cos_s = spin.cos();
    let sin_s = spin.sin();

    let cx = px_w as f64 / 2.0;
    let cy = px_h as f64 / 2.0;

    let la = time_secs * 0.3;
    let light = normalize([la.cos() * 0.6, -0.5, la.sin() * 0.4 + 0.6]);

    let mut fb = vec![(0u8, 0u8, 0u8); px_w * px_h];
    let mut hit_mask = vec![false; px_w * px_h];

    background_gradient(&mut fb, px_w, px_h);

    let y_min = ((cy + bob - half_h).floor() as isize).max(0) as usize;
    let y_max = ((cy + bob + half_h).ceil() as usize + 1).min(px_h);

    let mut faces: [(u8, f64); 4] = [
        (0, half_d * cos_s),
        (1, -half_d * cos_s),
        (2, half_w * sin_s),
        (3, -half_w * sin_s),
    ];
    faces.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    for &(face_id, _) in &faces {
        for py in y_min..y_max {
            let world_y = py as f64 - (cy + bob);
            if world_y.abs() > half_h {
                continue;
            }
            let v_tex = (world_y + half_h) / (2.0 * half_h);

            for px_x in 0..px_w {
                let world_x = px_x as f64 - cx;

                let (r, g, b, a) = match face_id {
                    0 => {
                        if cos_s.abs() < 1e-8 {
                            continue;
                        }
                        let lx = (world_x + half_d * sin_s) / cos_s;
                        if lx.abs() > half_w {
                            continue;
                        }
                        let u = (lx + half_w) / (2.0 * half_w);
                        let [tr, tg, tb, ta] = texture.sample(u, v_tex);
                        if ta < 10 {
                            continue;
                        }
                        let n = [-sin_s, 0.0, cos_s];
                        let ndotl = dot(n, light).max(0.0);
                        let bright = 0.35 + ndotl * 0.65;
                        let spec = specular(n, light, 32.0) * 0.2;
                        (
                            shade(tr, bright, spec),
                            shade(tg, bright, spec),
                            shade(tb, bright, spec),
                            ta,
                        )
                    }
                    1 => {
                        if cos_s.abs() < 1e-8 {
                            continue;
                        }
                        let lx = (world_x - half_d * sin_s) / cos_s;
                        if lx.abs() > half_w {
                            continue;
                        }
                        let u = (lx + half_w) / (2.0 * half_w);
                        let [tr, tg, tb, ta] = texture.sample(u, v_tex);
                        if ta < 10 {
                            continue;
                        }
                        let n = [sin_s, 0.0, -cos_s];
                        let ndotl = dot(n, light).max(0.0);
                        let bright = 0.35 + ndotl * 0.65;
                        let spec = specular(n, light, 32.0) * 0.2;
                        (
                            shade(tr, bright, spec),
                            shade(tg, bright, spec),
                            shade(tb, bright, spec),
                            ta,
                        )
                    }
                    2 => {
                        if sin_s.abs() < 1e-8 {
                            continue;
                        }
                        let lz = (half_w * cos_s - world_x) / sin_s;
                        if lz.abs() > half_d {
                            continue;
                        }
                        let [_, _, _, alpha] = texture.sample(0.999, v_tex);
                        if alpha < 10 {
                            continue;
                        }
                        let n = [cos_s, 0.0, sin_s];
                        let ndotl = dot(n, light).max(0.0);
                        let bright = 0.25 + ndotl * 0.45;
                        (
                            (panel_color[0] as f64 * bright * 0.7) as u8,
                            (panel_color[1] as f64 * bright * 0.7) as u8,
                            (panel_color[2] as f64 * bright * 0.7) as u8,
                            alpha,
                        )
                    }
                    3 => {
                        if sin_s.abs() < 1e-8 {
                            continue;
                        }
                        let lz = (-half_w * cos_s - world_x) / sin_s;
                        if lz.abs() > half_d {
                            continue;
                        }
                        let [_, _, _, alpha] = texture.sample(0.001, v_tex);
                        if alpha < 10 {
                            continue;
                        }
                        let n = [-cos_s, 0.0, -sin_s];
                        let ndotl = dot(n, light).max(0.0);
                        let bright = 0.25 + ndotl * 0.45;
                        (
                            (panel_color[0] as f64 * bright * 0.7) as u8,
                            (panel_color[1] as f64 * bright * 0.7) as u8,
                            (panel_color[2] as f64 * bright * 0.7) as u8,
                            alpha,
                        )
                    }
                    _ => continue,
                };

                let idx = py * px_w + px_x;
                hit_mask[idx] = true;
                let alpha = a as f64 / 255.0;
                let bg = fb[idx];
                let inv = 1.0 - alpha;
                fb[idx] = (
                    (r as f64 * alpha + bg.0 as f64 * inv) as u8,
                    (g as f64 * alpha + bg.1 as f64 * inv) as u8,
                    (b as f64 * alpha + bg.2 as f64 * inv) as u8,
                );
            }
        }
    }

    shadow_pass(&mut fb, &hit_mask, px_w, px_h);
    fb
}

fn billboard_size(tex_aspect: f64, px_w: usize, px_h: usize) -> (f64, f64) {
    let vp_aspect = px_w as f64 / px_h as f64;
    let fill = 0.65;
    if tex_aspect > vp_aspect {
        let board_w = px_w as f64 * fill;
        (board_w, board_w / tex_aspect)
    } else {
        let board_h = px_h as f64 * fill;
        (board_h * tex_aspect, board_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_texture() -> Texture<'static> {
        let mut pixels = vec![[0, 0, 0, 255]; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                pixels[y * 16 + x] = if x < 8 {
                    [255, 40, 40, 255]
                } else {
                    [40, 80, 255, 255]
                };
            }
        }
        let leaked = Box::leak(pixels.into_boxed_slice());
        Texture {
            pixels: leaked,
            width: 16,
            height: 16,
        }
    }

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
    fn front_face_preserves_transparent_padding() {
        let texture = padded_texture();
        let px_w = 160;
        let px_h = 120;
        let fb = render_billboard_rgb(&texture, px_w, px_h, 0.0);

        let mut bg = vec![(0u8, 0u8, 0u8); px_w * px_h];
        background_gradient(&mut bg, px_w, px_h);

        let (board_w, _) = billboard_size(1.0, px_w, px_h);
        let cx = px_w / 2;
        let cy = px_h / 2;
        let sample_x = cx - (board_w as usize / 3);
        let idx = cy * px_w + sample_x;

        assert_eq!(
            fb[idx], bg[idx],
            "transparent padding should remain cut out"
        );
    }

    #[test]
    fn back_face_reads_as_billboard_rear() {
        let texture = split_texture();
        let px_w = 180;
        let px_h = 140;
        let front = render_billboard_rgb(&texture, px_w, px_h, 0.0);
        let back_time = std::f64::consts::PI / 0.8;
        let back = render_billboard_rgb(&texture, px_w, px_h, back_time);

        let (board_w, _) = billboard_size(1.0, px_w, px_h);
        let cx = px_w / 2;
        let cy = px_h / 2;
        let left_x = cx - (board_w as usize / 4);
        let right_x = cx + (board_w as usize / 4);
        let left_front = front[cy * px_w + left_x];
        let right_front = front[cy * px_w + right_x];
        let left_back = back[cy * px_w + left_x];
        let right_back = back[cy * px_w + right_x];

        assert!(
            left_front.0 > left_front.2,
            "front face left side should stay warm"
        );
        assert!(
            right_front.2 > right_front.0,
            "front face right side should stay cool"
        );
        assert!(
            left_back.2 > left_back.0,
            "rear face should mirror the front artwork"
        );
        assert!(
            right_back.0 > right_back.2,
            "rear face should mirror the front artwork"
        );
    }
}
