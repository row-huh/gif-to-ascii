
# Gif 2 Ascii
Two separate tools for converting GIFs to ASCII art:

1. **Web converter** : converter built in typescript that runs locally on the browser, has higher customization with parameters (Lumonisity, Contrast, Intensity, Detail)
2. **CLI** : Rust-based, work in progress ([jump to section](#cli-work-in-progress))

## Web Converter
View [Live](https://gif-to-ascii.vercel.app/)
  
![Demo](readme-assets/demo.gif)

▶ Full demo: https://youtu.be/y4yYm5CCs1A
### Parameters

Tinker around and find out the best settings for your gifs
| Parameter | Effect |
|-----------|--------|
| **Luminosity** | enhance relative black and white information |
| **Detail** | reduces font size and packs in more characters to fill space |
| **Contrast** | enhances or reduces color saturation |
| **Intensity** | increasing it adds more white information |

---

## CLI *(work in progress)*

Both tools are developed independently.

### Goals for cli

- `gif-to-ascii /path/to/gif` — outputs an ASCII gif in the working directory
