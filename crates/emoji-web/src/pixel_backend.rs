use std::collections::HashMap;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

static FONT_BYTES: &[u8] = include_bytes!("Glass_TTY_VT220.ttf");

const TERM_COLS: u16 = 80;
const TERM_ROWS: u16 = 40;
const TERM_RENDER_SCALE: usize = 2;
const BASE_FONT_SIZE: f32 = 20.0;

struct GlyphCache {
    font: fontdue::Font,
    font_size: f32,
    cache: HashMap<char, RasterizedGlyph>,
}

struct RasterizedGlyph {
    bitmap: Vec<u8>,
    width: usize,
    height: usize,
    x_offset: i32,
    y_offset: i32,
}

impl GlyphCache {
    fn new(font_size: f32) -> Self {
        let font = fontdue::Font::from_bytes(
            FONT_BYTES,
            fontdue::FontSettings::default(),
        )
        .expect("failed to parse embedded font");
        Self {
            font,
            font_size,
            cache: HashMap::new(),
        }
    }

    fn ensure(&mut self, ch: char) {
        self.cache.entry(ch).or_insert_with(|| {
            let (metrics, bitmap) = self.font.rasterize(ch, self.font_size);
            RasterizedGlyph {
                bitmap,
                width: metrics.width,
                height: metrics.height,
                x_offset: metrics.xmin,
                y_offset: metrics.ymin,
            }
        });
    }
}

pub struct PixelBackend {
    cell_w: usize,
    cell_h: usize,
    fb: Vec<[u8; 4]>,
    pw: usize,
    ph: usize,
    cursor_pos: Position,
    glyphs: GlyphCache,
    baseline: i32,
}

impl PixelBackend {
    pub fn new(_pixel_w: usize, _pixel_h: usize) -> Self {
        let font_size = BASE_FONT_SIZE * TERM_RENDER_SCALE as f32;
        let glyphs = GlyphCache::new(font_size);

        let metrics = glyphs.font.metrics('M', font_size);
        let cell_w = metrics.advance_width.ceil() as usize;
        let line_metrics = glyphs.font.horizontal_line_metrics(font_size).unwrap();
        let cell_h = (line_metrics.new_line_size).ceil() as usize;
        let baseline = (-line_metrics.descent).ceil() as i32;

        let pw = TERM_COLS as usize * cell_w;
        let ph = TERM_ROWS as usize * cell_h;
        let fb = vec![[0, 0, 0, 0]; pw * ph];

        Self {
            cell_w,
            cell_h,
            fb,
            pw,
            ph,
            cursor_pos: Position::default(),
            glyphs,
            baseline,
        }
    }

    pub fn resize(&mut self, _pixel_w: usize, _pixel_h: usize) {
        // Fixed terminal size — ignore canvas resize
    }

    pub fn cols(&self) -> u16 {
        TERM_COLS
    }

    pub fn rows(&self) -> u16 {
        TERM_ROWS
    }

    pub fn cell_width(&self) -> usize {
        self.cell_w
    }

    pub fn cell_height(&self) -> usize {
        self.cell_h
    }

    pub fn pixel_width(&self) -> usize {
        self.pw
    }

    pub fn pixel_height(&self) -> usize {
        self.ph
    }

    pub fn framebuffer_rgba(&self) -> &[u8] {
        bytemuck::cast_slice(&self.fb)
    }

    fn draw_glyph(&mut self, ch: char, px_x: usize, px_y: usize, fg: [u8; 4]) {
        self.glyphs.ensure(ch);
        let glyph = &self.glyphs.cache[&ch];
        let gw = glyph.width;
        let gh = glyph.height;
        let origin_y = px_y as i32 + (self.cell_h as i32 - self.baseline) - gh as i32 - glyph.y_offset;
        let origin_x = px_x as i32 + glyph.x_offset;

        for gy in 0..gh {
            let screen_y = origin_y + gy as i32;
            if screen_y < 0 || screen_y >= self.ph as i32 {
                continue;
            }
            let sy = screen_y as usize;
            for gx in 0..gw {
                let screen_x = origin_x + gx as i32;
                if screen_x < 0 || screen_x >= self.pw as i32 {
                    continue;
                }
                let sx = screen_x as usize;
                let alpha = self.glyphs.cache[&ch].bitmap[gy * gw + gx];
                if alpha == 0 {
                    continue;
                }
                let idx = sy * self.pw + sx;
                let dst = &mut self.fb[idx];
                if alpha == 255 {
                    *dst = fg;
                } else {
                    let a = alpha as u16;
                    let inv = 255 - a;
                    dst[0] = ((fg[0] as u16 * a + dst[0] as u16 * inv) / 255) as u8;
                    dst[1] = ((fg[1] as u16 * a + dst[1] as u16 * inv) / 255) as u8;
                    dst[2] = ((fg[2] as u16 * a + dst[2] as u16 * inv) / 255) as u8;
                    dst[3] = 255;
                }
            }
        }
    }
}

fn color_to_green(color: Color) -> [u8; 4] {
    match color {
        Color::Reset => [0, 0, 0, 0],
        Color::Black => [0, 0, 0, 255],
        Color::White => [235, 235, 235, 255],
        Color::Rgb(r, g, b) => {
            if r > 220 && g > 220 && b > 220 {
                return [235, 235, 235, 255];
            }
            let lum = (r as u16 * 77 + g as u16 * 150 + b as u16 * 29) >> 8;
            [0, lum as u8, 0, if lum > 0 { 255 } else { 0 }]
        }
        _ => color_to_green(remap_to_rgb(color)),
    }
}

fn color_to_green_fg(color: Color) -> [u8; 4] {
    match color {
        Color::Reset => GREEN_FG,
        Color::Black => [0, 0, 0, 255],
        Color::White => [235, 235, 235, 255],
        Color::Rgb(r, g, b) => {
            if r > 220 && g > 220 && b > 220 {
                return [235, 235, 235, 255];
            }
            let lum = (r as u16 * 77 + g as u16 * 150 + b as u16 * 29) >> 8;
            let g_val = 80 + ((lum as u16 * 175) >> 8) as u8;
            [0, g_val, 0, 255]
        }
        _ => color_to_green_fg(remap_to_rgb(color)),
    }
}

fn remap_to_rgb(color: Color) -> Color {
    match color {
        Color::Red | Color::LightRed => Color::Rgb(200, 60, 60),
        Color::Green | Color::LightGreen => Color::Rgb(60, 255, 60),
        Color::Yellow | Color::LightYellow => Color::Rgb(200, 200, 60),
        Color::Blue | Color::LightBlue => Color::Rgb(60, 60, 200),
        Color::Magenta | Color::LightMagenta => Color::Rgb(200, 60, 200),
        Color::Cyan | Color::LightCyan => Color::Rgb(60, 200, 200),
        Color::White => Color::Rgb(200, 255, 200),
        Color::Gray => Color::Rgb(128, 128, 128),
        Color::DarkGray => Color::Rgb(80, 80, 80),
        Color::Indexed(idx) => {
            let v = match idx {
                0..=7 => 60 + idx as u8 * 20,
                8..=15 => 120 + (idx - 8) as u8 * 15,
                _ => 128,
            };
            Color::Rgb(v, v, v)
        }
        other => other,
    }
}

const GREEN_FG: [u8; 4] = [0, 204, 0, 255];
const BLACK_BG: [u8; 4] = [0, 0, 0, 0];

impl Backend for PixelBackend {
    type Error = core::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (col, row, cell) in content {
            let px_x = col as usize * self.cell_w;
            let px_y = row as usize * self.cell_h;

            let bg = color_to_green(cell.bg);

            if bg[3] > 0 {
                let max_x = (px_x + self.cell_w).min(self.pw);
                let max_y = (px_y + self.cell_h).min(self.ph);
                for py in px_y..max_y {
                    for px in px_x..max_x {
                        self.fb[py * self.pw + px] = bg;
                    }
                }
            }

            let symbol = cell.symbol();
            if symbol == " " || symbol.is_empty() {
                continue;
            }

            let fg = color_to_green_fg(cell.fg);
            for ch in symbol.chars() {
                self.draw_glyph(ch, px_x, px_y, fg);
            }
        }

        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor_pos)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.cursor_pos = position.into();
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.fb.fill(BLACK_BG);
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        match clear_type {
            ClearType::All => self.clear(),
            _ => Ok(()),
        }
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(Size::new(TERM_COLS, TERM_ROWS))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: Size::new(TERM_COLS, TERM_ROWS),
            pixels: Size::new(self.pw as u16, self.ph as u16),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
