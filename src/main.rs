use anyhow::{Context, Result};
use clap::Parser;
use image::{ImageBuffer, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use ab_glyph::{FontVec, PxScale};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

const FONT_RATIO: f64 = 0.5;
const LUMINANCE_THRESHOLD: f64 = 16.0;
const ASCII_CHARS: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

// Font metrics for rendering — tweak CHAR_W/CHAR_H if your font differs
const FONT_SIZE: f32 = 12.0;
const CHAR_W: u32 = 7;  // pixels per character cell (width)
const CHAR_H: u32 = 14; // pixels per character cell (height)

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
        let idx = (scale * ((ASCII_CHARS.len() - 1) as f64)).round() as usize;
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

/// Parse an ImageMagick pixel dump into a 2D grid of coloured ASCII chars.
/// Returns (rows, cols, pixels) where pixels is row-major.
fn parse_dump_to_colored(dump_path: &Path) -> Result<(usize, usize, Vec<Vec<ColoredChar>>)> {
    let input = fs::File::open(dump_path).context("opening dump file")?;
    let reader = BufReader::new(input);

    // rows[y] = vec of ColoredChar in x order
    let mut rows: Vec<Vec<ColoredChar>> = Vec::new();
    let mut prev_y: Option<usize> = None;

    for line in reader.lines().skip(1) {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((coord_part, rest)) = line.split_once(':') else { continue };
        let Some((_, y_str)) = coord_part.split_once(',') else { continue };
        let y: usize = match y_str.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let color_token = rest
            .split_whitespace()
            .find(|t| t.starts_with("srgb(") || t.starts_with("srgba("))
            .or_else(|| {
                let t = rest.trim();
                if t.contains('(') { Some(t) } else { None }
            });

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

/// Render a grid of coloured ASCII chars to a PNG image.
fn render_colored_ascii(
    rows: &[Vec<ColoredChar>],
    n_cols: usize,
    font: &FontVec,
    out_path: &Path,
) -> Result<()> {
    let img_w = n_cols as u32 * CHAR_W;
    let img_h = rows.len() as u32 * CHAR_H;
    let mut img: RgbImage = ImageBuffer::from_pixel(img_w, img_h, Rgb([0u8, 0u8, 0u8]));

    let scale = PxScale::from(FONT_SIZE);

    for (row_idx, row) in rows.iter().enumerate() {
        let y = row_idx as i32 * CHAR_H as i32;
        for (col_idx, cc) in row.iter().enumerate() {
            if cc.ch == ' ' {
                continue;
            }
            let x = col_idx as i32 * CHAR_W as i32;
            draw_text_mut(
                &mut img,
                Rgb([cc.r, cc.g, cc.b]),
                x,
                y,
                scale,
                font,
                &cc.ch.to_string(),
            );
        }
    }

    img.save(out_path).context("saving colored ascii PNG")?;
    Ok(())
}

fn get_frame_delays(path: &Path) -> Result<Vec<u32>> {
    let output = Command::new("magick")
        .args(["identify", "-format", "%T\n", path.to_str().unwrap()])
        .output()
        .context("running magick identify")?;
    if !output.status.success() {
        anyhow::bail!("magick identify returned an error");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let delays: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    if delays.is_empty() {
        anyhow::bail!("no frames reported by ImageMagick");
    }
    Ok(delays)
}

fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    let out = Command::new("magick")
        .args(["identify", "-ping", "-format", "%w %h", path.to_str().unwrap()])
        .output()
        .context("querying image dimensions")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut iter = s.trim().split_whitespace();
    let w: u32 = iter.next().context("missing width")?.parse()?;
    let h: u32 = iter.next().context("missing height")?.parse()?;
    Ok((w, h))
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Convert a GIF into a coloured ASCII-art GIF.")]
struct Cli {
    /// Input GIF file to convert
    input: PathBuf,

    /// Output GIF file name
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Target number of columns in the ASCII output
    #[arg(short, long, default_value_t = 80)]
    columns: u32,

    /// Path to a monospace TTF font file
    #[arg(short, long, default_value = "/usr/share/fonts/liberation/LiberationMono-Regular.ttf")]
    font: PathBuf,

    /// Keep intermediate working files
    #[arg(long)]
    keep: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.input.exists() {
        anyhow::bail!("input file does not exist: {}", cli.input.display());
    }

    // Load font once upfront
    let font_data = fs::read(&cli.font)
        .with_context(|| format!("reading font file: {}", cli.font.display()))?;
    let font = FontVec::try_from_vec(font_data)
        .context("parsing font — make sure it's a valid TTF")?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let working = std::env::current_dir()?.join(format!("ascii_frames_{ts}"));
    let frame_dir = working.join("frame_images");
    let ascii_png_dir = working.join("ascii_png");
    for d in &[&frame_dir, &ascii_png_dir] {
        fs::create_dir_all(d)?;
    }

    // 1. Extract frames
    eprintln!("Extracting frames...");
    let status = Command::new("ffmpeg")
        .args([
            "-i", cli.input.to_str().unwrap(),
            &format!("{}/frame%04d.png", frame_dir.display()),
        ])
        .status()
        .context("running ffmpeg")?;
    if !status.success() { anyhow::bail!("ffmpeg failed"); }

    // 2. Per-frame delays
    let delays = get_frame_delays(&cli.input)?;
    let avg_delay = delays.iter().sum::<u32>() / (delays.len() as u32);
    eprintln!("Average delay {}cs (≈{:.2} fps)", avg_delay, 100.0 / avg_delay as f64);

    // 3. Collect PNGs
    let mut pngs: Vec<PathBuf> = fs::read_dir(&frame_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    pngs.sort();
    eprintln!("Processing {} frames...", pngs.len());

    // 4. Per frame: resize → dump → colored PNG (all in one pass, no txt file needed)
    for (i, png) in pngs.iter().enumerate() {
        let (orig_w, orig_h) = image_dimensions(png)?;
        let new_w = cli.columns;
        let new_h = ((orig_h as f64 / orig_w as f64) * new_w as f64 * FONT_RATIO)
            .round()
            .max(1.0) as u32;

        let stem = png.file_stem().unwrap().to_string_lossy().into_owned();

        // Resize
        let resized = frame_dir.join(format!("{stem}-resized.png"));
        let status = Command::new("magick")
            .args([
                png.to_str().unwrap(),
                "-resize", &format!("{new_w}x{new_h}!"),
                resized.to_str().unwrap(),
            ])
            .status()?;
        if !status.success() { anyhow::bail!("magick resize failed on frame {i}"); }

        // Dump pixels to text
        let dump = frame_dir.join(format!("{stem}.txt"));
        let status = Command::new("magick")
            .args([resized.to_str().unwrap(), dump.to_str().unwrap()])
            .status()?;
        if !status.success() { anyhow::bail!("magick dump failed on frame {i}"); }

        // Parse dump → colored chars → render directly to PNG
        let (n_rows, n_cols, rows) = parse_dump_to_colored(&dump)
            .with_context(|| format!("parsing dump for frame {i}"))?;

        if n_rows == 0 || n_cols == 0 {
            anyhow::bail!("frame {i} produced an empty pixel grid");
        }

        let png_out = ascii_png_dir.join(format!("{stem}.png"));
        render_colored_ascii(&rows, n_cols, &font, &png_out)
            .with_context(|| format!("rendering frame {i}"))?;

        fs::remove_file(&resized)?;
        fs::remove_file(&dump)?;

        if (i + 1) % 5 == 0 || i + 1 == pngs.len() {
            eprintln!("  {}/{} frames rendered", i + 1, pngs.len());
        }
    }

    // 5. Assemble GIF
    eprintln!("Assembling output GIF...");
    let mut ascii_images: Vec<PathBuf> = fs::read_dir(&ascii_png_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    ascii_images.sort();

    if ascii_images.len() != delays.len() {
        eprintln!("Warning: {} frames, {} delays", ascii_images.len(), delays.len());
    }

    let output = cli.output
        .unwrap_or_else(|| cli.input.with_file_name("ascii.gif"));

    let mut cmd = Command::new("magick");
    cmd.args(["-loop", "0"]);
    for (i, img) in ascii_images.iter().enumerate() {
        let d = delays.get(i).cloned().unwrap_or(avg_delay);
        cmd.arg("-delay").arg(d.to_string());
        cmd.arg(img.to_str().unwrap());
    }
    cmd.arg(output.to_str().unwrap());

    let status = cmd.status().context("assembling GIF")?;
    if !status.success() { anyhow::bail!("magick GIF assembly failed"); }

    println!("✓ ASCII GIF written to {}", output.display());

    if cli.keep {
        println!("Intermediate files in {}", working.display());
    } else {
        fs::remove_dir_all(&working)?;
    }

    Ok(())
}