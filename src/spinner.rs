use std::f64::consts::PI;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
    Frame,
};

pub struct Palette {
    pub body: Color,
    pub ring_mid: Color,
    pub ring_inner: Color,
    pub hub_yellow: Color,
}

impl Palette {
    pub const fn new() -> Self {
        Self {
            body: Color::Rgb(0x0E, 0x3D, 0x3B),
            ring_mid: Color::Rgb(0x5F, 0xA9, 0xA0),
            ring_inner: Color::Rgb(0x8F, 0xC9, 0xBE),
            hub_yellow: Color::Rgb(0xF4, 0xC4, 0x30),
        }
    }
}

pub const PALETTE: Palette = Palette::new();

pub const R_CENTER: f64 = 0.22;
pub const R_YELLOW: f64 = 0.14;
pub const R_ARM: f64 = 0.62;
pub const R_LOBE: f64 = 0.30;
pub const ARM_HALF_WIDTH: f64 = 0.09;
pub const RING_INNER_FRAC: f64 = 0.45;
pub const RING_MID_FRAC: f64 = 0.75;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PixelClass {
    Background,
    Body,
    RingMid,
    RingInner,
    HubYellow,
}

pub struct SpinnerWidget {
    pub angle: f64,
    pub rainbow: bool,
    pub rainbow_phase: f64,
}

impl SpinnerWidget {
    pub fn new(angle: f64, rainbow: bool, rainbow_phase: f64) -> Self {
        Self {
            angle,
            rainbow,
            rainbow_phase,
        }
    }
}

impl Widget for SpinnerWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_spinner(area, buf, self.angle, self.rainbow, self.rainbow_phase);
    }
}

fn render_spinner(area: Rect, buf: &mut Buffer, angle: f64, rainbow: bool, rainbow_phase: f64) {
    let w = area.width as i32;
    let h = area.height as i32;
    if w == 0 || h == 0 {
        return;
    }

    let cx = w as f64 / 2.0;
    let cy_px = h as f64;
    let radius_px = (w.min(2 * h)) as f64 / 2.0 * 0.92;
    if radius_px <= 0.0 {
        return;
    }

    let cos_a = (-angle).cos();
    let sin_a = (-angle).sin();
    let lobe_centers: [(f64, f64); 3] = [0.0_f64, 2.0 * PI / 3.0, 4.0 * PI / 3.0]
        .map(|a| (R_ARM * a.cos(), R_ARM * a.sin()));

    let hue_deg = (rainbow_phase * 360.0).rem_euclid(360.0);
    // [Body, RingMid, RingInner, HubYellow]
    let rainbow_colors = [
        hsv_to_rgb(hue_deg, 0.75, 0.45),
        hsv_to_rgb(hue_deg, 0.65, 0.70),
        hsv_to_rgb(hue_deg, 0.35, 0.90),
        hsv_to_rgb(hue_deg, 0.85, 0.95),
    ];

    for row in 0..h {
        for col in 0..w {
            let top = classify_pixel(col, row, 0, cx, cy_px, radius_px, cos_a, sin_a, &lobe_centers);
            let bot = classify_pixel(col, row, 1, cx, cy_px, radius_px, cos_a, sin_a, &lobe_centers);

            if top == PixelClass::Background && bot == PixelClass::Background {
                let cell = &mut buf[(area.x + col as u16, area.y + row as u16)];
                cell.set_char(' ');
                cell.set_fg(Color::Reset);
                cell.set_bg(Color::Reset);
                continue;
            }

            let fg = class_to_color(top, rainbow, &rainbow_colors);
            let bg = class_to_color(bot, rainbow, &rainbow_colors);

            let cell = &mut buf[(area.x + col as u16, area.y + row as u16)];
            cell.set_char('▀');
            cell.set_fg(fg);
            cell.set_bg(bg);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_pixel(
    col: i32,
    row: i32,
    sub_row: i32,
    cx: f64,
    cy_px: f64,
    radius_px: f64,
    cos_a: f64,
    sin_a: f64,
    lobe_centers: &[(f64, f64); 3],
) -> PixelClass {
    let px = col as f64 + 0.5 - cx;
    let py = (row as f64 * 2.0 + sub_row as f64 + 0.5) - cy_px;
    let u = px / radius_px;
    let v = py / radius_px;

    let lx = u * cos_a - v * sin_a;
    let ly = u * sin_a + v * cos_a;

    let d_center = (lx * lx + ly * ly).sqrt();
    if d_center <= R_YELLOW {
        return PixelClass::HubYellow;
    }
    if d_center <= R_CENTER {
        return PixelClass::Body;
    }

    for &(lcx, lcy) in lobe_centers {
        let dx = lx - lcx;
        let dy = ly - lcy;
        let d_lobe = (dx * dx + dy * dy).sqrt();
        if d_lobe <= R_LOBE {
            let d = d_lobe / R_LOBE;
            return if d <= RING_INNER_FRAC {
                PixelClass::RingInner
            } else if d <= RING_MID_FRAC {
                PixelClass::RingMid
            } else {
                PixelClass::Body
            };
        }
    }

    for &(lcx, lcy) in lobe_centers {
        if point_to_segment_dist(lx, ly, 0.0, 0.0, lcx, lcy) <= ARM_HALF_WIDTH {
            return PixelClass::Body;
        }
    }

    PixelClass::Background
}

fn point_to_segment_dist(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let abx = bx - ax;
    let aby = by - ay;
    let apx = px - ax;
    let apy = py - ay;
    let ab_len_sq = abx * abx + aby * aby;
    let t = if ab_len_sq > 0.0 {
        ((apx * abx + apy * aby) / ab_len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let cx = ax + t * abx;
    let cy = ay + t * aby;
    let dx = px - cx;
    let dy = py - cy;
    (dx * dx + dy * dy).sqrt()
}

fn class_to_color(class: PixelClass, rainbow: bool, rainbow_colors: &[Color; 4]) -> Color {
    match class {
        PixelClass::Background => Color::Reset,
        PixelClass::Body => {
            if rainbow {
                rainbow_colors[0]
            } else {
                PALETTE.body
            }
        }
        PixelClass::RingMid => {
            if rainbow {
                rainbow_colors[1]
            } else {
                PALETTE.ring_mid
            }
        }
        PixelClass::RingInner => {
            if rainbow {
                rainbow_colors[2]
            } else {
                PALETTE.ring_inner
            }
        }
        PixelClass::HubYellow => {
            if rainbow {
                rainbow_colors[3]
            } else {
                PALETTE.hub_yellow
            }
        }
    }
}

/// h in degrees [0,360), s and v in [0,1].
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Color {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |val: f64| ((val + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to_u8(r1), to_u8(g1), to_u8(b1))
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn render_help_popup(frame: &mut Frame, area: Rect) {
    let popup_area = centered_rect(50, 44, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .title(" Terminal Fidget Spinner — Help ")
        .borders(Borders::ALL);
    let text = "\
Controls:
  \u{2190}/\u{2192}     accelerate spin (left/right)
  Space   hard stop
  ?       toggle this help
  q/Esc   quit
  Ctrl+C  quit

A tiny terminal fidget spinner. Speed up, slow
down, reverse, and watch it go rainbow near max
speed.

Press ? or Esc to close.";
    let para = Paragraph::new(text).block(block);
    frame.render_widget(para, popup_area);
}
