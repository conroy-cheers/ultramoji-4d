pub use emoji_renderer::texture::*;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

pub fn fb_to_lines(
    fb: &[(u8, u8, u8)],
    px_w: usize,
    px_h: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    for row in 0..height {
        let top_y = row * 2;
        let bot_y = row * 2 + 1;
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(px_w);

        for col in 0..px_w {
            let top = fb[top_y * px_w + col];
            let bot = if bot_y < px_h {
                fb[bot_y * px_w + col]
            } else {
                (0, 0, 0)
            };

            spans.push(Span::styled(
                "▀",
                Style::default()
                    .fg(Color::Rgb(top.0, top.1, top.2))
                    .bg(Color::Rgb(bot.0, bot.1, bot.2)),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}
