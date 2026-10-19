//! The tray and window icon, drawn in code until the real artwork lands with
//! the installers: an orange ring on navy, anti-aliased. The macOS menu bar
//! has its own, `<o>` from the logo's wordmark.

/// The logo's green, from the chevrons around "omnimem".
const GREEN: [u8; 3] = [0x63, 0xBD, 0x85];
/// Apple's system grey, which reads on a light or a dark menu bar. The ring
/// can't follow the menu bar the way a template image does, since a template
/// image loses its colour and the green with it, and from macOS 26 the bar is
/// transparent over whatever wallpaper is behind it.
const GREY: [u8; 3] = [0x8E, 0x8E, 0x93];

// The `<o>` geometry, as fractions of the icon's height.
const STROKE: f32 = 0.13;
const CHEVRON_HALF: f32 = 0.30;
const CHEVRON_DEPTH: f32 = 0.20;
const RING: f32 = 0.27;
const DOT: f32 = 0.10;
const GAP: f32 = 0.06;
const MARGIN: f32 = 0.02;

/// RGBA pixels for the macOS menu bar icon, `height` pixels tall, and its
/// width: green chevrons either side of a grey ring with a green dot in the
/// middle. macOS draws it 18 points tall, so 36 is sharp on a Retina screen.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn menu_bar(height: u32) -> (Vec<u8>, u32) {
    let h = height as f32;
    let half = STROKE / 2.0;
    let left_tip = MARGIN + half;
    let left_arms = left_tip + CHEVRON_DEPTH;
    let ring_x = left_arms + half + GAP + half + RING;
    let right_arms = ring_x + RING + half + GAP + half;
    let right_tip = right_arms + CHEVRON_DEPTH;
    let units = right_tip + half + MARGIN;
    let width = (units * h).ceil() as u32;
    // Centre the drawing in the whole pixels it was rounded up to.
    let shift = (width as f32 - units * h) / 2.0;

    let chevron = |tip: f32, arms: f32| {
        [
            ((arms, 0.5 - CHEVRON_HALF), (tip, 0.5)),
            ((tip, 0.5), (arms, 0.5 + CHEVRON_HALF)),
        ]
    };
    let strokes: Vec<_> = chevron(left_tip, left_arms)
        .into_iter()
        .chain(chevron(right_tip, right_arms))
        .map(|((ax, ay), (bx, by))| ((ax * h + shift, ay * h), (bx * h + shift, by * h)))
        .collect();
    let centre = (ring_x * h + shift, 0.5 * h);

    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let chevrons = strokes
                .iter()
                .map(|&(a, b)| coverage(segment_distance(p, a, b) - half * h))
                .fold(0.0, f32::max);
            let from_centre = ((p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2)).sqrt();
            let ring = coverage((from_centre - RING * h).abs() - half * h);
            let dot = coverage(from_centre - DOT * h);
            // The shapes never touch, so whichever covers the pixel colours it.
            let green = chevrons.max(dot);
            let colour = if green >= ring { GREEN } else { GREY };
            pixels.extend_from_slice(&colour);
            pixels.push((green.max(ring) * 255.0).round() as u8);
        }
    }
    (pixels, width)
}

/// How much of a pixel a shape covers, from the signed distance in pixels
/// between the pixel's centre and the shape's edge (negative inside).
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Distance from `p` to the line segment from `a` to `b`.
fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * abx).powi(2) + (p.1 - a.1 - t * aby).powi(2)).sqrt()
}

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

    #[test]
    fn the_menu_bar_icon_is_a_green_dot_in_a_grey_ring_between_green_chevrons() {
        let (icon, width) = menu_bar(36);
        assert_eq!(icon.len(), (width * 36 * 4) as usize);
        assert!(width > 36, "wider than it is tall: {width}");
        let at = |x: u32, y: u32| {
            let i = ((y * width + x) * 4) as usize;
            &icon[i..i + 4]
        };
        assert_eq!(at(0, 0)[3], 0, "the corner is transparent");
        assert_eq!(at(width / 2, 18), [GREEN[0], GREEN[1], GREEN[2], 255]);
        assert_eq!(
            at(width / 2, 12)[3],
            0,
            "a gap between the dot and the ring"
        );
        assert_eq!(at(width / 2, 8)[..3], GREY, "the ring above the dot");
        assert_eq!(at(3, 17)[..3], GREEN, "the left chevron's tip");
        assert_eq!(at(width - 4, 17)[..3], GREEN, "the right chevron's tip");
        let row: Vec<u8> = (0..width).map(|x| at(x, 10)[3]).collect();
        let mirrored: Vec<u8> = row.iter().rev().copied().collect();
        assert_eq!(row, mirrored, "symmetric left to right");
    }
}
