//! The icons, drawn in code, all from the `<o>` in the logo's wordmark: the
//! macOS menu bar icon, the square tray icon everywhere else, and the app
//! icon, which is also the window's.

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

/// The `<o>`'s proportions, as fractions of its height.
#[derive(Clone, Copy)]
struct Shape {
    stroke: f32,
    chevron_half: f32,
    chevron_depth: f32,
    ring: f32,
    dot: f32,
    gap: f32,
    margin: f32,
}

/// The `<o>` as the menu bar and the app icon draw it.
const WIDE: Shape = Shape {
    stroke: 0.13,
    chevron_half: 0.30,
    chevron_depth: 0.20,
    ring: 0.27,
    dot: 0.10,
    gap: 0.06,
    margin: 0.02,
};

/// A tighter `<o>` for trays that fit every icon into a square (GNOME's
/// AppIndicator extension, KDE, Windows), so it keeps most of the height
/// there rather than shrinking to two thirds of it.
const SQUARE: Shape = Shape {
    stroke: 0.14,
    chevron_half: 0.27,
    chevron_depth: 0.12,
    ring: 0.25,
    dot: 0.09,
    gap: 0.04,
    margin: 0.01,
};

impl Shape {
    /// The mark's width for a height of one.
    fn units(self) -> f32 {
        let half = self.stroke / 2.0;
        2.0 * (self.margin + half + self.chevron_depth + half + self.gap + half + self.ring)
    }
}

/// The `<o>` laid out `height` pixels tall with its box's top left corner at
/// `origin`.
struct Mark {
    shape: Shape,
    strokes: Vec<((f32, f32), (f32, f32))>,
    centre: (f32, f32),
    height: f32,
}

impl Mark {
    fn new(shape: Shape, height: f32, origin: (f32, f32)) -> Self {
        let half = shape.stroke / 2.0;
        let left_tip = shape.margin + half;
        let left_arms = left_tip + shape.chevron_depth;
        let ring_x = left_arms + half + shape.gap + half + shape.ring;
        let right_arms = ring_x + shape.ring + half + shape.gap + half;
        let right_tip = right_arms + shape.chevron_depth;
        let place = |(x, y): (f32, f32)| (origin.0 + x * height, origin.1 + y * height);
        let chevron = |tip: f32, arms: f32| {
            [
                ((arms, 0.5 - shape.chevron_half), (tip, 0.5)),
                ((tip, 0.5), (arms, 0.5 + shape.chevron_half)),
            ]
        };
        Self {
            shape,
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
        let half = self.shape.stroke / 2.0 * self.height;
        let chevrons = self
            .strokes
            .iter()
            .map(|&(a, b)| coverage(segment_distance(p, a, b) - half))
            .fold(0.0, f32::max);
        let from_centre = ((p.0 - self.centre.0).powi(2) + (p.1 - self.centre.1).powi(2)).sqrt();
        let ring = coverage((from_centre - self.shape.ring * self.height).abs() - half);
        let dot = coverage(from_centre - self.shape.dot * self.height);
        (chevrons.max(dot), ring)
    }

    /// RGBA pixels of the mark alone, green with a grey ring, on a
    /// transparent `width` by `height` canvas.
    fn pixels(&self, width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let (green, ring) = self.coverage((x as f32 + 0.5, y as f32 + 0.5));
                pixels.extend_from_slice(if green >= ring { &GREEN } else { &GREY });
                pixels.push((green.max(ring) * 255.0).round() as u8);
            }
        }
        pixels
    }
}

/// RGBA pixels for the macOS menu bar icon, `height` pixels tall, and its
/// width: green chevrons either side of a grey ring with a green dot in the
/// middle. macOS draws it 18 points tall, so 36 is sharp on a Retina screen.
pub fn menu_bar(height: u32) -> (Vec<u8>, u32) {
    let h = height as f32;
    let width = (WIDE.units() * h).ceil() as u32;
    // Centre the drawing in the whole pixels it was rounded up to.
    let mark = Mark::new(WIDE, h, ((width as f32 - WIDE.units() * h) / 2.0, 0.0));
    (mark.pixels(width, height), width)
}

/// RGBA pixels for the tray icon on Linux and Windows, `size` pixels square:
/// the compact `<o>` across its width, centred, in the menu bar's colours,
/// which read on a light panel or GNOME's dark top bar.
pub fn tray(size: u32) -> Vec<u8> {
    let side = size as f32;
    let height = side / SQUARE.units();
    Mark::new(SQUARE, height, (0.0, (side - height) / 2.0)).pixels(size, size)
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
        WIDE,
        mark_height,
        (
            centre - WIDE.units() * mark_height / 2.0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_icon_is_the_compact_mark_across_a_square() {
        let icon = tray(64);
        assert_eq!(icon.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &icon[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "the corner is transparent");
        assert_eq!(at(32, 32), [GREEN[0], GREEN[1], GREEN[2], 255], "the dot");
        assert_eq!(at(1, 32)[..3], GREEN, "the left chevron reaches the edge");
        assert_eq!(at(62, 32)[..3], GREEN, "and the right one");
        // The ring's top, a quarter of the mark's height above the centre.
        let ring_top = (32.0 - SQUARE.ring * 64.0 / SQUARE.units()) as usize;
        assert_eq!(at(32, ring_top)[..3], GREY, "the ring");
        // The square shape keeps far more of the height than the wide one.
        assert!(64.0 / SQUARE.units() > 0.75 * 64.0);
        assert!(64.0 / WIDE.units() < 0.70 * 64.0);
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
