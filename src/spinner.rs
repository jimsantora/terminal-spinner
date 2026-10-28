use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
    Frame,
};

use crate::render::{to_rgba8, Look, Shader, View};

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
        let hue = self
            .rainbow
            .then(|| (self.rainbow_phase * 360.0).rem_euclid(360.0));
        render_spinner(area, buf, self.angle, hue);
    }
}

/// Half-block fallback: each cell holds two vertically stacked pixels, drawn
/// with the same shader as the Kitty graphics renderer (minus the shadow,
/// since we can't blend against the terminal's own background).
fn render_spinner(area: Rect, buf: &mut Buffer, angle: f64, hue: Option<f64>) {
    let w = area.width as usize;
    let h = area.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    let scale = (w.min(2 * h)) as f64 / 2.0 * 0.92;
    let view = View {
        cx: w as f64 / 2.0,
        cy: h as f64,
        sx: scale,
        sy: scale,
    };
    let shader = Shader::new(
        view,
        Look {
            angle,
            blur: 0.0,
            hue,
            shadow: false,
        },
    );

    // Shade rows in parallel; big terminals have a lot of cells.
    let mut pixels = vec![[None::<Color>; 2]; w * h];
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk_idx, chunk) in pixels.chunks_mut(rows_per * w).enumerate() {
            let shader = &shader;
            scope.spawn(move || {
                for (i, cell) in chunk.iter_mut().enumerate() {
                    let row = chunk_idx * rows_per + i / w;
                    let x = (i % w) as f64 + 0.5;
                    for (sub, out) in cell.iter_mut().enumerate() {
                        let y = (row * 2 + sub) as f64 + 0.5;
                        let [r, g, b, a] = to_rgba8(shader.pixel(x, y));
                        *out = (a >= 128).then_some(Color::Rgb(r, g, b));
                    }
                }
            });
        }
    });

    for (i, [top, bot]) in pixels.into_iter().enumerate() {
        let cell = &mut buf[(area.x + (i % w) as u16, area.y + (i / w) as u16)];
        if top.is_none() && bot.is_none() {
            cell.set_char(' ');
            cell.set_fg(Color::Reset);
            cell.set_bg(Color::Reset);
        } else {
            cell.set_char('▀');
            cell.set_fg(top.unwrap_or(Color::Reset));
            cell.set_bg(bot.unwrap_or(Color::Reset));
        }
    }
}

/// h in degrees [0,360), s and v in [0,1].
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Color {
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

pub fn render_help_popup(frame: &mut Frame, area: Rect, renderer: &str) {
    let popup_area = centered_rect(50, 48, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .title(" Terminal Fidget Spinner — Help ")
        .borders(Borders::ALL);
    let text = format!(
        "\
Controls:
  \u{2190}/\u{2192}     accelerate spin (left/right)
  Space   hard stop
  ?       toggle this help
  q/Esc   quit
  Ctrl+C  quit

A tiny terminal fidget spinner. Speed up, slow
down, reverse, and watch it go rainbow near max
speed.

Renderer: {renderer}

Press ? or Esc to close."
    );
    let para = Paragraph::new(text).block(block);
    frame.render_widget(para, popup_area);
}
