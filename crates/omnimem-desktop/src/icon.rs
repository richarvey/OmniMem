//! The icons, drawn in code: the tray and window icon, an orange ring on
//! navy; and on macOS the `<o>` from the logo's wordmark, in the menu bar and
//! as the app icon.

/// The logo's green, from the chevrons around "omnimem".
const GREEN: [u8; 3] = [0x63, 0xBD, 0x85];
/// Apple's system grey, which reads on a light or a dark menu bar. The ring
/// can't follow the menu bar the way a template image does, since a template
/// image loses its colour and the green with it, and from macOS 26 the bar is
/// transparent over whatever wallpaper is behind it.
const GREY: [u8; 3] = [0x8E, 0x8E, 0x93];
/// The app icon's ring: the wordmark's colour in the logo for dark backgrounds.
const LIGHT: [u8; 3] = [0xE5, 0xE7, 0xEB];
/// The app icon's background, the logo's near-black at the bottom rising to
/// a slightly lighter navy at the top.
const NAVY_TOP: [u8; 3] = [0x1F, 0x2B, 0x3A];
const NAVY_BOTTOM: [u8; 3] = [0x12, 0x1B, 0x24];

// The `<o>` geometry, as fractions of the mark's height.
const STROKE: f32 = 0.13;
const CHEVRON_HALF: f32 = 0.30;
const CHEVRON_DEPTH: f32 = 0.20;
const RING: f32 = 0.27;
const DOT: f32 = 0.10;
const GAP: f32 = 0.06;
const MARGIN: f32 = 0.02;

/// The `<o>` laid out `height` pixels tall with its box's top left corner at
/// `origin`.
struct Mark {
    strokes: Vec<((f32, f32), (f32, f32))>,
    centre: (f32, f32),
    height: f32,
}

impl Mark {
    /// The mark's width for a height of one.
    fn units() -> f32 {
        let half = STROKE / 2.0;
        2.0 * (MARGIN + half + CHEVRON_DEPTH + half + GAP + half + RING)
    }

    fn new(height: f32, origin: (f32, f32)) -> Self {
        let half = STROKE / 2.0;
        let left_tip = MARGIN + half;
        let left_arms = left_tip + CHEVRON_DEPTH;
        let ring_x = left_arms + half + GAP + half + RING;
        let right_arms = ring_x + RING + half + GAP + half;
        let right_tip = right_arms + CHEVRON_DEPTH;
        let place = |(x, y): (f32, f32)| (origin.0 + x * height, origin.1 + y * height);
        let chevron = |tip: f32, arms: f32| {
            [
                ((arms, 0.5 - CHEVRON_HALF), (tip, 0.5)),
                ((tip, 0.5), (arms, 0.5 + CHEVRON_HALF)),
            ]
        };
        Self {
            strokes: chevron(left_tip, left_arms)
                .into_iter()
                .chain(chevron(right_tip, right_arms))
                .map(|(a, b)| (place(a), place(b)))
                .collect(),
            centre: place((ring_x, 0.5)),
            height,
        }
    }

    /// How much of the pixel centred on `p` the green parts (chevrons and
    /// dot) and the ring cover. The shapes never touch, so at most one of
    /// them covers any pixel.
    fn coverage(&self, p: (f32, f32)) -> (f32, f32) {
        let half = STROKE / 2.0 * self.height;
        let chevrons = self
            .strokes
            .iter()
            .map(|&(a, b)| coverage(segment_distance(p, a, b) - half))
            .fold(0.0, f32::max);
        let from_centre = ((p.0 - self.centre.0).powi(2) + (p.1 - self.centre.1).powi(2)).sqrt();
        let ring = coverage((from_centre - RING * self.height).abs() - half);
        let dot = coverage(from_centre - DOT * self.height);
        (chevrons.max(dot), ring)
    }
}

/// RGBA pixels for the macOS menu bar icon, `height` pixels tall, and its
/// width: green chevrons either side of a grey ring with a green dot in the
/// middle. macOS draws it 18 points tall, so 36 is sharp on a Retina screen.
pub fn menu_bar(height: u32) -> (Vec<u8>, u32) {
    let h = height as f32;
    let width = (Mark::units() * h).ceil() as u32;
    // Centre the drawing in the whole pixels it was rounded up to.
    let mark = Mark::new(h, ((width as f32 - Mark::units() * h) / 2.0, 0.0));
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (green, ring) = mark.coverage((x as f32 + 0.5, y as f32 + 0.5));
            pixels.extend_from_slice(if green >= ring { &GREEN } else { &GREY });
            pixels.push((green.max(ring) * 255.0).round() as u8);
        }
    }
    (pixels, width)
}

/// RGBA pixels for the macOS app icon, `size` pixels square: the `<o>` with
/// a light ring on a navy tile, laid out on Apple's 1024 point grid (an 824
/// point rounded square with a soft shadow under it), so it sits with the
/// other icons in Applications and the Dock.
pub fn app_icon(size: u32) -> Vec<u8> {
    let s = size as f32 / 1024.0;
    let (centre, half_side, radius) = (512.0 * s, 412.0 * s, 185.0 * s);
    let (shadow_drop, shadow_blur) = (10.0 * s, 16.0 * s);
    let mark_height = 400.0 * s;
    let mark = Mark::new(
        mark_height,
        (
            centre - Mark::units() * mark_height / 2.0,
            centre - mark_height / 2.0,
        ),
    );
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let tile = coverage(rounded_square_distance(p, centre, half_side, radius));
            let below =
                rounded_square_distance((p.0, p.1 - shadow_drop), centre, half_side, radius);
            let shadow = 0.3 * (1.0 - (below / shadow_blur).clamp(0.0, 1.0));
            // The tile's colour: the gradient, then the ring, then the green.
            let t = ((p.1 - (centre - half_side)) / (2.0 * half_side)).clamp(0.0, 1.0);
            let (green, ring) = mark.coverage(p);
            let colour: Vec<f32> = (0..3)
                .map(|c| {
                    let navy = mix(NAVY_TOP[c], NAVY_BOTTOM[c], t);
                    let with_ring = navy + (f32::from(LIGHT[c]) - navy) * ring;
                    with_ring + (f32::from(GREEN[c]) - with_ring) * green
                })
                .collect();
            // The tile over its black shadow, with straight alpha.
            let alpha = tile + shadow * (1.0 - tile);
            for value in colour {
                let over = if alpha > 0.0 {
                    value * tile / alpha
                } else {
                    0.0
                };
                pixels.push(over.round().clamp(0.0, 255.0) as u8);
            }
            pixels.push((alpha * 255.0).round() as u8);
        }
    }
    pixels
}

fn mix(from: u8, to: u8, t: f32) -> f32 {
    f32::from(from) + (f32::from(to) - f32::from(from)) * t
}

/// Signed distance from `p` to a square of half side `half` centred on
/// (`centre`, `centre`) with corners rounded to `radius`.
fn rounded_square_distance(p: (f32, f32), centre: f32, half: f32, radius: f32) -> f32 {
    let q = (
        (p.0 - centre).abs() - (half - radius),
        (p.1 - centre).abs() - (half - radius),
    );
    let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
    outside + q.0.max(q.1).min(0.0) - radius
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

    #[test]
    fn the_app_icon_is_the_mark_on_a_navy_tile_with_a_shadow() {
        let icon = app_icon(256);
        assert_eq!(icon.len(), 256 * 256 * 4);
        let at = |x: usize, y: usize| &icon[(y * 256 + x) * 4..(y * 256 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "outside the tile is transparent");
        assert_eq!(at(128, 128), [GREEN[0], GREEN[1], GREEN[2], 255], "the dot");
        assert_eq!(
            at(128, 101),
            [LIGHT[0], LIGHT[1], LIGHT[2], 255],
            "the ring"
        );
        let tile = at(40, 128);
        assert_eq!(tile[3], 255, "the tile is opaque");
        for c in 0..3 {
            let range = NAVY_BOTTOM[c].min(NAVY_TOP[c])..=NAVY_BOTTOM[c].max(NAVY_TOP[c]);
            assert!(range.contains(&tile[c]), "navy behind the mark: {tile:?}");
        }
        let shadow = at(128, 233);
        assert_eq!(shadow[..3], [0, 0, 0], "a black shadow under the tile");
        assert!(
            (1..255).contains(&shadow[3]),
            "and a soft one: {}",
            shadow[3]
        );
        let row: Vec<&[u8]> = (0..256).map(|x| at(x, 128)).collect();
        let mirrored: Vec<&[u8]> = row.iter().rev().copied().collect();
        assert_eq!(row, mirrored, "symmetric left to right");
    }
}
