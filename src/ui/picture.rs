//! Picture previews in the terminal. Terminals that speak a graphics
//! protocol (kitty, sixel, iTerm2) get a sharp preview; every other terminal
//! gets one drawn with half blocks (`▀`, two pixels per cell), which works
//! anywhere with colour. Previews are small: the full picture stays on disk.

use super::theme::{Depth, nearest_xterm, theme};
use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image as ImageWidget, Resize};
use std::sync::OnceLock;

/// A decoded picture and the bytes it came from.
#[derive(Clone)]
pub struct Picture {
    pub bytes: Vec<u8>,
    pub image: DynamicImage,
    /// "png", "jpeg", "webp" or "gif": also the file extension.
    pub format: &'static str,
}

impl Picture {
    pub fn decode(bytes: Vec<u8>) -> Result<Self> {
        let format = image::guess_format(&bytes).context("not a picture this preview can read")?;
        let image = image::load_from_memory_with_format(&bytes, format).context("could not read the picture")?;
        let format = match format {
            image::ImageFormat::Jpeg => "jpeg",
            image::ImageFormat::WebP => "webp",
            image::ImageFormat::Gif => "gif",
            _ => "png",
        };
        Ok(Self { bytes, image, format })
    }

    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::decode(std::fs::read(path)?)
    }

    pub fn extension(&self) -> &'static str {
        if self.format == "jpeg" { "jpg" } else { self.format }
    }

    /// "1024×1024 · PNG · 1.4 MB"
    pub fn describe(&self) -> String {
        let (w, h) = self.image.dimensions();
        format!("{w}×{h} · {} · {}", self.format.to_uppercase(), size_label(self.bytes.len()))
    }
}

/// `folder/name`, or `name-2`, `name-3`… when that is taken.
pub fn unique_path(folder: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, "png"));
    (2..)
        .map(|n| folder.join(format!("{stem}-{n}.{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Where "Save as" puts a picture: a folder gets `default_name`, a name
/// without an extension gets `extension`, and `~/` means the home folder.
pub fn save_as_target(typed: &str, default_name: &str, extension: &str) -> std::path::PathBuf {
    let typed = typed.trim();
    let mut path = match typed.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => std::path::PathBuf::from(typed),
    };
    if typed.ends_with('/') || path.is_dir() {
        path = path.join(default_name);
    } else if path.extension().is_none() {
        path.set_extension(extension);
    }
    path
}

pub fn size_label(bytes: usize) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.0} KB", bytes as f64 / 1000.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

/// Cells (columns, rows) that show a `w`×`h` picture within the limits,
/// keeping its shape. A cell is about twice as tall as it is wide.
pub fn fit(w: u32, h: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if w == 0 || h == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let aspect = w as f64 / h as f64;
    let mut cols = max_cols as f64;
    let mut rows = cols / aspect / 2.0;
    if rows > max_rows as f64 {
        rows = max_rows as f64;
        cols = rows * 2.0 * aspect;
    }
    ((cols.round() as u16).clamp(1, max_cols), (rows.round() as u16).clamp(1, max_rows))
}

/// The picture drawn with half blocks in `cols`×`rows` cells. `None` when
/// the terminal shows no colour, so callers can describe it in words instead.
pub fn blocks(image: &DynamicImage, cols: u16, rows: u16) -> Option<Vec<Line<'static>>> {
    let depth = theme().depth;
    if depth < Depth::Ansi256 || cols == 0 || rows == 0 {
        return None;
    }
    let small = image.resize_exact(cols as u32, rows as u32 * 2, FilterType::Triangle).to_rgba8();
    let colour = |x: u32, y: u32| -> Color {
        let [r, g, b, a] = small.get_pixel(x, y).0;
        // Transparent parts sit on the background colour.
        let mix = |c: u8, bg: u8| ((c as u32 * a as u32 + bg as u32 * (255 - a as u32)) / 255) as u8;
        let rgb = (mix(r, 0x0B), mix(g, 0x0C), mix(b, 0x0E));
        match depth {
            Depth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            _ => Color::Indexed(nearest_xterm(rgb)),
        }
    };
    Some(
        (0..rows as u32)
            .map(|row| {
                Line::from(
                    (0..cols as u32)
                        .map(|x| Span::styled("▀", Style::default().fg(colour(x, row * 2)).bg(colour(x, row * 2 + 1))))
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
    )
}

/// How this terminal previews pictures.
#[derive(Clone)]
pub enum Preview {
    Off,
    Blocks,
    Graphics(Picker),
}

static PREVIEW: OnceLock<Preview> = OnceLock::new();

/// Decide once per run. `setting` is images.preview from settings.json.
/// Asking the terminal about graphics means writing a query and reading the
/// answer, which only happens when the terminal looks like it can draw
/// pictures (or the setting asks for a protocol): a terminal that never
/// answers would otherwise hold up the preview.
pub fn preview(setting: &str) -> &'static Preview {
    PREVIEW.get_or_init(|| {
        if theme().depth == Depth::None && !matches!(setting, "kitty" | "sixel" | "iterm2") {
            return Preview::Off;
        }
        let forced = match setting {
            "off" | "none" => return Preview::Off,
            "blocks" | "halfblocks" => return Preview::Blocks,
            "kitty" => Some(ProtocolType::Kitty),
            "sixel" => Some(ProtocolType::Sixel),
            "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
            _ => None,
        };
        if forced.is_none() && !graphics_hinted() {
            return Preview::Blocks;
        }
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        if let Some(protocol) = forced {
            picker.set_protocol_type(protocol);
        }
        match picker.protocol_type() {
            ProtocolType::Halfblocks => Preview::Blocks,
            _ => Preview::Graphics(picker),
        }
    })
}

/// Terminals known to draw pictures, from what they put in the environment.
fn graphics_hinted() -> bool {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let term = var("TERM");
    let program = var("TERM_PROGRAM");
    !var("KITTY_WINDOW_ID").is_empty()
        || !var("WEZTERM_EXECUTABLE").is_empty()
        || !var("KONSOLE_VERSION").is_empty()
        || !var("WT_SESSION").is_empty()
        || var("LC_TERMINAL") == "iTerm2"
        || ["iTerm.app", "WezTerm", "ghostty", "rio", "mintty", "Tabby"].contains(&program.as_str())
        || ["kitty", "ghostty", "foot", "mlterm", "contour", "wezterm", "yaft"].iter().any(|name| term.contains(name))
}

/// A sharp preview for graphics terminals, sized to fit `cols`×`rows`.
pub fn graphics(picker: &Picker, image: &DynamicImage, cols: u16, rows: u16) -> Option<Protocol> {
    picker
        .new_protocol(image.clone(), Size::new(cols, rows), Resize::Fit(Some(FilterType::Triangle)))
        .ok()
}

/// Draw a graphics preview into a buffer area.
pub fn render_graphics(protocol: &Protocol, area: Rect, buf: &mut Buffer) {
    ImageWidget::new(protocol).render(area, buf);
}

/// The cells a graphics preview takes up.
pub fn graphics_size(protocol: &Protocol) -> (u16, u16) {
    let size = protocol.size();
    (size.width, size.height)
}

/// Print a preview into ordinary terminal output (for `aihat image`), with
/// `indent` columns before it. Returns false when there is nothing to show.
pub fn print(picture: &Picture, setting: &str, indent: u16, max_cols: u16, max_rows: u16) -> bool {
    use std::io::Write;
    let (w, h) = picture.image.dimensions();
    let (cols, rows) = fit(w, h, max_cols, max_rows);
    let mut out = std::io::stdout().lock();
    let pad = " ".repeat(indent as usize);
    match preview(setting) {
        Preview::Off => false,
        Preview::Blocks => {
            let Some(lines) = blocks(&picture.image, cols, rows) else {
                return false;
            };
            for line in lines {
                let _ = writeln!(out, "{pad}{}", super::ansi::line(&line));
            }
            true
        }
        Preview::Graphics(picker) => {
            let Some(protocol) = graphics(picker, &picture.image, cols, rows) else {
                return false;
            };
            let (used_cols, used_rows) = graphics_size(&protocol);
            let area = Rect::new(0, 0, used_cols.max(1), used_rows.max(1));
            let mut buf = Buffer::empty(area);
            render_graphics(&protocol, area, &mut buf);
            // Make room first so drawing never scrolls, then draw from the top
            // and come back to the line under the picture.
            let _ = write!(out, "{}", "\n".repeat(used_rows as usize));
            let _ = write!(out, "\x1b[{used_rows}A\r");
            for y in 0..used_rows {
                let _ = write!(out, "{pad}");
                for x in 0..used_cols {
                    let Some(cell) = buf.cell((x, y)) else { continue };
                    if cell.diff_option == ratatui::buffer::CellDiffOption::Skip {
                        let _ = write!(out, "\x1b[1C");
                        continue;
                    }
                    let symbol = cell.symbol();
                    if symbol.len() > 8 {
                        // The picture itself: drawn from here without moving the cursor.
                        let _ = write!(out, "\x1b7{symbol}\x1b8\x1b[1C");
                    } else {
                        let line = Line::from(Span::styled(symbol.to_string(), cell.style()));
                        let _ = write!(out, "{}", super::ansi::line(&line));
                    }
                }
                let _ = write!(out, "\r\n");
            }
            let _ = out.flush();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_the_shape() {
        assert_eq!(fit(1024, 1024, 40, 40), (40, 20));
        assert_eq!(fit(1024, 1024, 40, 10), (20, 10));
        assert_eq!(fit(1344, 768, 48, 24), (48, 14));
        assert_eq!(fit(0, 10, 40, 10), (0, 0));
    }

    #[test]
    fn saving_never_overwrites_and_folders_get_a_name() {
        let dir = std::env::temp_dir().join(format!("aihat-picture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a.png"));
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a-2.png"));
        let folder = format!("{}/", dir.display());
        assert_eq!(save_as_target(&folder, "aihat-1.png", "png"), dir.join("aihat-1.png"));
        assert_eq!(save_as_target(&dir.join("lighthouse").display().to_string(), "x.png", "jpg"), dir.join("lighthouse.jpg"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decodes_a_png_and_names_its_format() {
        let mut bytes = Vec::new();
        DynamicImage::new_rgb8(4, 2)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let picture = Picture::decode(bytes).unwrap();
        assert_eq!(picture.format, "png");
        assert!(picture.describe().starts_with("4×2 · PNG"));
    }
}
