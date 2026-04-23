use std::collections::VecDeque;

pub const ALPHA_SHAPE_THRESHOLD: u8 = 24;
pub const COLOR_SOURCE_ALPHA_THRESHOLD: u8 = 230;

pub struct Texture<'a> {
    pub pixels: &'a [[u8; 4]],
    pub width: u32,
    pub height: u32,
}

impl Texture<'_> {
    pub fn sample(&self, u: f64, v: f64) -> [u8; 4] {
        if self.width == 0 || self.height == 0 {
            return [0, 0, 0, 0];
        }
        let x = ((u.clamp(0.0, 0.9999) * self.width as f64) as u32).min(self.width - 1);
        let y = ((v.clamp(0.0, 0.9999) * self.height as f64) as u32).min(self.height - 1);
        let idx = (y * self.width + x) as usize;
        self.pixels.get(idx).copied().unwrap_or([0, 0, 0, 0])
    }

    pub fn edge_color(&self) -> [u8; 3] {
        let (mut r, mut g, mut b, mut count) = (0u64, 0u64, 0u64, 0u64);
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return [80, 80, 80];
        }
        for x in 0..w {
            for &y in &[0, h - 1] {
                let idx = (y * w + x) as usize;
                if let Some(&[pr, pg, pb, pa]) = self.pixels.get(idx) {
                    if pa >= COLOR_SOURCE_ALPHA_THRESHOLD {
                        r += pr as u64;
                        g += pg as u64;
                        b += pb as u64;
                        count += 1;
                    }
                }
            }
        }
        for y in 0..h {
            for &x in &[0, w - 1] {
                let idx = (y * w + x) as usize;
                if let Some(&[pr, pg, pb, pa]) = self.pixels.get(idx) {
                    if pa >= COLOR_SOURCE_ALPHA_THRESHOLD {
                        r += pr as u64;
                        g += pg as u64;
                        b += pb as u64;
                        count += 1;
                    }
                }
            }
        }
        if count == 0 {
            for &[pr, pg, pb, pa] in self.pixels {
                if pa >= COLOR_SOURCE_ALPHA_THRESHOLD {
                    r += pr as u64;
                    g += pg as u64;
                    b += pb as u64;
                    count += 1;
                }
            }
        }
        if count == 0 {
            for &[pr, pg, pb, pa] in self.pixels {
                if pa >= ALPHA_SHAPE_THRESHOLD {
                    r += pr as u64;
                    g += pg as u64;
                    b += pb as u64;
                    count += 1;
                }
            }
        }
        if count == 0 {
            return [80, 80, 80];
        }
        [(r / count) as u8, (g / count) as u8, (b / count) as u8]
    }
}

pub fn background_gradient(fb: &mut [(u8, u8, u8)], px_w: usize, px_h: usize) {
    let cx = px_w as f64 / 2.0;
    let cy = px_h as f64 / 2.0;
    let max_dist = (cx * cx + cy * cy).sqrt();
    for py in 0..px_h {
        for px_x in 0..px_w {
            let dx = px_x as f64 - cx;
            let dy = py as f64 - cy;
            let t = ((dx * dx + dy * dy).sqrt() / max_dist).min(1.0);
            let v = (20.0 * (1.0 - t * 0.4)) as u8;
            fb[py * px_w + px_x] = (v / 3, v / 3, v);
        }
    }
}

pub fn shadow_pass(fb: &mut [(u8, u8, u8)], hit_mask: &[bool], px_w: usize, px_h: usize) {
    let sdx = 4isize;
    let sdy = 6isize;
    for py in 0..px_h {
        for px_x in 0..px_w {
            if hit_mask[py * px_w + px_x] {
                continue;
            }
            let sx = px_x as isize - sdx;
            let sy = py as isize - sdy;
            if sx >= 0
                && (sx as usize) < px_w
                && sy >= 0
                && (sy as usize) < px_h
                && hit_mask[sy as usize * px_w + sx as usize]
            {
                let c = &mut fb[py * px_w + px_x];
                c.0 = (c.0 as f64 * 0.4) as u8;
                c.1 = (c.1 as f64 * 0.4) as u8;
                c.2 = (c.2 as f64 * 0.4) as u8;
            }
        }
    }
}

pub fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        return [0.0, 0.0, 1.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn specular(normal: [f64; 3], light: [f64; 3], power: f64) -> f64 {
    let ndotl = dot(normal, light);
    let reflect = [
        2.0 * ndotl * normal[0] - light[0],
        2.0 * ndotl * normal[1] - light[1],
        2.0 * ndotl * normal[2] - light[2],
    ];
    let view_dot = -reflect[2];
    view_dot.max(0.0).powf(power)
}

pub fn shade(channel: u8, brightness: f64, spec: f64) -> u8 {
    ((channel as f64 * brightness + spec * 255.0).min(255.0)) as u8
}

pub fn fill_transparent_rgb_from_nearest(
    pixels: &mut [[u8; 4]],
    width: u32,
    height: u32,
    alpha_threshold: u8,
) {
    if width == 0 || height == 0 || pixels.is_empty() {
        return;
    }

    let w = width as usize;
    let h = height as usize;
    let original = pixels.to_vec();
    let mut nearest = vec![usize::MAX; pixels.len()];
    let mut queue = VecDeque::new();

    for (idx, pixel) in original.iter().enumerate() {
        if pixel[3] >= alpha_threshold {
            nearest[idx] = idx;
            queue.push_back(idx);
        }
    }

    if queue.is_empty() && alpha_threshold > ALPHA_SHAPE_THRESHOLD {
        fill_transparent_rgb_from_nearest(pixels, width, height, ALPHA_SHAPE_THRESHOLD);
        return;
    }

    if queue.is_empty() {
        return;
    }

    while let Some(idx) = queue.pop_front() {
        let source = nearest[idx];
        let x = idx % w;
        let y = idx / w;

        if x > 0 {
            let next = idx - 1;
            if nearest[next] == usize::MAX {
                nearest[next] = source;
                queue.push_back(next);
            }
        }
        if x + 1 < w {
            let next = idx + 1;
            if nearest[next] == usize::MAX {
                nearest[next] = source;
                queue.push_back(next);
            }
        }
        if y > 0 {
            let next = idx - w;
            if nearest[next] == usize::MAX {
                nearest[next] = source;
                queue.push_back(next);
            }
        }
        if y + 1 < h {
            let next = idx + w;
            if nearest[next] == usize::MAX {
                nearest[next] = source;
                queue.push_back(next);
            }
        }
    }

    for (idx, pixel) in pixels.iter_mut().enumerate() {
        if pixel[3] >= alpha_threshold {
            continue;
        }
        let source = nearest[idx];
        if source == usize::MAX {
            continue;
        }
        pixel[0] = original[source][0];
        pixel[1] = original[source][1];
        pixel[2] = original[source][2];
    }
}
