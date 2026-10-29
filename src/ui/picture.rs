//! Picture previews in the terminal. Terminals that speak a graphics
//! protocol (kitty, sixel, iTerm2) get the picture itself, found by asking
//! the terminal; every other terminal gets one drawn with block symbols:
//! each cell takes the half, quarter or eighth block and the two colours
//! that match its part of the picture best. The full picture stays on disk.

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
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;

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

/// A cell's size in pixels (width, height), as the terminal answered it.
static CELL: OnceLock<(u16, u16)> = OnceLock::new();

/// Set once the terminal was asked about graphics: a terminal that shows
/// part of the question needs the screen drawn again.
static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the terminal was just asked (once).
pub fn take_asked() -> bool {
    ASKED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// A cell's size in pixels: from the terminal's answer, or its window size.
fn cell_pixels() -> Option<(u16, u16)> {
    CELL.get().copied().or_else(|| {
        crossterm::terminal::window_size()
            .ok()
            .filter(|size| size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0)
            .map(|size| (size.width / size.columns, size.height / size.rows))
            .filter(|(w, h)| *w > 0 && *h > 0)
    })
}

/// How much taller a cell is than wide: from the terminal when it tells
/// (most do), else the usual 2.
pub fn cell_aspect() -> f64 {
    cell_pixels()
        .map(|(w, h)| h as f64 / w as f64)
        .filter(|aspect| (1.2..=3.5).contains(aspect))
        .unwrap_or(2.0)
}

/// Cells (columns, rows) that show a `w`×`h` picture within the limits,
/// keeping its shape on this terminal's cells.
pub fn fit(w: u32, h: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    fit_cells(w, h, max_cols, max_rows, cell_aspect())
}

/// [`fit`] for cells `aspect` times taller than wide.
pub fn fit_cells(w: u32, h: u32, max_cols: u16, max_rows: u16, aspect: f64) -> (u16, u16) {
    if w == 0 || h == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let shape = w as f64 / h as f64;
    let mut cols = max_cols as f64;
    let mut rows = cols / shape / aspect;
    if rows > max_rows as f64 {
        rows = max_rows as f64;
        cols = rows * aspect * shape;
    }
    ((cols.round() as u16).clamp(1, max_cols), (rows.round() as u16).clamp(1, max_rows))
}

/// A cell is read as 8×16 pixels.
const CELL_W: usize = 8;
const CELL_H: usize = 16;

/// Part of a cell: (x0, y0, x1, y1) in its 8×16 pixels.
type Part = (usize, usize, usize, usize);

/// Block symbols and the parts of the cell each one covers. Shapes that are
/// another's opposite (▀ is ▄ with the colours swapped) are left out.
const SHAPES: [(&str, &[Part]); 20] = [
    ("▁", &[(0, 14, 8, 16)]),
    ("▂", &[(0, 12, 8, 16)]),
    ("▃", &[(0, 10, 8, 16)]),
    ("▄", &[(0, 8, 8, 16)]),
    ("▅", &[(0, 6, 8, 16)]),
    ("▆", &[(0, 4, 8, 16)]),
    ("▇", &[(0, 2, 8, 16)]),
    ("▏", &[(0, 0, 1, 16)]),
    ("▎", &[(0, 0, 2, 16)]),
    ("▍", &[(0, 0, 3, 16)]),
    ("▌", &[(0, 0, 4, 16)]),
    ("▋", &[(0, 0, 5, 16)]),
    ("▊", &[(0, 0, 6, 16)]),
    ("▉", &[(0, 0, 7, 16)]),
    ("▘", &[(0, 0, 4, 8)]),
    ("▝", &[(4, 0, 8, 8)]),
    ("▖", &[(0, 8, 4, 16)]),
    ("▗", &[(4, 8, 8, 16)]),
    ("▚", &[(0, 0, 4, 8), (4, 8, 8, 16)]),
    ("▞", &[(4, 0, 8, 8), (0, 8, 4, 16)]),
];

/// A pixel on the chat background (transparent parts show the background).
fn on_background([r, g, b, a]: [u8; 4]) -> [f32; 3] {
    let a = a as f32 / 255.0;
    let mix = |c: u8, bg: u8| c as f32 * a + bg as f32 * (1.0 - a);
    [mix(r, 0x0B), mix(g, 0x0C), mix(b, 0x0E)]
}

fn cell_colour(rgb: [f32; 3], depth: Depth) -> Color {
    let [r, g, b] = rgb.map(|c| c.round().clamp(0.0, 255.0) as u8);
    match depth {
        Depth::TrueColor => Color::Rgb(r, g, b),
        _ => Color::Indexed(nearest_xterm((r, g, b))),
    }
}

/// The picture drawn with block symbols in `cols`×`rows` cells: each cell
/// gets the shape and the two colours closest to its 8×16 pixels, so edges
/// and small details come out far sharper than with half blocks alone.
/// `None` when the terminal shows no colour.
pub fn symbols(image: &DynamicImage, cols: u16, rows: u16) -> Option<Vec<Line<'static>>> {
    symbols_at(image, cols, rows, theme().depth)
}

fn symbols_at(image: &DynamicImage, cols: u16, rows: u16, depth: Depth) -> Option<Vec<Line<'static>>> {
    if depth < Depth::Ansi256 || cols == 0 || rows == 0 {
        return None;
    }
    let small = image.resize_exact(cols as u32 * CELL_W as u32, rows as u32 * CELL_H as u32, FilterType::Lanczos3).to_rgba8();
    let area = (CELL_W * CELL_H) as f32;
    let lines = (0..rows as usize)
        .map(|row| {
            let spans: Vec<Span<'static>> = (0..cols as usize)
                .map(|col| {
                    // Running sums over the cell, so any rectangle adds up at once.
                    let mut sums = [[[0f32; 3]; CELL_W + 1]; CELL_H + 1];
                    for y in 0..CELL_H {
                        for x in 0..CELL_W {
                            let pixel = on_background(small.get_pixel((col * CELL_W + x) as u32, (row * CELL_H + y) as u32).0);
                            for c in 0..3 {
                                sums[y + 1][x + 1][c] = pixel[c] + sums[y][x + 1][c] + sums[y + 1][x][c] - sums[y][x][c];
                            }
                        }
                    }
                    let rect = |(x0, y0, x1, y1): Part| -> [f32; 3] {
                        std::array::from_fn(|c| sums[y1][x1][c] - sums[y0][x1][c] - sums[y1][x0][c] + sums[y0][x0][c])
                    };
                    let total = rect((0, 0, CELL_W, CELL_H));
                    // The best split leaves the least colour error, which is
                    // the one with the most squared colour between its parts.
                    let flat: f32 = total.iter().map(|c| c * c / area).sum();
                    let mut best: Option<(&str, [f32; 3], [f32; 3], f32)> = None;
                    for (symbol, rects) in SHAPES {
                        let mut fg = [0f32; 3];
                        let mut count = 0f32;
                        for &r in rects {
                            let sum = rect(r);
                            (0..3).for_each(|c| fg[c] += sum[c]);
                            count += ((r.2 - r.0) * (r.3 - r.1)) as f32;
                        }
                        let rest = area - count;
                        let bg: [f32; 3] = std::array::from_fn(|c| total[c] - fg[c]);
                        let score: f32 = (0..3).map(|c| fg[c] * fg[c] / count + bg[c] * bg[c] / rest).sum();
                        if best.is_none_or(|(_, _, _, top)| score > top) {
                            best = Some((symbol, fg.map(|v| v / count), bg.map(|v| v / rest), score));
                        }
                    }
                    match best {
                        // Hardly any better than one colour: a plain cell.
                        Some((symbol, fg, bg, score)) if score - flat > 24.0 * area => {
                            Span::styled(symbol, Style::default().fg(cell_colour(fg, depth)).bg(cell_colour(bg, depth)))
                        }
                        _ => Span::styled(" ", Style::default().bg(cell_colour(total.map(|v| v / area), depth))),
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    Some(lines)
}

/// The picture drawn with half blocks (`▀`, two pixels per cell) in
/// `cols`×`rows` cells, for fonts without the other block symbols.
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
    /// Block symbols: halves, quarters and eighths.
    Blocks,
    /// Half blocks only.
    HalfBlocks,
    Graphics(Picker),
}

impl Preview {
    /// "sixel", "block symbols"…: for `aihat preview`.
    pub fn label(&self) -> &'static str {
        match self {
            Preview::Off => "no preview",
            Preview::Blocks => "block symbols",
            Preview::HalfBlocks => "half blocks",
            Preview::Graphics(picker) => match picker.protocol_type() {
                ProtocolType::Kitty => "kitty graphics",
                ProtocolType::Sixel => "sixel graphics",
                ProtocolType::Iterm2 => "iTerm2 inline pictures",
                ProtocolType::Halfblocks => "half blocks",
            },
        }
    }

    /// The picture as cells, for every way except a graphics protocol.
    pub fn cells(&self, image: &DynamicImage, cols: u16, rows: u16) -> Option<Vec<Line<'static>>> {
        match self {
            Preview::Blocks => symbols(image, cols, rows),
            Preview::HalfBlocks => blocks(image, cols, rows),
            _ => None,
        }
    }
}

static PREVIEW: OnceLock<Preview> = OnceLock::new();

/// Decide once per run, on the first picture. `setting` is images.preview
/// from settings.json: auto, kitty, sixel, iterm2, blocks, halfblocks or off.
/// On auto the terminal is asked what it can draw; every terminal answers
/// the status request that ends the question, and an answer that never
/// comes gives up after a second.
pub fn preview(setting: &str) -> &'static Preview {
    PREVIEW.get_or_init(|| choose(setting))
}

fn choose(setting: &str) -> Preview {
    let forced = match setting {
        "off" | "none" => return Preview::Off,
        "blocks" | "symbols" => return Preview::Blocks,
        "halfblocks" | "half" => return Preview::HalfBlocks,
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" => Some(ProtocolType::Sixel),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        _ => None,
    };
    if theme().depth == Depth::None && forced.is_none() {
        return Preview::Off;
    }
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    // Pictures inside tmux or screen need their own set-up: blocks there.
    let multiplexed = !var("TMUX").is_empty() || var("TERM").starts_with("tmux") || var("TERM").starts_with("screen");
    // Only a terminal can answer; a pipe would lose what was piped in.
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let answers = if interactive && !multiplexed && forced.is_none() { ask_terminal(Duration::from_millis(1500)) } else { None };
    if let Some(cell) = answers.as_ref().and_then(|a| a.cell) {
        let _ = CELL.set(cell);
    }
    let program = var("TERM_PROGRAM");
    let detected = if multiplexed {
        None
    } else if !var("WEZTERM_EXECUTABLE").is_empty() {
        // WezTerm draws iTerm2 pictures best.
        Some(ProtocolType::Iterm2)
    } else if !var("KONSOLE_VERSION").is_empty() {
        // Konsole's sixel and kitty support is incomplete.
        None
    } else if answers.as_ref().is_some_and(|a| a.kitty) {
        Some(ProtocolType::Kitty)
    } else if answers.as_ref().is_some_and(|a| a.sixel) {
        Some(ProtocolType::Sixel)
    } else if ["iTerm", "mintty", "vscode", "Tabby", "Hyper", "rio", "Bobcat", "WarpTerminal"].iter().any(|name| program.contains(name))
        || var("LC_TERMINAL").contains("iTerm")
    {
        Some(ProtocolType::Iterm2)
    } else {
        None
    };
    let Some(protocol) = forced.or(detected) else {
        return Preview::Blocks;
    };
    let (w, h) = cell_pixels().unwrap_or((10, 20));
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(w, h));
    picker.set_protocol_type(protocol);
    Preview::Graphics(picker)
}

/// What the terminal said it can draw, and its cell size in pixels.
#[derive(Debug, Default, PartialEq)]
struct Answers {
    kitty: bool,
    sixel: bool,
    /// (width, height)
    cell: Option<(u16, u16)>,
}

/// Read the answers to a kitty graphics query, device attributes (4 means
/// sixel) and the cell size in pixels (`ESC [ 6 ; h ; w t`).
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_answers(reply: &[u8]) -> Answers {
    let text = String::from_utf8_lossy(reply);
    let mut answers = Answers { kitty: text.contains("\x1b_Gi=31;OK"), ..Answers::default() };
    let mut rest = &*text;
    while let Some(start) = rest.find("\x1b[?") {
        let after = &rest[start + 3..];
        let end = after.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(after.len());
        if after[end..].starts_with('c') {
            answers.sixel |= after[..end].split(';').any(|p| p == "4");
        }
        rest = &after[end..];
    }
    if let Some(start) = text.find("\x1b[6;") {
        let after = &text[start + 4..];
        if let Some(end) = after.find('t') {
            let mut parts = after[..end].split(';').map(|p| p.parse::<u16>().ok().filter(|n| *n > 0));
            if let (Some(Some(h)), Some(Some(w))) = (parts.next(), parts.next()) {
                answers.cell = Some((w, h));
            }
        }
    }
    answers
}

/// Ask the terminal what it can draw. Every terminal answers the status
/// request that ends the question, so this waits only as long as the
/// terminal takes; nothing is left reading the keyboard afterwards.
#[cfg(unix)]
fn ask_terminal(timeout: Duration) -> Option<Answers> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    let fd = tty.as_raw_fd();
    // SAFETY: plain termios and poll calls on a file descriptor we own.
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return None;
    }
    let mut quiet = original;
    quiet.c_lflag &= !(libc::ICANON | libc::ECHO);
    quiet.c_cc[libc::VMIN] = 0;
    quiet.c_cc[libc::VTIME] = 0;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    let question = "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[16t\x1b[5n";
    ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut reply = Vec::new();
    if (&tty).write_all(question.as_bytes()).and_then(|_| (&tty).flush()).is_ok() {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            let mut waiting = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut waiting, 1, left.as_millis().clamp(1, 5000) as i32) } <= 0 {
                break;
            }
            let mut chunk = [0u8; 512];
            let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if read <= 0 {
                break;
            }
            reply.extend_from_slice(&chunk[..read as usize]);
            // The status answer comes last: everything is in.
            if reply.windows(4).any(|w| w == b"\x1b[0n" || w == b"\x1b[3n") {
                break;
            }
        }
    }
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    let answered = reply.windows(4).any(|w| w == b"\x1b[0n" || w == b"\x1b[3n");
    answered.then(|| parse_answers(&reply))
}

#[cfg(not(unix))]
fn ask_terminal(_timeout: Duration) -> Option<Answers> {
    // Windows consoles do not reliably pass answers back to programs.
    None
}

/// A picture in the chat, drawn with cells so it scrolls with the text:
/// half blocks when settings.json asks for them, else block symbols.
pub fn chat_cells(image: &DynamicImage, cols: u16, rows: u16) -> Option<Vec<Line<'static>>> {
    match PREVIEW.get() {
        Some(Preview::Off) => None,
        Some(Preview::HalfBlocks) => blocks(image, cols, rows),
        _ => symbols(image, cols, rows),
    }
}

/// Settle the way pictures are drawn now, when settings.json names one
/// (no question to the terminal needed).
pub fn settle(setting: &str) {
    if !matches!(setting, "" | "auto") {
        preview(setting);
    }
}

/// The picture itself for graphics terminals, sized to fit `cols`×`rows`.
pub fn graphics(picker: &Picker, image: &DynamicImage, cols: u16, rows: u16) -> Option<Protocol> {
    picker
        .new_protocol(image.clone(), Size::new(cols, rows), Resize::Fit(Some(FilterType::Lanczos3)))
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
    print_with(preview(setting), picture, indent, max_cols, max_rows)
}

/// [`print`] with a chosen way of drawing (`aihat preview --as`).
pub fn print_with(how: &Preview, picture: &Picture, indent: u16, max_cols: u16, max_rows: u16) -> bool {
    use std::io::Write;
    let (w, h) = picture.image.dimensions();
    let (cols, rows) = fit(w, h, max_cols, max_rows);
    let mut out = std::io::stdout().lock();
    let pad = " ".repeat(indent as usize);
    match how {
        Preview::Off => false,
        Preview::Blocks | Preview::HalfBlocks => {
            let Some(lines) = how.cells(&picture.image, cols, rows) else {
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
        assert_eq!(fit_cells(1024, 1024, 40, 40, 2.0), (40, 20));
        assert_eq!(fit_cells(1024, 1024, 40, 10, 2.0), (20, 10));
        assert_eq!(fit_cells(1344, 768, 48, 24, 2.0), (48, 14));
        assert_eq!(fit_cells(0, 10, 40, 10, 2.0), (0, 0));
        // Taller cells take fewer rows for the same picture.
        assert_eq!(fit_cells(1024, 1024, 40, 40, 2.5), (40, 16));
    }

    #[test]
    fn terminal_answers_are_read() {
        let kitty = parse_answers(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c\x1b[6;20;10t\x1b[0n");
        assert_eq!(kitty, Answers { kitty: true, sixel: false, cell: Some((10, 20)) });
        let sixel = parse_answers(b"\x1b[?64;1;2;4;6;9;15;18;21;22c\x1b[0n");
        assert!(sixel.sixel && !sixel.kitty && sixel.cell.is_none());
        let plain = parse_answers(b"\x1b[?1;2c\x1b[0n");
        assert_eq!(plain, Answers::default());
    }

    #[test]
    fn symbols_follow_edges_inside_a_cell() {
        // One cell: white on the left quarter, black elsewhere → ▎.
        let mut picture = image::RgbImage::new(8, 16);
        for (x, _, pixel) in picture.enumerate_pixels_mut() {
            *pixel = if x < 2 { image::Rgb([255, 255, 255]) } else { image::Rgb([0, 0, 0]) };
        }
        let lines = symbols_at(&DynamicImage::ImageRgb8(picture), 1, 1, Depth::TrueColor).unwrap();
        assert_eq!(lines[0].spans[0].content, "▎");
        // One flat colour → a plain cell.
        let flat = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 16, image::Rgb([200, 30, 30])));
        assert_eq!(symbols_at(&flat, 1, 1, Depth::TrueColor).unwrap()[0].spans[0].content, " ");
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
