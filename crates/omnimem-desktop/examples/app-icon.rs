//! Writes the macOS app icon as an iconset for `iconutil -c icns`, which
//! scripts/macos-package.sh turns into OmniMem.icns:
//!
//!     cargo run -p omnimem-desktop --example app-icon -- OmniMem.iconset

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use omnimem_desktop::icon::app_icon;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: app-icon <iconset directory>")?,
    );
    std::fs::create_dir_all(&dir)?;
    // Each size iconutil expects, at 1x and 2x.
    for points in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let size = points * scale;
            let name = match scale {
                1 => format!("icon_{points}x{points}.png"),
                _ => format!("icon_{points}x{points}@{scale}x.png"),
            };
            let file = BufWriter::new(File::create(dir.join(name))?);
            let mut encoder = png::Encoder::new(file, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&app_icon(size))?;
        }
    }
    Ok(())
}
