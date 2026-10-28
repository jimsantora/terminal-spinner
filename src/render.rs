//! Resolution-independent spinner rasterizer.
//!
//! The spinner is described as a signed distance field (SDF), which gives us
//! analytic anti-aliasing, a rounded bevel, soft drop shadows and fillets for
//! free. Lighting is fixed in screen space, so highlights stay put while the
//! spinner rotates underneath them — like a real spinner under a desk lamp.

use std::f64::consts::TAU;

use crate::spinner::{
    hsv_to_rgb, ARM_HALF_WIDTH, PALETTE, RING_INNER_FRAC, RING_MID_FRAC, R_ARM, R_CENTER, R_LOBE,
    R_YELLOW,
};

/// Fillet radius used when blending the hub, arms and lobes together.
const FILLET: f64 = 0.06;
/// Width of the rounded edge on the body, in spinner units.
const BEVEL: f64 = 0.045;
/// Shadow offset (screen space, down-right of a top-left light).
const SHADOW_OFFSET: (f64, f64) = (0.025, 0.04);
const SHADOW_ALPHA: f64 = 0.4;
const SHADOW_SOFTNESS: f64 = 0.035;
/// Nothing (spinner or shadow) is ever drawn beyond this radius.
const MAX_EXTENT: f64 = 1.05;
/// Max distance between motion blur samples at the spinner's tips, in pixels.
const BLUR_STEP_PX: f64 = 2.0;

/// Where the spinner sits in the output image, in output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub cx: f64,
    pub cy: f64,
    /// Output pixels per spinner unit.
    pub scale: f64,
}

/// What to draw.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub angle: f64,
    /// Total motion blur arc in radians (0 = sharp).
    pub blur: f64,
    /// Rainbow hue in degrees, or `None` for the normal palette.
    pub hue: Option<f64>,
    pub shadow: bool,
}

type Rgb = [f64; 3];

/// Per-frame constants shared by every pixel.
pub struct Shader {
    view: View,
    shadow: bool,
    angle: f64,
    blur: f64,
    /// Number of motion blur samples per pixel.
    samples: usize,
    lobes: [(f64, f64); 3],
    body: Rgb,
    ring_mid: Rgb,
    ring_inner: Rgb,
    hub: Rgb,
}

impl Shader {
    pub fn new(view: View, look: Look) -> Self {
        let tip_px = view.scale * (R_ARM + R_LOBE);
        let samples = ((look.blur * tip_px / BLUR_STEP_PX).ceil() as usize).max(1);
        let lobes =
            [0.0, TAU / 3.0, 2.0 * TAU / 3.0].map(|a: f64| (R_ARM * a.cos(), R_ARM * a.sin()));

        let [body, ring_mid, ring_inner, hub] = match look.hue {
            Some(h) => [
                hsv_to_rgb(h, 0.75, 0.45),
                hsv_to_rgb(h, 0.65, 0.70),
                hsv_to_rgb(h, 0.35, 0.90),
                hsv_to_rgb(h, 0.85, 0.95),
            ],
            None => [
                PALETTE.body,
                PALETTE.ring_mid,
                PALETTE.ring_inner,
                PALETTE.hub_yellow,
            ],
        }
        .map(rgb_f64);

        Self {
            view,
            shadow: look.shadow,
            angle: look.angle,
            blur: look.blur,
            samples,
            lobes,
            body,
            ring_mid,
            ring_inner,
            hub,
        }
    }

    /// Premultiplied RGBA (0..1) for the pixel whose center is at (x, y).
    pub fn pixel(&self, x: f64, y: f64) -> [f64; 4] {
        let u = (x - self.view.cx) / self.view.scale;
        let v = (y - self.view.cy) / self.view.scale;
        if u * u + v * v > MAX_EXTENT * MAX_EXTENT {
            return [0.0; 4];
        }
        // Stratified samples across the blur arc, jittered per pixel so
        // the copies blend into a smooth smear instead of visible steps.
        let n = self.samples;
        let jitter = if n == 1 { 0.5 } else { hash(x, y) };
        let mut acc = [0.0; 4];
        for k in 0..n {
            let t = (k as f64 + jitter) / n as f64 - 0.5;
            let a = -(self.angle + t * self.blur);
            let p = self.sample(u, v, a.cos(), a.sin());
            for i in 0..4 {
                acc[i] += p[i];
            }
        }
        acc.map(|x| x / n as f64)
    }

    fn sample(&self, u: f64, v: f64, c: f64, s: f64) -> [f64; 4] {
        let pw = 1.0 / self.view.scale;
        let to_local = |x: f64, y: f64| (x * c - y * s, x * s + y * c);

        let (lx, ly) = to_local(u, v);
        let d = self.sdf(lx, ly);
        let cov = coverage(d, pw);

        let shadow = if self.shadow && cov < 1.0 {
            let (sx, sy) = to_local(u - SHADOW_OFFSET.0, v - SHADOW_OFFSET.1);
            let soft = SHADOW_SOFTNESS + pw;
            SHADOW_ALPHA * (1.0 - smoothstep(-soft, soft, self.sdf(sx, sy)))
        } else {
            0.0
        };

        if cov <= 0.0 {
            return [0.0, 0.0, 0.0, shadow];
        }

        // Light and half vector, rotated into the spinner's local frame.
        let (lgx, lgy) = to_local(-0.45, -0.6);
        let light = normalize([lgx, lgy, 0.66]);
        let half = normalize([light[0], light[1], light[2] + 1.0]);
        let lit = |base: Rgb, n: [f64; 3], diffuse: (f64, f64), spec: (f64, f64)| -> Rgb {
            let ndl = dot(n, light).max(0.0);
            let ndh = dot(n, half).max(0.0);
            let k = diffuse.0 + diffuse.1 * ndl;
            let sp = spec.1 * ndh.powf(spec.0);
            base.map(|ch| ch * k + sp)
        };

        // Body: plastic with a rounded edge.
        let body_n = if d > -BEVEL {
            let (gx, gy) = self.gradient(lx, ly);
            let t = (-d / BEVEL).clamp(0.0, 1.0);
            let slope = (1.0 - t).powf(1.5) * 2.4;
            normalize([gx * slope, gy * slope, 1.0])
        } else {
            [0.0, 0.0, 1.0]
        };
        let mut color = lit(self.body, body_n, (0.45, 0.65), (24.0, 0.3));

        // Bearings in the lobes: a metal outer race and a domed cap.
        let (lcx, lcy) = self
            .lobes
            .iter()
            .copied()
            .min_by(|a, b| dist2(lx, ly, *a).total_cmp(&dist2(lx, ly, *b)))
            .unwrap();
        let (dx, dy) = (lx - lcx, ly - lcy);
        let r = (dx * dx + dy * dy).sqrt();
        let dir = if r > 1e-9 {
            (dx / r, dy / r)
        } else {
            (0.0, 0.0)
        };
        let r_mid = RING_MID_FRAC * R_LOBE;
        let r_inner = RING_INNER_FRAC * R_LOBE;

        let in_mid = coverage(r - r_mid, pw);
        if in_mid > 0.0 {
            let center = (r_mid + r_inner) / 2.0;
            let half_w = (r_mid - r_inner) / 2.0;
            let t = ((r - center) / half_w).clamp(-1.0, 1.0);
            let n = normalize([dir.0 * t * 1.1, dir.1 * t * 1.1, 1.0]);
            let race = lit(self.ring_mid, n, (0.3, 0.7), (60.0, 0.9));
            color = lerp(color, race, in_mid);
        }
        let in_inner = coverage(r - r_inner, pw);
        if in_inner > 0.0 {
            let t = (r / r_inner).min(1.0);
            let n = normalize([dir.0 * t * 0.8, dir.1 * t * 0.8, 1.0]);
            let cap = lit(self.ring_inner, n, (0.4, 0.6), (40.0, 0.7));
            color = lerp(color, cap, in_inner);
        }

        // Domed hub cap.
        let rh = (lx * lx + ly * ly).sqrt();
        let in_hub = coverage(rh - R_YELLOW, pw);
        if in_hub > 0.0 {
            let t = (rh / R_YELLOW).min(1.0);
            let k = if rh > 1e-9 { t * 0.75 / rh } else { 0.0 };
            let n = normalize([lx * k, ly * k, 1.0]);
            let cap = lit(self.hub, n, (0.45, 0.6), (30.0, 0.55));
            color = lerp(color, cap, in_hub);
        }

        // Thin grooves where parts meet.
        let groove = |dist: f64| 1.0 - 0.45 * (-(dist / (0.008 + pw)).powi(2)).exp();
        let shade = groove(r - r_mid) * groove(r - r_inner) * groove(rh - R_YELLOW);
        color = color.map(|ch| (ch * shade).clamp(0.0, 1.0));

        // Spinner over its own shadow (premultiplied).
        let a = cov + shadow * (1.0 - cov);
        [color[0] * cov, color[1] * cov, color[2] * cov, a]
    }

    fn sdf(&self, x: f64, y: f64) -> f64 {
        let mut d = (x * x + y * y).sqrt() - R_CENTER;
        for &(cx, cy) in &self.lobes {
            d = smin(d, dist2(x, y, (cx, cy)).sqrt() - R_LOBE, FILLET);
            d = smin(d, segment_dist(x, y, cx, cy) - ARM_HALF_WIDTH, FILLET);
        }
        d
    }

    fn gradient(&self, x: f64, y: f64) -> (f64, f64) {
        let e = 1e-3;
        let gx = self.sdf(x + e, y) - self.sdf(x - e, y);
        let gy = self.sdf(x, y + e) - self.sdf(x, y - e);
        let len = (gx * gx + gy * gy).sqrt();
        if len > 1e-12 {
            (gx / len, gy / len)
        } else {
            (0.0, 0.0)
        }
    }
}

/// Render a straight-alpha RGBA8 image.
pub fn render_rgba(width: u32, height: u32, view: View, look: Look) -> Vec<u8> {
    let shader = Shader::new(view, look);
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for (i, px) in out.chunks_exact_mut(4).enumerate() {
        let x = (i % width as usize) as f64 + 0.5;
        let y = (i / width as usize) as f64 + 0.5;
        px.copy_from_slice(&to_rgba8(shader.pixel(x, y)));
    }
    out
}

/// Premultiplied float RGBA to straight-alpha RGBA8.
pub fn to_rgba8(p: [f64; 4]) -> [u8; 4] {
    let a = p[3];
    if a <= 0.0 {
        return [0; 4];
    }
    let q = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    [q(p[0] / a), q(p[1] / a), q(p[2] / a), q(a)]
}

fn rgb_f64(c: ratatui::style::Color) -> Rgb {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => [r, g, b].map(|v| v as f64 / 255.0),
        _ => [0.5; 3],
    }
}

/// Cheap per-pixel pseudo-random number in [0, 1).
fn hash(x: f64, y: f64) -> f64 {
    let h = (x * 12.9898 + y * 78.233).sin() * 43758.5453;
    h - h.floor()
}

fn coverage(d: f64, pw: f64) -> f64 {
    (0.5 - d / pw).clamp(0.0, 1.0)
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn smin(a: f64, b: f64, k: f64) -> f64 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let l = dot(v, v).sqrt();
    v.map(|x| x / l)
}

fn dist2(x: f64, y: f64, c: (f64, f64)) -> f64 {
    (x - c.0).powi(2) + (y - c.1).powi(2)
}

/// Distance from (px, py) to the segment from the origin to (bx, by).
fn segment_dist(px: f64, py: f64, bx: f64, by: f64) -> f64 {
    let t = ((px * bx + py * by) / (bx * bx + by * by)).clamp(0.0, 1.0);
    ((px - t * bx).powi(2) + (py - t * by).powi(2)).sqrt()
}
