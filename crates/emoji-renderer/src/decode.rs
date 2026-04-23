use crate::texture::{COLOR_SOURCE_ALPHA_THRESHOLD, fill_transparent_rgb_from_nearest};

pub fn decode_emoji_frames(data: &[u8]) -> Option<(Vec<Vec<[u8; 4]>>, Vec<u32>, u32, u32)> {
    if data.len() >= 6 && (&data[0..4] == b"GIF8") {
        if let Ok(decoder) = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)) {
            use image::AnimationDecoder;
            let frames: Vec<image::Frame> = decoder.into_frames().filter_map(|f| f.ok()).collect();
            if frames.len() > 1 {
                let w = frames[0].buffer().width();
                let h = frames[0].buffer().height();
                let mut rgba_frames = Vec::with_capacity(frames.len());
                let mut delays = Vec::with_capacity(frames.len());
                for f in &frames {
                    let buf = f.buffer();
                    let resized = if buf.width() != w || buf.height() != h {
                        image::imageops::resize(buf, w, h, image::imageops::FilterType::Nearest)
                    } else {
                        buf.clone()
                    };
                    let mut pixels: Vec<[u8; 4]> = resized.pixels().map(|p| p.0).collect();
                    fill_transparent_rgb_from_nearest(
                        &mut pixels,
                        w,
                        h,
                        COLOR_SOURCE_ALPHA_THRESHOLD,
                    );
                    rgba_frames.push(pixels);
                    let (numer, denom) = f.delay().numer_denom_ms();
                    delays.push(if denom == 0 { numer } else { numer / denom });
                }
                return Some((rgba_frames, delays, w, h));
            }
        }
    }

    let img = image::load_from_memory(data).ok()?;
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    let mut pixels: Vec<[u8; 4]> = rgba.pixels().map(|p| p.0).collect();
    fill_transparent_rgb_from_nearest(&mut pixels, w, h, COLOR_SOURCE_ALPHA_THRESHOLD);
    Some((vec![pixels], vec![0], w, h))
}
