import { useState, useRef, useCallback, useEffect, useMemo, Fragment } from "react";
import { Upload, X, Play, Pause, Palette, Sun, Contrast, Download, RotateCcw, FileText, FileImage, Github, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Progress } from "@/components/ui/progress";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import JSZip from "jszip";
import { GIFEncoder, quantize, applyPalette } from "gifenc";

// Dense to sparse - index 0 = darkest, last = brightest
const ASCII_CHARS = "@%#*+=-:. ";

interface PixelData {
  r: number;
  g: number;
  b: number;
}

type ColorMode = "original" | "mono";

function brightness(r: number, g: number, b: number) {
  return 0.299 * r + 0.587 * g + 0.114 * b;
}

const MAX_EXTRACT_COLS = 250;

/** Subsample a high-res pixel grid to a target column count */
function subsampleFrame(frame: PixelData[][], targetCols: number): PixelData[][] {
  const srcCols = frame[0]?.length || 0;
  const srcRows = frame.length;
  if (targetCols >= srcCols) return frame;
  const scale = srcCols / targetCols;
  const targetRows = Math.floor(srcRows / scale);
  const result: PixelData[][] = [];
  for (let r = 0; r < targetRows; r++) {
    const row: PixelData[] = [];
    const srcR = Math.floor(r * scale);
    for (let c = 0; c < targetCols; c++) {
      const srcC = Math.floor(c * scale);
      row.push(frame[srcR][srcC]);
    }
    result.push(row);
  }
  return result;
}

/** Compute ASCII frame from raw pixels with adaptive contrast + luminosity + contrast adjustment */
function computeAsciiFrame(
  frame: PixelData[][],
  luminosity: number,
  contrast: number,
  minBright: number,
  maxBright: number,
  targetCols: number,
  colorBoost: number // -100 to 100
) {
  const sampled = subsampleFrame(frame, targetCols);
  const range = maxBright - minBright || 1;
  const midpoint = (minBright + maxBright) / 2;
  const contrastFactor = Math.pow((contrast + 100) / 100, 2);
  const boostFactor = 1 + colorBoost / 50; // 0 to 3

  return sampled.map((row) =>
    row.map((px) => {
      const raw = brightness(px.r, px.g, px.b) + (luminosity * 2.55);
      const contrasted = midpoint + (raw - midpoint) * contrastFactor;
      const normalized = Math.max(0, Math.min(1, (contrasted - minBright) / range));
      const charIdx = Math.floor(normalized * (ASCII_CHARS.length - 1));

      const applyAdj = (v: number) => {
        const l = v + luminosity * 2.55;
        const c = 128 + (l - 128) * contrastFactor;
        // Boost: intensify distance from gray
        const gray = brightness(px.r, px.g, px.b);
        const boosted = gray + (c - gray) * boostFactor;
        return Math.max(0, Math.min(255, boosted));
      };

      return {
        char: ASCII_CHARS[charIdx],
        r: applyAdj(px.r),
        g: applyAdj(px.g),
        b: applyAdj(px.b),
      };
    })
  );
}

function extractFramePixels(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  cols: number
): PixelData[][] {
  const aspectRatio = 0.5;
  const cellW = width / cols;
  const cellH = cellW / aspectRatio;
  const rows = Math.floor(height / cellH);
  const { data } = ctx.getImageData(0, 0, width, height);

  const result: PixelData[][] = [];
  for (let row = 0; row < rows; row++) {
    const line: PixelData[] = [];
    for (let col = 0; col < cols; col++) {
      const x = Math.floor(col * cellW);
      const y = Math.floor(row * cellH);
      const idx = (y * width + x) * 4;
      line.push({ r: data[idx], g: data[idx + 1], b: data[idx + 2] });
    }
    result.push(line);
  }
  return result;
}

/** Analyze brightness range across all frames */
function analyzeBrightness(frames: PixelData[][][]): { min: number; max: number } {
  let min = 255, max = 0;
  // Sample every 3rd frame, every 4th pixel for speed
  for (let f = 0; f < frames.length; f += Math.max(1, Math.floor(frames.length / 10))) {
    const frame = frames[f];
    for (let r = 0; r < frame.length; r += 2) {
      for (let c = 0; c < frame[r].length; c += 4) {
        const b = brightness(frame[r][c].r, frame[r][c].g, frame[r][c].b);
        if (b < min) min = b;
        if (b > max) max = b;
      }
    }
  }
  return { min, max };
}

async function extractGifFramesFallback(
  file: File
): Promise<{ frames: PixelData[][][]; delays: number[] }> {
  return new Promise((resolve) => {
    const img = new Image();
    const url = URL.createObjectURL(file);
    img.onload = () => {
      const canvas = document.createElement("canvas");
      const maxWidth = MAX_EXTRACT_COLS;
      const scale = Math.min(1, maxWidth / img.width);
      canvas.width = Math.floor(img.width * scale);
      canvas.height = Math.floor(img.height * scale);
      const ctx = canvas.getContext("2d")!;
      ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
      const pixels = extractFramePixels(ctx, canvas.width, canvas.height, Math.min(MAX_EXTRACT_COLS, canvas.width));
      URL.revokeObjectURL(url);
      resolve({ frames: [pixels], delays: [100] });
    };
    img.src = url;
  });
}

async function extractAllGifFrames(
  file: File
): Promise<{ frames: PixelData[][][]; delays: number[] }> {
  const arrayBuffer = await file.arrayBuffer();

  if ("ImageDecoder" in window) {
    try {
      const decoder = new (window as any).ImageDecoder({
        type: "image/gif",
        data: arrayBuffer,
      });
      await decoder.tracks.ready;
      const trackCount = decoder.tracks.selectedTrack?.frameCount || 1;
      const frames: PixelData[][][] = [];
      const delays: number[] = [];
      const canvas = document.createElement("canvas");
      let ctx: CanvasRenderingContext2D | null = null;
      const maxCols = MAX_EXTRACT_COLS;

      for (let i = 0; i < Math.min(trackCount, 60); i++) {
        const result = await decoder.decode({ frameIndex: i });
        const frame = result.image;
        if (!ctx) {
          const scale = Math.min(1, 1200 / frame.displayWidth);
          canvas.width = Math.floor(frame.displayWidth * scale);
          canvas.height = Math.floor(frame.displayHeight * scale);
          ctx = canvas.getContext("2d")!;
        }
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frames.push(extractFramePixels(ctx, canvas.width, canvas.height, maxCols));
        delays.push(Math.max(frame.duration / 1000, 50));
        frame.close();
      }
      decoder.close();
      return { frames, delays };
    } catch {
      return extractGifFramesFallback(file);
    }
  }
  return extractGifFramesFallback(file);
}

interface AsciiRenderChar {
  char: string;
  r: number;
  g: number;
  b: number;
}

function AsciiCanvas({
  frame,
  colorMode,
  monoColor,
  fontSize,
}: {
  frame: AsciiRenderChar[][];
  colorMode: ColorMode;
  monoColor: string;
  fontSize: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rows = frame.length;
  const cols = frame[0]?.length || 0;
  const charW = fontSize * 0.6;
  const charH = fontSize;
  const width = Math.ceil(cols * charW);
  const height = rows * charH;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d")!;
    ctx.fillStyle = "#0a0f0a";
    ctx.fillRect(0, 0, width, height);
    ctx.font = `${charH}px monospace`;
    ctx.textBaseline = "top";

    if (colorMode === "mono") {
      ctx.fillStyle = monoColor;
      for (let r = 0; r < rows; r++) {
        let line = "";
        for (let c = 0; c < cols; c++) line += frame[r][c].char;
        ctx.fillText(line, 0, r * charH);
      }
    } else {
      for (let r = 0; r < rows; r++) {
        for (let c = 0; c < cols; c++) {
          const ch = frame[r][c];
          ctx.fillStyle = `rgb(${ch.r | 0},${ch.g | 0},${ch.b | 0})`;
          ctx.fillText(ch.char, c * charW, r * charH);
        }
      }
    }
  }, [frame, colorMode, monoColor, width, height, rows, cols, charW, charH]);

  return (
    <div className="overflow-auto rounded border border-border bg-card p-2">
      <canvas
        ref={canvasRef}
        style={{ width, height, imageRendering: "pixelated" }}
      />
    </div>
  );
}

/** Sample previews for the homepage showcase */
const SAMPLE_PREVIEWS = [
  {
    label: "Anime Close-up",
    gif: "/samples/sample1.gif",
    ascii: "/samples/sample1-ascii.gif",
  },
  {
    label: "Field of Flowers",
    gif: "/samples/sample2.gif",
    ascii: "/samples/sample2-ascii.gif",
  },
  {
    label: "Water Surface",
    gif: "/samples/sample3.gif",
    ascii: "/samples/sample3-ascii.gif",
  },
];

const ASCII_TITLE = `
  ██████╗ ██╗███████╗    ██████╗      █████╗ ███████╗ ██████╗██╗██╗
 ██╔════╝ ██║██╔════╝    ╚════██╗    ██╔══██╗██╔════╝██╔════╝██║██║
 ██║  ███╗██║█████╗       █████╔╝    ███████║███████╗██║     ██║██║
 ██║   ██║██║██╔══╝      ██╔═══╝     ██╔══██║╚════██║██║     ██║██║
 ╚██████╔╝██║██║         ███████╗    ██║  ██║███████║╚██████╗██║██║
  ╚═════╝ ╚═╝╚═╝         ╚══════╝    ╚═╝  ╚═╝╚══════╝ ╚═════╝╚═╝╚═╝
`.trimEnd();

function PreviewShowcase() {
  const [activeIdx, setActiveIdx] = useState(0);
  const [ready, setReady] = useState(false);

  // Preload all sample GIFs on mount
  useEffect(() => {
    const urls = SAMPLE_PREVIEWS.flatMap((s) => [s.gif, s.ascii]);
    let loaded = 0;
    urls.forEach((url) => {
      const img = new Image();
      img.onload = img.onerror = () => {
        loaded++;
        if (loaded >= urls.length) setReady(true);
      };
      img.src = url;
    });
  }, []);

  useEffect(() => {
    if (!ready) return;
    const timer = setInterval(() => {
      setActiveIdx((i) => (i + 1) % SAMPLE_PREVIEWS.length);
    }, 3000);
    return () => clearInterval(timer);
  }, [ready]);

  const sample = SAMPLE_PREVIEWS[activeIdx];

  return (
    <div className="w-full max-w-5xl">
      <div className="grid grid-cols-2 gap-1">
        {/* Render ALL samples, only show active — keeps them cached in DOM */}
        {SAMPLE_PREVIEWS.map((s, i) => (
          <Fragment key={s.label}>
            <div className={`relative overflow-hidden rounded-l-lg border border-border bg-card ${i !== activeIdx ? "hidden" : ""}`}>
              <img src={s.gif} alt="Original" className="h-64 w-full object-cover" />
              <div className="absolute bottom-2 left-2 rounded bg-background/80 px-2 py-0.5 text-[10px] uppercase tracking-widest text-muted-foreground backdrop-blur-sm">
                original
              </div>
            </div>
            <div className={`relative overflow-hidden rounded-r-lg border border-border bg-card ${i !== activeIdx ? "hidden" : ""}`}>
              <img src={s.ascii} alt="ASCII" className="h-64 w-full object-cover" />
              <div className="absolute bottom-2 left-2 rounded bg-background/80 px-2 py-0.5 text-[10px] uppercase tracking-widest text-muted-foreground backdrop-blur-sm">
                ascii
              </div>
            </div>
          </Fragment>
        ))}
      </div>
    </div>
  );
}


export default function GifToAscii() {
  const [rawFrames, setRawFrames] = useState<PixelData[][][]>([]);
  const [currentFrame, setCurrentFrame] = useState(0);
  const [delays, setDelays] = useState<number[]>([]);
  const [isPlaying, setIsPlaying] = useState(true);
  const [loading, setLoading] = useState(false);
  const [fileName, setFileName] = useState("");
  const [gifUrl, setGifUrl] = useState("");
  const [exporting, setExporting] = useState(false);
  const [exportProgress, setExportProgress] = useState(0);
  const [exportLabel, setExportLabel] = useState("");
  const [colorMode, setColorMode] = useState<ColorMode>("original");
  const [monoColor, setMonoColor] = useState("#22c55e");
  const [luminosity, setLuminosity] = useState(0);
  const [contrast, setContrast] = useState(0);
  const [fontSize, setFontSize] = useState(10);
  const [colorBoost, setColorBoost] = useState(0);
  const [brightnessRange, setBrightnessRange] = useState({ min: 0, max: 255 });
  const targetCols = Math.round(600 / (fontSize * 0.6));
  const fileRef = useRef<HTMLInputElement>(null);
  const timerRef = useRef<number>();
  const [dragOver, setDragOver] = useState(false);

  const processFile = useCallback(async (file: File) => {
    if (!file.type.includes("gif") && !file.type.includes("image")) return;
    setLoading(true);
    setFileName(file.name);
    setGifUrl(URL.createObjectURL(file));
    try {
      const { frames, delays } = await extractAllGifFrames(file);
      setRawFrames(frames);
      setDelays(delays);
      setCurrentFrame(0);
      setIsPlaying(frames.length > 1);
      setColorMode("original");
      setLuminosity(0);
      setContrast(0);
      const range = analyzeBrightness(frames);
      setBrightnessRange(range);
    } finally {
      setLoading(false);
    }
  }, []);

  // Compute the current ASCII frame on the fly with luminosity
  const currentAsciiFrame = useMemo(() => {
    if (rawFrames.length === 0) return null;
    return computeAsciiFrame(
      rawFrames[currentFrame],
      luminosity,
      contrast,
      brightnessRange.min,
      brightnessRange.max,
      targetCols,
      colorBoost
    );
  }, [rawFrames, currentFrame, luminosity, contrast, brightnessRange, targetCols, colorBoost]);

  useEffect(() => {
    if (!isPlaying || rawFrames.length <= 1) return;
    timerRef.current = window.setTimeout(() => {
      setCurrentFrame((f) => (f + 1) % rawFrames.length);
    }, delays[currentFrame] || 100);
    return () => clearTimeout(timerRef.current);
  }, [isPlaying, currentFrame, rawFrames, delays]);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      const file = e.dataTransfer.files[0];
      if (file) processFile(file);
    },
    [processFile]
  );

  const reset = () => {
    if (gifUrl) URL.revokeObjectURL(gifUrl);
    setRawFrames([]);
    setCurrentFrame(0);
    setDelays([]);
    setFileName("");
    setGifUrl("");
    setIsPlaying(true);
    setLuminosity(0);
    setContrast(0);
  };

  const baseName = fileName.replace(/\.[^.]+$/, "") || "ascii";

  /** Render an ASCII frame to a canvas and return its ImageData */
  function renderAsciiToCanvas(frame: typeof currentAsciiFrame, cMode: ColorMode, mColor: string) {
    if (!frame) return null;
    const charW = 6;
    const charH = 10;
    const cols = frame[0].length;
    const rows = frame.length;
    const canvas = document.createElement("canvas");
    canvas.width = cols * charW;
    canvas.height = rows * charH;
    const ctx = canvas.getContext("2d")!;
    ctx.fillStyle = "#0a0f0a";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.font = `${charH}px monospace`;
    ctx.textBaseline = "top";

    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < cols; c++) {
        const ch = frame[r][c];
        if (cMode === "mono") {
          ctx.fillStyle = mColor;
        } else {
          ctx.fillStyle = `rgb(${Math.round(ch.r)},${Math.round(ch.g)},${Math.round(ch.b)})`;
        }
        ctx.fillText(ch.char, c * charW, r * charH);
      }
    }
    return { canvas, ctx, width: canvas.width, height: canvas.height };
  }

  const exportAsGif = useCallback(async () => {
    if (rawFrames.length === 0) return;
    setExporting(true);
    setExportProgress(0);
    setExportLabel("Rendering GIF…");
    try {
      const total = rawFrames.length;
      const allFrames = rawFrames.map((f, i) => {
        const result = computeAsciiFrame(f, luminosity, contrast, brightnessRange.min, brightnessRange.max, targetCols, colorBoost);
        return result;
      });
      const first = renderAsciiToCanvas(allFrames[0], colorMode, monoColor);
      if (!first) return;
      const { width, height } = first;

      const gif = GIFEncoder();

      for (let i = 0; i < allFrames.length; i++) {
        setExportProgress(Math.round(((i + 1) / total) * 90));
        setExportLabel(`Encoding frame ${i + 1} / ${total}`);
        const result = renderAsciiToCanvas(allFrames[i], colorMode, monoColor);
        if (!result) continue;
        const imageData = result.ctx.getImageData(0, 0, width, height);
        const palette = quantize(imageData.data, 256);
        const index = applyPalette(imageData.data, palette);
        gif.writeFrame(index, width, height, {
          palette,
          delay: delays[i] || 100,
        });
        // Yield to UI thread
        if (i % 5 === 0) await new Promise((r) => setTimeout(r, 0));
      }

      setExportProgress(95);
      setExportLabel("Finalizing…");
      gif.finish();
      const blob = new Blob([gif.bytes()], { type: "image/gif" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = baseName + "-ascii.gif";
      a.click();
      URL.revokeObjectURL(a.href);
      setExportProgress(100);
    } finally {
      setTimeout(() => setExporting(false), 400);
    }
  }, [rawFrames, luminosity, contrast, brightnessRange, colorMode, monoColor, delays, baseName, targetCols, colorBoost]);

  const exportAsTxtZip = useCallback(async () => {
    if (rawFrames.length === 0) return;
    setExporting(true);
    setExportProgress(0);
    setExportLabel("Packing text frames…");
    try {
      const zip = new JSZip();
      const total = rawFrames.length;
      for (let i = 0; i < total; i++) {
        const frame = computeAsciiFrame(rawFrames[i], luminosity, contrast, brightnessRange.min, brightnessRange.max, targetCols, colorBoost);
        const text = frame.map((line) => line.map((c) => c.char).join("")).join("\n");
        const num = String(i + 1).padStart(3, "0");
        zip.file(`${num}.txt`, text);
        setExportProgress(Math.round(((i + 1) / total) * 80));
        setExportLabel(`Frame ${i + 1} / ${total}`);
        if (i % 10 === 0) await new Promise((r) => setTimeout(r, 0));
      }
      setExportProgress(90);
      setExportLabel("Compressing ZIP…");
      const blob = await zip.generateAsync({ type: "blob" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = baseName + "-ascii-frames.zip";
      a.click();
      URL.revokeObjectURL(a.href);
      setExportProgress(100);
    } finally {
      setTimeout(() => setExporting(false), 400);
    }
  }, [rawFrames, luminosity, contrast, brightnessRange, baseName, targetCols, colorBoost]);

  return (
    <div className="relative flex min-h-screen flex-col items-center bg-background px-4 py-8 font-mono text-foreground">
      {/* GitHub link - top right */}
      <a
        href="https://github.com/row-huh/gif-to-ascii"
        target="_blank"
        rel="noopener noreferrer"
        className="absolute right-4 top-4 rounded border border-border p-1.5 text-muted-foreground transition-colors hover:border-primary hover:text-primary"
        title="View on GitHub"
      >
        <Github className="h-4 w-4" />
      </a>

      {rawFrames.length === 0 ? (
        <div className="flex w-full max-w-5xl flex-1 flex-col items-center justify-center gap-10">
          {/* ASCII art heading */}
          <div className="flex flex-col items-center gap-4">
            <h1 className="sr-only">GIF 2 ASCII</h1>
            <pre className="hidden select-none text-[7px] leading-[1.1] text-primary sm:block md:text-[9px] lg:text-[11px]" aria-hidden="true">
              {ASCII_TITLE}
            </pre>
            <span className="block text-2xl font-bold tracking-widest text-primary sm:hidden">GIF → ASCII</span>
            <div className="flex items-center gap-4">
              <p className="text-xs text-muted-foreground tracking-widest uppercase">Drop a GIF and watch it render in text</p>
            </div>
          </div>

          {/* Preview showcase */}
          <PreviewShowcase />

          {/* Upload area */}
          <div
            onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
            onDragLeave={() => setDragOver(false)}
            onDrop={handleDrop}
            onClick={() => fileRef.current?.click()}
            className={`flex w-full max-w-5xl cursor-pointer flex-col items-center justify-center rounded-lg border-2 border-dashed py-16 transition-all ${
              dragOver ? "border-primary bg-primary/5" : "border-border hover:border-primary/40"
            }`}
          >
            {loading ? (
              <p className="animate-pulse text-primary">Processing...</p>
            ) : (
              <>
                <Upload className="mb-3 h-8 w-8 text-muted-foreground" />
                <p className="text-sm text-muted-foreground">Click or drag a GIF here</p>
              </>
            )}
            <input
              ref={fileRef}
              type="file"
              accept="image/gif"
              className="hidden"
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) processFile(f);
              }}
            />
          </div>

          <p className="text-[10px] text-muted-foreground/50 tracking-widest uppercase">All processing happens locally in your browser</p>
        </div>
      ) : (
        <div className="w-full max-w-7xl">
          {/* Controls */}
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <span className="text-xs text-muted-foreground">
              {fileName}
              {rawFrames.length > 1 && ` • frame ${currentFrame + 1}/${rawFrames.length}`}
              {` • range ${Math.round(brightnessRange.min)}–${Math.round(brightnessRange.max)}`}
            </span>
            <div className="flex gap-2">
              <Button variant="outline" size="sm" onClick={() => { setLuminosity(0); setContrast(0); setFontSize(10); setColorBoost(0); setColorMode("original"); }} className="h-7 gap-1 font-mono text-xs">
                <RotateCcw className="h-3 w-3" /> Reset
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="sm" disabled={exporting} className="h-7 gap-1 font-mono text-xs">
                    <Download className="h-3 w-3" /> {exporting ? "Exporting…" : "Export"}
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="font-mono text-xs">
                  <DropdownMenuItem onClick={exportAsGif} className="gap-2">
                    <FileImage className="h-3 w-3" /> Download as .gif
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={exportAsTxtZip} className="gap-2">
                    <FileText className="h-3 w-3" /> Download frames as .txt (ZIP)
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
              {rawFrames.length > 1 && (
                <Button variant="outline" size="sm" onClick={() => setIsPlaying(!isPlaying)} className="h-7 gap-1 font-mono text-xs">
                  {isPlaying ? <Pause className="h-3 w-3" /> : <Play className="h-3 w-3" />}
                </Button>
              )}
              <Button variant="outline" size="sm" onClick={reset} className="h-7 gap-1 font-mono text-xs">
                <X className="h-3 w-3" /> Clear
              </Button>
            </div>
          </div>

          {/* Side by side */}
          <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
            <div>
              <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">Original</p>
              <div className="flex items-center justify-center overflow-hidden rounded border border-border bg-card p-2">
                <img src={gifUrl} alt="Original GIF" className="max-h-[500px] max-w-full object-contain" />
              </div>
            </div>
            <div>
              <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">ASCII</p>
              {currentAsciiFrame && (
                <AsciiCanvas frame={currentAsciiFrame} colorMode={colorMode} monoColor={monoColor} fontSize={fontSize} />
              )}
            </div>
          </div>

          {/* Controls bar */}
          <div className="mt-6 space-y-3">
            <div className="flex items-center gap-3 rounded border border-border bg-card p-3">
              <Sun className="h-4 w-4 shrink-0 text-muted-foreground" />
              <span className="w-20 shrink-0 text-xs text-muted-foreground">Luminosity</span>
              <Slider value={[luminosity]} onValueChange={([v]) => setLuminosity(v)} min={-100} max={100} step={1} className="flex-1" />
              <span className="w-10 text-right text-xs tabular-nums text-muted-foreground">{luminosity > 0 ? "+" : ""}{luminosity}</span>
            </div>
            <div className="flex items-center gap-3 rounded border border-border bg-card p-3">
              <Contrast className="h-4 w-4 shrink-0 text-muted-foreground" />
              <span className="w-20 shrink-0 text-xs text-muted-foreground">Contrast</span>
              <Slider value={[contrast]} onValueChange={([v]) => setContrast(v)} min={-100} max={100} step={1} className="flex-1" />
              <span className="w-10 text-right text-xs tabular-nums text-muted-foreground">{contrast > 0 ? "+" : ""}{contrast}</span>
            </div>
            <div className="flex items-center gap-3 rounded border border-border bg-card p-3">
              <Palette className="h-4 w-4 shrink-0 text-muted-foreground" />
              <span className="w-20 shrink-0 text-xs text-muted-foreground">Intensity</span>
              <Slider value={[colorBoost]} onValueChange={([v]) => setColorBoost(v)} min={-100} max={100} step={1} className="flex-1" />
              <span className="w-10 text-right text-xs tabular-nums text-muted-foreground">{colorBoost > 0 ? "+" : ""}{colorBoost}</span>
            </div>
            <div className="flex items-center gap-3 rounded border border-border bg-card p-3">
              <span className="shrink-0 text-xs font-bold text-muted-foreground">Aa</span>
              <span className="w-20 shrink-0 text-xs text-muted-foreground">Detail</span>
              <Slider value={[fontSize]} onValueChange={([v]) => setFontSize(v)} min={4} max={20} step={1} className="flex-1" />
              <span className="w-16 text-right text-xs tabular-nums text-muted-foreground">{targetCols} cols</span>
            </div>
            <div className="flex flex-wrap items-center gap-3 rounded border border-border bg-card p-3">
              <Palette className="h-4 w-4 text-muted-foreground" />
              <button onClick={() => setColorMode("original")} className={`rounded px-3 py-1 text-xs font-medium transition-colors ${colorMode === "original" ? "bg-primary text-primary-foreground" : "bg-secondary text-secondary-foreground hover:bg-accent"}`}>Original Colors</button>
              <div className="h-4 w-px bg-border" />
              <button onClick={() => setColorMode("mono")} className={`rounded px-3 py-1 text-xs font-medium transition-colors ${colorMode === "mono" ? "bg-primary text-primary-foreground" : "bg-secondary text-secondary-foreground hover:bg-accent"}`}>Mono</button>
              {colorMode === "mono" && (
                <input type="color" value={monoColor} onChange={(e) => setMonoColor(e.target.value)} className="h-7 w-10 cursor-pointer rounded border border-border bg-transparent" title="Pick color" />
              )}
            </div>
          </div>
        </div>
      )}
      {/* Export progress dialog */}
      <Dialog open={exporting} onOpenChange={() => {}}>
        <DialogContent className="max-w-sm font-mono [&>button]:hidden" onPointerDownOutside={(e) => e.preventDefault()}>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" />
              Exporting
            </DialogTitle>
            <DialogDescription className="text-xs">
              {exportLabel}
            </DialogDescription>
          </DialogHeader>
          <Progress value={exportProgress} className="h-2" />
          <p className="text-right text-xs tabular-nums text-muted-foreground">{exportProgress}%</p>
        </DialogContent>
      </Dialog>
    </div>
  );
}
