mod app;
mod input;
mod kitty;
mod render;
mod spinner;

use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};

use crossterm::{
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;
use input::{Input, Reader};
use kitty::{KittyRenderer, Pose};
use spinner::{render_help_popup, SpinnerWidget};

/// DEC mode 2033: the terminal tells us when we're hidden, so we can stop
/// drawing (e.g. in a background tab).
const VISIBILITY_REPORTS_ON: &[u8] = b"\x1b[?2033h";
const VISIBILITY_REPORTS_OFF: &[u8] = b"\x1b[?2033l";

fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    let _ = stdout.write_all(VISIBILITY_REPORTS_OFF);
    disable_raw_mode()?;
    execute!(stdout, LeaveAlternateScreen)?;
    execute!(stdout, crossterm::cursor::Show)?;
    Ok(())
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

fn main() -> io::Result<()> {
    let force_halfblock = std::env::args().skip(1).any(|a| a == "--halfblock");

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let _guard = TerminalGuard;

    let mut reader = Reader::new()?;
    let kitty = if cfg!(unix) && !force_halfblock && kitty::detect(&mut reader, &mut io::stdout())?
    {
        Some(KittyRenderer::new())
    } else {
        None
    };
    if cfg!(unix) {
        io::stdout().write_all(VISIBILITY_REPORTS_ON)?;
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    run_app(&mut terminal, &mut reader, kitty)
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    reader: &mut Reader,
    mut kitty: Option<KittyRenderer>,
) -> io::Result<()> {
    let mut app = App::new();
    let tick_rate = Duration::from_millis(16);
    let mut visible = true;
    // Kitty graphics commands, written after each ratatui draw.
    let mut out = Vec::new();
    let mut stdout = io::stdout();

    while app.running {
        if let Some(k) = kitty.as_mut() {
            // No pixel size, or the terminal keeps rejecting our images:
            // fall back to half-blocks.
            if !k.sync_geometry(&mut out) || k.failed() {
                kitty.take().unwrap().shutdown(&mut out);
            }
        }

        if visible {
            execute!(stdout, BeginSynchronizedUpdate)?;
            terminal.draw(|frame| {
                let area = frame.area();
                if kitty.is_none() {
                    frame.render_widget(
                        SpinnerWidget::new(app.angle, app.is_rainbow(), app.rainbow_phase),
                        area,
                    );
                }
                if app.show_help {
                    let renderer = if kitty.is_some() {
                        "Kitty graphics"
                    } else {
                        "half-block"
                    };
                    render_help_popup(frame, area, renderer);
                }
            })?;
            if let Some(k) = kitty.as_mut() {
                let pose = Pose {
                    angle: app.angle,
                    omega: app.omega,
                    rainbow: app.is_rainbow(),
                    rainbow_phase: app.rainbow_phase,
                };
                k.draw(&pose, app.show_help, &mut out);
            }
            stdout.write_all(&out)?;
            execute!(stdout, EndSynchronizedUpdate)?;
        } else if let Some(k) = kitty.as_mut() {
            k.idle(&mut out);
            stdout.write_all(&out)?;
            stdout.flush()?;
        }
        out.clear();

        let timeout = if visible {
            tick_rate.saturating_sub(app.last_tick.elapsed())
        } else {
            Duration::from_millis(100)
        };
        for event in reader.poll(timeout)? {
            match event {
                Input::Key(code, modifiers) => app.on_key(code, modifiers),
                Input::Visible(v) => visible = v,
                Input::Graphics { id, message } => {
                    if let Some(k) = kitty.as_mut() {
                        k.on_response(id, &message, &mut out);
                    }
                }
                Input::DeviceAttributes => {}
            }
        }

        let dt = app.last_tick.elapsed().as_secs_f64();
        app.last_tick = Instant::now();
        app.tick(dt);
    }

    if let Some(k) = kitty {
        k.shutdown(&mut out);
    }
    stdout.write_all(&out)?;
    stdout.flush()
}
