use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const FONT_RATIO: f64 = 0.5;
const LUMINANCE_THRESHOLD: f64 = 16.0;
const ASCII_CHARS: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

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

/// Parse a single ImageMagick colour channel value to u8.
/// Handles all three formats ImageMagick may emit:
///   - integer:   "255"
///   - float:     "255.0" or "254.7"
///   - percent:   "100%" or "50.2%"
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

/// Read a text-based ImageMagick dump and convert it into ASCII art lines.
/// Handles srgb(r,g,b) and srgba(r,g,b,a) with integer, float or percent values.
fn convert_dump_to_ascii(dump_path: &Path, out_path: &Path) -> Result<()> {
    let input = fs::File::open(dump_path).context("opening dump file")?;
    let reader = BufReader::new(input);
    let mut writer = fs::File::create(out_path).context("creating ascii output file")?;

    let mut prev_y: Option<u32> = None;

    for line in reader.lines().skip(1) {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Format: "x,y: (r,g,b)\tsrgb(r,g,b)"  or  "x,y: srgb(r,g,b)"
        let Some((coord_part, rest)) = line.split_once(':') else { continue };
        let Some((_, y_str)) = coord_part.split_once(',') else { continue };
        let y: u32 = match y_str.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Find the srgb/srgba(...) token anywhere on the rest of the line.
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
            if prev_y.is_some() {
                writer.write_all(b"\n")?;
            }
            prev_y = Some(y);
        }
        writer.write_all(ch.to_string().as_bytes())?;
    }

    writer.write_all(b"\n")?;
    Ok(())
}

fn get_frame_delays(path: &Path) -> Result<Vec<u32>> {
    let output = Command::new("magick")
        .args(["identify", "-format", "%T\n", path.to_str().unwrap()])
        .output()
        .context("running magick identify to extract frame delays")?;

    if !output.status.success() {
        anyhow::bail!("magick identify returned an error");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let delays: Vec<u32> = text.lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();

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
#[command(author, version, about = "Convert a GIF into an ASCII-art GIF.")]
struct Cli {
    /// Input GIF file to convert
    input: PathBuf,

    /// Output GIF file name (will be overwritten if it exists)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Target number of columns in the ASCII output (controls width)
    #[arg(short, long, default_value_t = 80)]
    columns: u32,

    /// Keep intermediate working files instead of deleting them
    #[arg(long)]
    keep: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.input.exists() {
        anyhow::bail!("input file does not exist: {}", cli.input.display());
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let working = std::env::current_dir()?.join(format!("ascii_frames_{ts}"));
    let frame_dir = working.join("frame_images");
    let ascii_png_dir = working.join("ascii_png");
    for d in &[&frame_dir, &ascii_png_dir] {
        fs::create_dir_all(d).context("creating working directory")?;
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
    if !status.success() {
        anyhow::bail!("ffmpeg failed");
    }

    // 2. Per-frame delays
    let delays = get_frame_delays(&cli.input)?;
    let avg_delay = delays.iter().sum::<u32>() / (delays.len() as u32);
    let fps = if avg_delay > 0 { 100.0 / avg_delay as f64 } else { 0.0 };
    eprintln!("Original average delay {}cs (≈{:.2} fps)", avg_delay, fps);

    // 3. Collect PNGs
    let mut pngs: Vec<PathBuf> = fs::read_dir(&frame_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    pngs.sort();
    eprintln!("Processing {} frames...", pngs.len());

    // 4. Resize -> dump -> ASCII txt
    for (i, png) in pngs.iter().enumerate() {
        let (orig_w, orig_h) = image_dimensions(png)?;
        let new_w = cli.columns;
        let new_h = ((orig_h as f64 / orig_w as f64) * new_w as f64 * FONT_RATIO)
            .round()
            .max(1.0) as u32;

        let stem = png.file_stem().unwrap().to_string_lossy().into_owned();

        let resized = frame_dir.join(format!("{stem}-resized.png"));
        let status = Command::new("magick")
            .args([
                png.to_str().unwrap(),
                "-resize", &format!("{new_w}x{new_h}!"),
                resized.to_str().unwrap(),
            ])
            .status()
            .context("resizing frame")?;
        if !status.success() { anyhow::bail!("magick resize failed on frame {i}"); }

        let dump = frame_dir.join(format!("{stem}.txt"));
        let status = Command::new("magick")
            .args([resized.to_str().unwrap(), dump.to_str().unwrap()])
            .status()
            .context("creating text dump")?;
        if !status.success() { anyhow::bail!("magick dump failed on frame {i}"); }

        // DEBUG: print the first few lines of the dump so you can verify the format
        if i == 0 {
            let f = fs::File::open(&dump)?;
            let r = BufReader::new(f);
            eprintln!("=== Dump format sample (frame 0) ===");
            for l in r.lines().take(5) {
                eprintln!("  {:?}", l?);
            }
            eprintln!("=====================================");
        }

        let ascii_txt = frame_dir.join(format!("{stem}.ascii.txt"));
        convert_dump_to_ascii(&dump, &ascii_txt)
            .with_context(|| format!("converting dump to ASCII for frame {i}"))?;

        fs::remove_file(&resized)?;
        fs::remove_file(&dump)?;

        if (i + 1) % 5 == 0 || i + 1 == pngs.len() {
            eprintln!("  {}/{} frames converted", i + 1, pngs.len());
        }
    }

    // 5. Render ASCII txt -> PNG
    eprintln!("Rendering ASCII frames to PNG...");
    let mut ascii_txts: Vec<PathBuf> = fs::read_dir(&frame_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_str().map(|s| s.ends_with(".ascii.txt")).unwrap_or(false))
        .collect();
    ascii_txts.sort();

    for (i, txt) in ascii_txts.iter().enumerate() {
        let base = txt
            .file_name().unwrap()
            .to_string_lossy()
            .replace(".ascii.txt", "");
        let png_out = ascii_png_dir.join(format!("{base}.png"));

        let status = Command::new("magick")
            .args([
                "-background", "white",
                "-fill",       "black",
                "-font",       "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
                "-pointsize",  "12",
                &format!("label:@{}", txt.display()),
                png_out.to_str().unwrap(),
            ])
            .status()
            .context("rendering ASCII text to PNG")?;
        if !status.success() { anyhow::bail!("magick label render failed on frame {i}"); }
    }

    // 6. Assemble GIF
    eprintln!("Assembling output GIF...");
    let mut ascii_images: Vec<PathBuf> = fs::read_dir(&ascii_png_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    ascii_images.sort();

    if ascii_images.len() != delays.len() {
        eprintln!(
            "Warning: {} frames but {} delays; extras use average delay",
            ascii_images.len(), delays.len()
        );
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

    let status = cmd.status().context("assembling output GIF")?;
    if !status.success() { anyhow::bail!("magick GIF assembly failed"); }

    println!("ASCII GIF written to {}", output.display());

    // 7. Cleanup
    if cli.keep {
        println!("Intermediate files kept in {}", working.display());
    } else {
        fs::remove_dir_all(&working).context("removing working directory")?;
    }

    Ok(())
}