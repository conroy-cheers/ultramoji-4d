pub fn glyph_pattern(ch: char) -> [u8; 5] {
    match ch.to_ascii_uppercase() {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'A' => [0b111, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b110, 0b100, 0b110, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b011],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '_' => [0b000, 0b000, 0b000, 0b000, 0b111],
        '(' => [0b010, 0b100, 0b100, 0b100, 0b010],
        ')' => [0b010, 0b001, 0b001, 0b001, 0b010],
        '[' => [0b110, 0b100, 0b100, 0b100, 0b110],
        ']' => [0b011, 0b001, 0b001, 0b001, 0b011],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '#' => [0b010, 0b111, 0b010, 0b111, 0b010],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        '?' => [0b111, 0b001, 0b010, 0b000, 0b010],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        '*' => [0b101, 0b010, 0b111, 0b010, 0b101],
        '\'' => [0b010, 0b010, 0b000, 0b000, 0b000],
        '"' => [0b101, 0b101, 0b000, 0b000, 0b000],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b101, 0b010, 0b000, 0b010],
    }
}

pub fn draw_text_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: [u8; 4],
) {
    let mut cursor_x = x;
    for ch in text.chars() {
        draw_glyph_rgba(fb, width, height, cursor_x, y, ch, scale, color);
        cursor_x += 4 * scale;
    }
}

fn draw_glyph_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    ch: char,
    scale: usize,
    color: [u8; 4],
) {
    let glyph = glyph_pattern(ch);
    for (row, pattern) in glyph.iter().enumerate() {
        for col in 0..3usize {
            if (pattern >> (2 - col)) & 1 == 0 {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    set_pixel_rgba(
                        fb,
                        width,
                        height,
                        x + col * scale + sx,
                        y + row * scale + sy,
                        color,
                    );
                }
            }
        }
    }
}

pub fn fill_rect_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    color: [u8; 4],
) {
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for py in y..max_y {
        for px in x..max_x {
            fb[py * width + px] = color;
        }
    }
}

pub fn stroke_rect_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_w: usize,
    rect_h: usize,
    color: [u8; 4],
) {
    if rect_w == 0 || rect_h == 0 {
        return;
    }
    let max_x = (x + rect_w).min(width);
    let max_y = (y + rect_h).min(height);
    for px in x..max_x {
        set_pixel_rgba(fb, width, height, px, y, color);
        set_pixel_rgba(fb, width, height, px, max_y.saturating_sub(1), color);
    }
    for py in y..max_y {
        set_pixel_rgba(fb, width, height, x, py, color);
        set_pixel_rgba(fb, width, height, max_x.saturating_sub(1), py, color);
    }
}

pub fn text_width(text: &str, scale: usize) -> usize {
    text.chars().count() * 4 * scale
}

fn set_pixel_rgba(
    fb: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    color: [u8; 4],
) {
    if x < width && y < height {
        fb[y * width + x] = color;
    }
}
