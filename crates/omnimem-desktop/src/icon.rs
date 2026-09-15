//! The tray and window icon, drawn in code until the real artwork lands with
//! the installers: an orange ring on navy, anti-aliased.

/// RGBA pixels for a `size` by `size` icon.
pub fn rgba(size: u32) -> Vec<u8> {
    let (navy, orange) = ([27u8, 34, 51], [255u8, 142, 1]);
    let centre = (size as f32 - 1.0) / 2.0;
    let outer = size as f32 / 2.0;
    let (ring_out, ring_in) = (outer * 0.72, outer * 0.44);
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            // Coverage of the disc edge, and of the ring inside it.
            let disc = (outer - d).clamp(0.0, 1.0);
            let ring = ((ring_out - d).clamp(0.0, 1.0)) * ((d - ring_in).clamp(0.0, 1.0));
            let colour: Vec<u8> = navy
                .iter()
                .zip(orange)
                .map(|(n, o)| (f32::from(*n) * (1.0 - ring) + f32::from(o) * ring).round() as u8)
                .collect();
            pixels.extend_from_slice(&colour);
            pixels.push((disc * 255.0).round() as u8);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_square_rgba_with_transparent_corners() {
        let icon = rgba(32);
        assert_eq!(icon.len(), 32 * 32 * 4);
        assert_eq!(icon[3], 0, "the corner is transparent");
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(icon[centre + 3], 255, "the middle is opaque");
    }
}
