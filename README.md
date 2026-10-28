# terminal-spinner

A fidget spinner for your terminal, built with [ratatui](https://ratatui.rs).

Flick it with the arrow keys, watch it spin down like the real thing, and
catch it going rainbow if you get it up to full speed.

![demo](assets/demo.gif)

## Features

- Real-pixel rendering with the Kitty graphics protocol (Ghostty 1.4+, Kitty):
  anti-aliased edges, bevel shading, metal bearings, a soft drop shadow and
  speed-dependent motion blur
- Falls back to true-color half-block pixels everywhere else (not ASCII art)
- Renders on your terminal's own background color/theme — no fixed backdrop
- Realistic physics: taps accelerate it, friction winds it back down over a few seconds
- Rainbow color cycle at top speed
- Built-in help popup (`?`)

## Controls

| Key | Action |
|---|---|
| `←` / `→` | Accelerate spin left / right (tap repeatedly to go faster) |
| `Space` | Hard stop |
| `?` | Toggle help popup |
| `q` / `Esc` / `Ctrl+C` | Quit |

Run with `--halfblock` to force the half-block renderer, e.g. to compare.

## Requirements

- A terminal with true-color (24-bit RGB) support
- For the high-res renderer: a terminal with Kitty graphics *animation*
  support (Ghostty 1.4+, Kitty) on macOS or Linux
- Rust 1.74+ (2021 edition)

## Build & run

```sh
cargo run --release
```

## Install locally

```sh
cargo install --path .
```

This installs the `terminal-spinner` binary to `~/.cargo/bin` (make sure that's on your `PATH`).

## Download a prebuilt binary

Prebuilt binaries for macOS (Apple Silicon & Intel), Linux (x86_64), and
Windows (x86_64) are attached to each [GitHub Release](https://github.com/jimsantora/terminal-spinner/releases).
Download the archive for your platform, extract it, and run the
`terminal-spinner` (or `terminal-spinner.exe`) binary directly.

## Cutting a release

Releases are built and published automatically by
[`.github/workflows/release.yml`](.github/workflows/release.yml) whenever a
tag matching `v*.*.*` is pushed:

```sh
git tag v0.1.0
git push origin v0.1.0
```

This cross-compiles the binary for each supported platform and attaches the
archives to a new GitHub Release for that tag.

## How it works

The spinner body is procedurally defined (a center hub plus three lobes at
120° apart) as a signed distance field in `src/render.rs`. That gives
anti-aliasing, the rounded bevel, fillets and the soft shadow almost for free.
Lighting is fixed in screen space, so the highlights stay put while the
spinner turns underneath them. `src/app.rs` holds the spin physics (angular
velocity, friction, acceleration).

**Kitty graphics** (`src/kitty.rs`): the spinner looks identical every 120°,
so a set of frames covering one third of a turn is enough. Background
threads render sets for four motion-blur levels plus 16 rainbow hues, and
each set is uploaded once as a single animated Kitty image. After that,
showing a frame is a ~25 byte `a=a,c=N` command instead of a full-screen
repaint. Frames the spinner needs right now jump the render queue; until
they arrive, the closest available frame is shown. If the terminal drops
an image (e.g. under storage pressure), it is re-rendered on demand.

Support is detected at startup by uploading a 1×1 image and adding a frame
to it: terminals without animation support (such as Ghostty before 1.4)
reject the frame and get the half-block renderer.

**Half-block fallback**: each character cell is split into two pixels via
the `▀` glyph's foreground/background colors, shaded with the same code.

**Being a good citizen**: frames are wrapped in synchronized output (mode
2026) so they never tear, and with visibility reports (mode 2033) the
spinner stops drawing while its tab or window is hidden.
