use anyhow::{Context, Result};
use clap::Parser;
use image::{ImageBuffer, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Character ramp ────────────────────────────────────────────────────────────
// Dense-to-sparse ordering (index 0 = darkest, last = space/lightest).
// This 70-level ramp from Pétitcolas / Paul Bourke gives much smoother
// tonal gradation than a 10-char set — critical for detailed, high-contrast GIFs.
const ASCII_CHARS: &[char] = &[
    '$','@','B','%','8','&','W','M','#','*','o','a','h','k','b','d','p','q',
    'w','m','Z','O','0','Q','L','C','J','U','Y','X','z','c','v','u','n','x',
    'r','j','f','t','/','\\','|','(',')','{','}','[',']','?','-','_','+','~',
    '<','>','i','!','l','I',';',':',',','"','^','`','\'', '.', ' ',
];

// Minimum / maximum character cell height in pixels.
//   • 8px lower bound: below this, glyphs smear into indistinct blobs
//   • 16px upper bound: above this, the grid is too coarse for detail
const MIN_CHAR_H_PX: u32 = 8;
const MAX_CHAR_H_PX: u32 = 16;

const LUMINANCE_THRESHOLD: f64 = 12.0; // slightly lower = richer shadow detail

/// A single coloured ASCII character at a grid position.
#[derive(Clone)]
struct ColoredChar {
    ch: char,
    r: u8,
    g: u8,
    b: u8,
}

fn pixel_for(r: u8, g: u8, b: u8) -> char {
    let lum = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
    if lum <= LUMINANCE_THRESHOLD {
        ' '
    } else {
        let scale = (lum - LUMINANCE_THRESHOLD) / (255.0 - LUMINANCE_THRESHOLD);
        // Never index the trailing ' ' (space) via the luminance path
        let idx = ((scale * (ASCII_CHARS.len() - 2) as f64).round() as usize)
            .min(ASCII_CHARS.len() - 2);
        ASCII_CHARS[idx]
    }
}

fn parse_channel(s: &str) -> Result<u8> {
    let s = s.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let f: f64 = pct.trim().parse().context("parsing % channel")?;
        Ok((f / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8)
    } else {
        let f: f64 = s.parse().context("parsing channel")?;
        Ok(f.round().clamp(0.0, 255.0) as u8)
    }
}

fn parse_dump_to_colored(dump_path: &Path) -> Result<(usize, usize, Vec<Vec<ColoredChar>>)> {
    let input = fs::File::open(dump_path).context("opening dump file")?;
    let reader = BufReader::new(input);

    let mut rows: Vec<Vec<ColoredChar>> = Vec::new();
    let mut prev_y: Option<usize> = None;

    for line in reader.lines().skip(1) {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }

        let Some((coord_part, rest)) = line.split_once(':') else { continue };
        let Some((_, y_str)) = coord_part.split_once(',') else { continue };
        let y: usize = match y_str.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let color_token = rest
            .split_whitespace()
            .find(|t| t.starts_with("srgb(") || t.starts_with("srgba("))
            .or_else(|| { let t = rest.trim(); if t.contains('(') { Some(t) } else { None } });

        let Some(token) = color_token else { continue };
        let inner = token
            .trim_start_matches("srgba(")
            .trim_start_matches("srgb(")
            .trim_end_matches(')');

        let mut channels = inner.splitn(4, ',');
        let (r, g, b) = match (channels.next(), channels.next(), channels.next()) {
            (Some(r), Some(g), Some(b)) => {
                match (parse_channel(r), parse_channel(g), parse_channel(b)) {
                    (Ok(r), Ok(g), Ok(b)) => (r, g, b),
                    _ => continue,
                }
            }
            _ => continue,
        };

        let ch = pixel_for(r, g, b);
        if Some(y) != prev_y {
            rows.push(Vec::new());
            prev_y = Some(y);
        }
        rows.last_mut().unwrap().push(ColoredChar { ch, r, g, b });
    }

    let n_rows = rows.len();
    let n_cols = rows.first().map(|r| r.len()).unwrap_or(0);
    Ok((n_rows, n_cols, rows))
}

/// Measure the *actual* pixel advance width and line height of the loaded font
/// at a given pixel scale using ab_glyph's real metrics.
///
/// Why this matters:
///   Monospace fonts are NOT 0.5 × height wide — Liberation Mono is ~0.60,
///   Courier New ~0.55, Consolas ~0.53.  Assuming 0.5 compresses every column,
///   making the rendered image lean and squash horizontally.
///   Querying h_advance directly gives pixel-perfect cell widths.
fn measure_font_cell(font: &FontVec, font_size: f32) -> (u32, u32) {
    let scaled = font.as_scaled(PxScale::from(font_size));
    // 'M' advance = the representative width for a monospace font
    let m_id = scaled.glyph_id('M');
    let char_w = scaled.h_advance(m_id).ceil() as u32;
    // height() = ascent - descent, tightly packs rows with no line gap
    let char_h = scaled.height().ceil() as u32;
    (char_w.max(1), char_h.max(1))
}

/// Choose font_size + grid dimensions so:
///   1. char_h ∈ [MIN_CHAR_H_PX, MAX_CHAR_H_PX] — readable glyphs
///   2. n_cols × char_w ≈ orig_w, n_rows × char_h ≈ orig_h (exact replica)
///   3. char_w is from real font metrics — eliminates distortion/leaning
///
/// Iterates all candidate font sizes and picks the one whose canvas is
/// closest to the original dimensions.
fn compute_grid(font: &FontVec, orig_w: u32, orig_h: u32) -> (u32, u32, u32, u32, f32) {
    let mut best: Option<(u32, u32, u32, u32, f32, u32)> = None;

    for char_h_target in MIN_CHAR_H_PX..=MAX_CHAR_H_PX {
        let font_size = char_h_target as f32;
        let (char_w, char_h) = measure_font_cell(font, font_size);
        if char_w == 0 || char_h == 0 { continue; }

        let n_cols = (orig_w / char_w).max(1);
        let n_rows = (orig_h / char_h).max(1);

        // Total pixel deviation from original size — minimise this
        let err = orig_w.abs_diff(n_cols * char_w) + orig_h.abs_diff(n_rows * char_h);

        if best.is_none() || err < best.unwrap().5 {
            best = Some((n_cols, n_rows, char_w, char_h, font_size, err));
        }
    }

    let (n_cols, n_rows, char_w, char_h, font_size, _) = best.unwrap();
    (n_cols, n_rows, char_w, char_h, font_size)
}

fn render_colored_ascii(
    rows: &[Vec<ColoredChar>],
    font: &FontVec,
    out_path: &Path,
    target_w: u32,
    target_h: u32,
    char_w: u32,
    char_h: u32,
    font_size: f32,
) -> Result<()> {
    let mut img: RgbImage = ImageBuffer::from_pixel(target_w, target_h, Rgb([0u8, 0u8, 0u8]));
    let scale = PxScale::from(font_size);

    for (row_idx, row) in rows.iter().enumerate() {
        let y = row_idx as i32 * char_h as i32;
        for (col_idx, cc) in row.iter().enumerate() {
            if cc.ch == ' ' { continue; }
            let x = col_idx as i32 * char_w as i32;
            draw_text_mut(&mut img, Rgb([cc.r, cc.g, cc.b]), x, y, scale, font, &cc.ch.to_string());
        }
    }

    img.save(out_path).context("saving colored ascii PNG")?;
    Ok(())
}

fn get_frame_delays(path: &Path) -> Result<Vec<u32>> {
    let output = Command::new("magick")
        .args(["identify", "-format", "%T\n", path.to_str().unwrap()])
        .output().context("running magick identify")?;
    if !output.status.success() { anyhow::bail!("magick identify returned an error"); }
    let text = String::from_utf8_lossy(&output.stdout);
    let delays: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    if delays.is_empty() { anyhow::bail!("no frames reported by ImageMagick"); }
    Ok(delays)
}

fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    let out = Command::new("magick")
        .args(["identify", "-ping", "-format", "%w %h\n", path.to_str().unwrap()])
        .output().context("querying image dimensions")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let first_line = s.lines().next().unwrap_or("").trim().to_string();
    let mut iter = first_line.split_whitespace();
    let w: u32 = iter.next().context("missing width")?.parse()?;
    let h: u32 = iter.next().context("missing height")?.parse()?;
    Ok((w, h))
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Convert a GIF into a coloured ASCII-art GIF.")]
struct Cli {
    /// Input GIF file
    input: PathBuf,

    /// Output GIF file name
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Monospace TTF/OTF font path
    #[arg(short, long, default_value = "/usr/share/fonts/liberation/LiberationMono-Regular.ttf")]
    font: PathBuf,

    /// Keep intermediate working files
    #[arg(long)]
    keep: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.input.exists() { anyhow::bail!("input file does not exist: {}", cli.input.display()); }

    let font_data = fs::read(&cli.font)
        .with_context(|| format!("reading font file: {}", cli.font.display()))?;
    let font = FontVec::try_from_vec(font_data).context("parsing font")?;

    // 1. Original dimensions
    let (orig_w, orig_h) = image_dimensions(&cli.input)?;
    eprintln!("Original GIF: {}×{} px", orig_w, orig_h);

    // 2. Compute grid from real font metrics (fixes distortion + size bugs)
    let (n_cols, n_rows, char_w, char_h, font_size) = compute_grid(&font, orig_w, orig_h);
    let canvas_w = n_cols * char_w;
    let canvas_h = n_rows * char_h;
    eprintln!(
        "Grid: {}×{} chars | cell: {}×{}px | font: {}px | canvas: {}×{}px (orig {}×{})",
        n_cols, n_rows, char_w, char_h, font_size, canvas_w, canvas_h, orig_w, orig_h
    );

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let working = std::env::current_dir()?.join(format!("ascii_frames_{ts}"));
    let frame_dir = working.join("frame_images");
    let ascii_png_dir = working.join("ascii_png");
    for d in &[&frame_dir, &ascii_png_dir] { fs::create_dir_all(d)?; }

    // 3. Extract frames
    eprintln!("Extracting frames...");
    let status = Command::new("ffmpeg")
        .args(["-i", cli.input.to_str().unwrap(),
               &format!("{}/frame%04d.png", frame_dir.display())])
        .status().context("running ffmpeg")?;
    if !status.success() { anyhow::bail!("ffmpeg failed"); }

    // 4. Delays
    let delays = get_frame_delays(&cli.input)?;
    let avg_delay = delays.iter().sum::<u32>() / (delays.len() as u32);
    eprintln!("Average delay {}cs (≈{:.2} fps)", avg_delay, 100.0 / avg_delay as f64);

    // 5. Collect PNGs
    let mut pngs: Vec<PathBuf> = fs::read_dir(&frame_dir)?
        .filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    pngs.sort();
    eprintln!("Processing {} frames...", pngs.len());

    // 6. Per frame: resize to n_cols×n_rows → dump → render
    for (i, png) in pngs.iter().enumerate() {
        let stem = png.file_stem().unwrap().to_string_lossy().into_owned();

        let resized = frame_dir.join(format!("{stem}-resized.png"));
        let status = Command::new("magick")
            .args([
                png.to_str().unwrap(),
                "-resize", &format!("{n_cols}x{n_rows}!"),
                "-filter", "Lanczos",
                "-colorspace", "sRGB",
                resized.to_str().unwrap(),
            ]).status()?;
        if !status.success() { anyhow::bail!("magick resize failed on frame {i}"); }

        let dump = frame_dir.join(format!("{stem}.txt"));
        let status = Command::new("magick")
            .args([resized.to_str().unwrap(), dump.to_str().unwrap()])
            .status()?;
        if !status.success() { anyhow::bail!("magick dump failed on frame {i}"); }

        let (_nr, _nc, rows) = parse_dump_to_colored(&dump)
            .with_context(|| format!("parsing dump for frame {i}"))?;
        if rows.is_empty() { anyhow::bail!("frame {i} produced an empty pixel grid"); }

        let png_out = ascii_png_dir.join(format!("{stem}.png"));
        render_colored_ascii(&rows, &font, &png_out, canvas_w, canvas_h, char_w, char_h, font_size)
            .with_context(|| format!("rendering frame {i}"))?;

        fs::remove_file(&resized)?;
        fs::remove_file(&dump)?;

        if (i + 1) % 5 == 0 || i + 1 == pngs.len() {
            eprintln!("  {}/{} frames rendered", i + 1, pngs.len());
        }
    }

    // 7. Assemble GIF
    eprintln!("Assembling output GIF...");
    let mut ascii_images: Vec<PathBuf> = fs::read_dir(&ascii_png_dir)?
        .filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    ascii_images.sort();

    if ascii_images.len() != delays.len() {
        eprintln!("Warning: {} frames, {} delays", ascii_images.len(), delays.len());
    }

    let output = cli.output.unwrap_or_else(|| cli.input.with_file_name("ascii.gif"));

    let mut cmd = Command::new("magick");
    cmd.args(["-loop", "0"]);
    for (i, img) in ascii_images.iter().enumerate() {
        let d = delays.get(i).cloned().unwrap_or(avg_delay);
        cmd.arg("-delay").arg(d.to_string());
        cmd.arg(img.to_str().unwrap());
    }
    cmd.args(["-coalesce", "-resize", &format!("{orig_w}x{orig_h}!"), output.to_str().unwrap()]);

    let status = cmd.status().context("assembling GIF")?;
    if !status.success() { anyhow::bail!("magick GIF assembly failed"); }

    println!("✓ ASCII GIF written to {} ({}×{})", output.display(), orig_w, orig_h);
    if cli.keep { println!("Intermediate files in {}", working.display()); }
    else { fs::remove_dir_all(&working)?; }

    Ok(())
}