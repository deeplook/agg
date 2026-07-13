# Changelog

This is a **fork of [asciinema/agg](https://github.com/asciinema/agg)** that adds
support for rendering **inline images** embedded in asciicast recordings into the
output GIF. It is a strict superset of upstream agg: recordings without inline
images render exactly as before, with identical flags and output.

The fork lives at **<https://github.com/deeplook/agg>** and is based on upstream
agg **1.9.0**.

This file documents what this fork adds relative to upstream. The format is based
on [Keep a Changelog](https://keepachangelog.com/).

---

## Installing / using this fork over upstream agg

This fork builds the same `agg` binary, so it can replace an existing install.

**With Cargo (recommended):**

```sh
cargo install --git https://github.com/deeplook/agg --locked
```

This installs `agg` into `~/.cargo/bin`. Make sure that directory precedes any
other `agg` (e.g. a Homebrew-installed upstream one) on your `PATH`:

```sh
which agg          # should print ~/.cargo/bin/agg
agg --version
```

**From source:**

```sh
git clone https://github.com/deeplook/agg
cd agg
cargo build --release
# binary at target/release/agg
```

**Optional runtime dependency (PDF only):** rendering inline **PDF** images
requires one external rasterizer on your `PATH` — `pdftoppm` (poppler), `gs`
(ghostscript), or `mutool` (mupdf). On macOS the built-in `sips` is used
automatically, so no install is needed there. All other image formats work with
no external tools.

**Try it** with the bundled example recordings:

```sh
agg tests/assets/images-iterm2.cast      out.gif   # PNG/JPEG/WebP/BMP/GIF/SVG/PDF
agg tests/assets/images-kitty.cast       out.gif   # kitty graphics protocol
agg tests/assets/image-animated-gif.cast out.gif   # animated GIF playback
agg tests/assets/image-pdf.cast          out.gif   # PDF (needs a rasterizer)
```

---

## [1.9.0+images] — 2026-07-13

Based on upstream agg 1.9.0. `agg --version` reports `1.9.0+images` so this fork
is distinguishable from an upstream install.

### Added

- **iTerm2 inline images (OSC 1337).** Images emitted via the iTerm2 inline
  image protocol (e.g. `imgcat`, Streamlit, matplotlib terminal backends) are
  parsed out of the recording, positioned on the terminal grid, and composited
  into the GIF.
  - Formats: **PNG, JPEG, GIF, WebP, BMP, SVG, and PDF**.
  - Both the simple `File=` form and the chunked `MultipartFile` form.
  - Format is detected from the data's magic bytes (not filename/params).
  - Works with both the `swash` (default) and `resvg` renderers.

- **kitty graphics protocol.** APC-based graphics sequences
  (`ESC _ G … ESC \`) are supported for direct transmission:
  - Transmit-and-display (`a=T`) and transmit-then-put by image id
    (`a=t` then `a=p`).
  - PNG (`f=100`) and raw RGB/RGBA pixels (`f=24`/`f=32`), optionally
    zlib-compressed (`o=z`) and/or split across chunks (`m=1`).
  - Cell-sized placement via `c`/`r`, and delete-all (`a=d`).
  - Both protocols can appear in the same recording.

- **Animated GIF and APNG playback.** Embedded animated images now play in the
  output GIF instead of showing only their first frame. agg synthesizes frames
  during the recording's idle gaps (bounded by `--fps-cap`), advancing the
  embedded animation and looping it. See _Notes & limitations_ below.

- **PDF rendering** via an external rasterizer, tried in order: `pdftoppm`,
  `gs`, `mutool`, then macOS's built-in `sips`. The first page is rendered. If
  none is available, the PDF image is skipped and a one-time warning is logged.

- **`--no-inline-images`** flag to disable inline-image parsing and compositing
  entirely (falls back to text-only rendering, skipping all image work).

- **Example recordings** under `tests/assets/`: `images-iterm2.cast`,
  `images-kitty.cast`, `image-animated-gif.cast`, and `image-pdf.cast`.

### Image sizing

- Height is driven by the reserved terminal rows (explicit `height=`/`r=` in
  cells, else derived from the image's natural size capped to the terminal
  width). Width follows the source aspect ratio (letterboxed to fit,
  `object-fit: contain` style), never exceeding the space to the right edge.
- Because sizing is measured in terminal cells, images scale with `--font-size`
  and `--line-height`, and are bounded by `--cols`.

### Notes & limitations

- **Animated images play only within existing idle gaps** in the recording. If a
  recording ends immediately after the image (no later event), the animation
  freezes on its first frame. This is intentional (it keeps output length tied to
  the recording); no synthetic trailing playback is added.
- **PDF requires an external rasterizer** (see above); macOS works out of the box
  via `sips` (rendered at the PDF's native resolution).
- **kitty:** file/shared-memory transmission media, the native animation protocol
  (`a=f`/`a=a`), unicode placeholders, selective deletes (only delete-all is
  honored), and the do-not-move-cursor flag are not implemented — a
  recording-to-GIF renderer doesn't need them.
- Behavior is unchanged for recordings that contain no inline images.

### Unchanged from upstream

Everything else — CLI flags, themes, fonts, frame selection, GIF encoding — is
identical to upstream agg 1.9.0.

---

## Upstream history

For changes in agg itself (versions up to and including 1.9.0), see the
[upstream repository](https://github.com/asciinema/agg).

### Credits

agg is created and maintained by Marcin Kulik and the asciinema project. This
fork only adds the inline-image features listed above.
