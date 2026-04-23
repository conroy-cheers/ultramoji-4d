pub use emoji_renderer::cpu::*;

use ratatui::text::Line;

use super::common::{Texture, fb_to_lines};

pub fn render_billboard(
    texture: &Texture,
    width: usize,
    height: usize,
    time_secs: f64,
) -> Vec<Line<'static>> {
    let px_w = width;
    let px_h = height * 2;
    let fb = render_billboard_rgb(texture, px_w, px_h, time_secs);
    fb_to_lines(&fb, px_w, px_h, height)
}
