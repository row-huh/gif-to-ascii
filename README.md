<div align="center">

| Original | ASCII |
|----------|-------|
| ![orignal](readme-assets/itachi.gif__) | ![ascii](readme-assets/itachi-ascii.gif__) |

</div>

# gif-to-ascii

Two separate tools for converting GIFs to ASCII art:

- **Web converter** — runs locally in the browser, TypeScript, with full parameter control
- **CLI** — Rust-based, work in progress ([jump to section](#cli-a-work-in-progress))

---

## Web Converter

> **Live:** [add-url]  
> **Demo:** ![readme-assets/demo.mp4](readme-assets/demo.mp4)

### Parameters

Tweak these to dial in the look you want:

| Parameter | Effect |
|-----------|--------|
| **Luminosity** | — |
| **Detail** | reduces font size and packs in more characters to fill space |
| **Contrast** | enhances or reduces color saturation |
| **Intensity** | increasing it adds more white information |

---

## CLI *(work in progress)*

Both tools are developed independently.

### Goals

- `gif-to-ascii /path/to/gif` — outputs an ASCII gif in the working directory
- eventually: a command to set a gif in [fastfetch](https://github.com/fastfetch-cli/fastfetch) *(this was the original motivation — the converter came first)*