use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

// constants taken from the shell-script description
const FONT_RATIO: f64 = 0.5; // vertical scaling applied when resizing images
const LUMINANCE_THRESHOLD: f64 = 16.0; // dark cutoff (0..=255)
const ASCII_CHARS: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// Small utility to map an (r,g,b) triple to an ascii character.
fn pixel_for(r: u8, g: u8, b: u8) -> char {
    // compute relative luminance
    let r = r as f64;
    let g = g as f64;
    let b = b as f64;
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    if lum <= LUMINANCE_THRESHOLD {
        ' '
    } else {
        let scale = (lum - LUMINANCE_THRESHOLD) / (255.0 - LUMINANCE_THRESHOLD);
        let idx = (scale * ((ASCII_CHARS.len() - 1) as f64)).round() as usize;
        ASCII_CHARS[idx]
    }
}

/// Read a text-based ImageMagick dump of the form "x,y: srgb(r,g,b)" and
/// convert it into a file containing lines of ascii characters.
fn convert_dump_to_ascii(dump_path: &Path, out_path: &Path) -> Result<()> {
    let input = fs::File::open(dump_path).context("opening dump file")?;
    let reader = BufReader::new(input);
    let mut writer = fs::File::create(out_path).context("creating ascii output file")?;

    let mut prev_y: Option<u32> = None;

    // skip header line(s); ImageMagick dumps typically begin with a header
    // like "# ImageMagick pixel enumeration: 80,60,255,srgb".
    for line in reader.lines().skip(1) {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // parse "x,y: srgb(r,g,b)"
        if let Some((coord_part, color_part)) = line.split_once(':') {
            if let Some((_, y_str)) = coord_part.split_once(',') {
                let y: u32 = y_str.trim().parse()?;
                let rgb_vals: Vec<u8> = color_part
                    .trim()
                    .trim_start_matches("srgb(")
                    .trim_end_matches(')')
                    .split(',')
                    .map(|s| s.parse::<u8>())
                    .collect::<std::result::Result<_, _>>()?;
                let ch = pixel_for(rgb_vals[0], rgb_vals[1], rgb_vals[2]);

                if Some(y) != prev_y {
                    if prev_y.is_some() {
                        writer.write_all(b"\n")?;
                    }
                    prev_y = Some(y);
                }
                writer.write_all(ch.to_string().as_bytes())?;
            }
        }
    }
    Ok(())
}

/// Ask ImageMagick what the delay for each frame in the source GIF is and
/// return all of the values (measured in hundredths of a second).  The
/// caller can average them or use them individually when recreating the
/// output GIF.  The original shell script just grabbed an average with
/// `identify -format "%T"` and we used that for simplicity; here we keep
/// the per-frame delays so that the converted GIF can match the exact
/// timing of the input (e.g. 23 fps would become delay≈4).
fn get_frame_delays(path: &Path) -> Result<Vec<u32>> {
    let output = Command::new("magick")
        .args(&["identify", "-format", "%T\n", path.to_str().unwrap()])
        .output()
        .context("running magick identify to extract frame delays")?;

    if !output.status.success() {
        anyhow::bail!("magick identify returned an error");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut delays = Vec::new();
    for line in text.lines() {
        if let Ok(val) = line.trim().parse::<u32>() {
            delays.push(val);
        }
    }
    if delays.is_empty() {
        anyhow::bail!("no frames reported by ImageMagick");
    }
    Ok(delays)
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Convert a GIF into an ASCII-art GIF using external tools and Rust for orchestration.", long_about = None)]
struct Cli {
    /// Input GIF file to convert
    input: PathBuf,

    /// Output GIF file name (will be overwritten if it exists)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Target number of columns in the ASCII output (controls width)
    #[arg(short, long, default_value_t = 80)]
    columns: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.input.exists() {
        anyhow::bail!("input file does not exist");
    }

    let working = std::env::current_dir()?
        .join(format!("ascii_frames_{}", chrono::Local::now().format("%Y%m%d%H%M%S")));
    fs::create_dir_all(&working).context("creating working directory")?;

    let frame_dir = working.join("frame_images");
    fs::create_dir_all(&frame_dir).context("creating frame output directory")?;

    // extract frames with ffmpeg; we don't need to specify a framerate
    // because ffmpeg will split into exactly one image per input frame.
    let status = Command::new("ffmpeg")
        .args(&["-i", cli.input.to_str().unwrap(), &format!("{}/frame%04d.png", frame_dir.display())])
        .status()
        .context("running ffmpeg to extract frames")?;
    if !status.success() {
        anyhow::bail!("ffmpeg failed");
    }

    // grab per-frame delays so we can reassemble with the same timing
    let delays = get_frame_delays(&cli.input)?;
    let avg_delay = delays.iter().sum::<u32>() / (delays.len() as u32);
    let fps = if avg_delay > 0 {
        100.0 / (avg_delay as f64)
    } else {
        0.0
    };
    eprintln!("original average delay {} (≈{:.2} fps)", avg_delay, fps);

    // process each extracted PNG
    let mut pngs: Vec<PathBuf> = fs::read_dir(&frame_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    pngs.sort();

    for png in &pngs {
        let height_str = Command::new("magick")
            .args(&["identify", "-ping", "-format", "%h", png.to_str().unwrap()])
            .output()
            .context("querying image height")?;
        let height: u32 = String::from_utf8_lossy(&height_str.stdout).trim().parse()?;
        let new_height = ((height as f64) * FONT_RATIO).round() as u32;

        let resized = png.with_file_name(format!("{}-resized.png", png.file_stem().unwrap().to_string_lossy()));
        let status = Command::new("magick")
            .args(&[png.to_str().unwrap(), "-resize", &format!("x{}!", new_height), resized.to_str().unwrap()])
            .status()
            .context("resizing frame")?;
        if !status.success() {
            anyhow::bail!("magick resize failed");
        }

        let dump = resized.with_extension("txt");
        let status = Command::new("magick")
            .args(&[resized.to_str().unwrap(), dump.to_str().unwrap()])
            .status()
            .context("creating text dump")?;
        if !status.success() {
            anyhow::bail!("magick convert to dump failed");
        }

        let ascii = png.with_extension("ascii.txt");
        convert_dump_to_ascii(&dump, &ascii)?;

        // clean up intermediate files
        fs::remove_file(&resized)?;
        fs::remove_file(&dump)?;
    }

    // now render ascii text files to pngs so we can build a gif
    let ascii_png_dir = working.join("ascii_png");
    fs::create_dir_all(&ascii_png_dir)?;
    for txt in fs::read_dir(&frame_dir)? {
        let txt = txt?.path();
        if txt.extension().and_then(|s| s.to_str()) != Some("txt") {
            continue;
        }
        let pngname = ascii_png_dir.join(txt.file_stem().unwrap()).with_extension("png");
        let status = Command::new("magick")
            .args(&["-background", "white", "-fill", "black", "-font", "Courier", "-pointsize", "12", &format!("label:@{}", txt.display()), pngname.to_str().unwrap()])
            .status()
            .context("rendering ascii text to png")?;
        if !status.success() {
            anyhow::bail!("magick label render failed");
        }
    }

    // finally, assemble gif from ascii_pngs.  We pair each image with its
    // corresponding delay so the output matches the original timing.
    let output = cli
        .output
        .unwrap_or_else(|| cli.input.with_file_name("ascii.gif"));

    // collect ascii images in sorted order so they line up with `delays`
    let mut ascii_images: Vec<PathBuf> = fs::read_dir(&ascii_png_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    ascii_images.sort();

    if ascii_images.len() != delays.len() {
        eprintln!("warning: frame count and delay count differ ({} vs {})",
                  ascii_images.len(), delays.len());
    }

    let mut cmd = Command::new("magick");
    for (i, img) in ascii_images.iter().enumerate() {
        let d = delays.get(i).cloned().unwrap_or(avg_delay);
        cmd.arg("-delay").arg(d.to_string());
        cmd.arg(img.to_str().unwrap());
    }
    cmd.arg(output.to_str().unwrap());
    let status = cmd.status().context("assembling output gif")?;
    if !status.success() {
        anyhow::bail!("magick gif assembly failed");
    }

    println!("ASCII gif written to {}", output.display());
    println!("intermediate files left in {} (remove when you are done)", working.display());

    Ok(())
}
