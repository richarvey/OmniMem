//! Writes the app icon for the installers. Given a folder, the macOS iconset
//! for `iconutil -c icns`, which scripts/macos-package.sh turns into
//! OmniMem.icns; given a path ending in `.ico`, the Windows icon
//! scripts/windows-package.sh puts on the MSI's shortcut:
//!
//!     cargo run -p omnimem-desktop --example app-icon -- OmniMem.iconset
//!     cargo run -p omnimem-desktop --example app-icon -- OmniMem.ico

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use omnimem_desktop::icon::app_icon;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: app-icon <iconset directory | file.ico>")?,
    );
    if out
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ico"))
    {
        write_ico(&out)
    } else {
        write_iconset(&out)
    }
}

/// Each size iconutil expects, at 1x and 2x.
fn write_iconset(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    for points in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let size = points * scale;
            let name = match scale {
                1 => format!("icon_{points}x{points}.png"),
                _ => format!("icon_{points}x{points}@{scale}x.png"),
            };
            std::fs::write(dir.join(name), png(size)?)?;
        }
    }
    Ok(())
}

/// An ICO holding PNG images at the sizes Windows asks for, from the 16
/// pixel list view to the 256 pixel tile.
fn write_ico(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let images = sizes.map(png);
    let mut file = BufWriter::new(File::create(path)?);
    // ICONDIR: reserved, type 1 (icon), image count.
    file.write_all(&[0, 0, 1, 0])?;
    file.write_all(&(sizes.len() as u16).to_le_bytes())?;
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, image) in sizes.iter().zip(&images) {
        let image = image.as_ref().map_err(|e| e.to_string())?;
        // ICONDIRENTRY: width and height (0 means 256), no palette, one
        // plane, 32 bits per pixel, then where the PNG is and how long.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        file.write_all(&[side, side, 0, 0])?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&32u16.to_le_bytes())?;
        file.write_all(&(image.len() as u32).to_le_bytes())?;
        file.write_all(&offset.to_le_bytes())?;
        offset += image.len() as u32;
    }
    for image in images {
        file.write_all(&image.map_err(|e| e.to_string())?)?;
    }
    Ok(())
}

/// The app icon at `size` pixels square, as a PNG.
fn png(size: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&app_icon(size))?;
    Ok(bytes)
}
