use std::f64::consts::TAU;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};

pub const MAX_OMEGA: f64 = 18.0;
pub const ACCEL_STEP: f64 = 2.5;
pub const FRICTION_PER_SEC: f64 = 0.35;
pub const RAINBOW_THRESHOLD: f64 = 0.87;
pub const OMEGA_EPSILON: f64 = 0.01;
pub const RAINBOW_HUE_SPEED_DEG_PER_SEC: f64 = 180.0;

pub struct App {
    pub angle: f64,
    pub omega: f64,
    pub show_help: bool,
    pub rainbow_phase: f64,
    pub last_tick: Instant,
    pub running: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            angle: 0.0,
            omega: 0.0,
            show_help: false,
            rainbow_phase: 0.0,
            last_tick: Instant::now(),
            running: true,
        }
    }

    pub fn is_rainbow(&self) -> bool {
        self.omega.abs() >= RAINBOW_THRESHOLD * MAX_OMEGA
    }

    pub fn tick(&mut self, dt: f64) {
        self.omega *= (-FRICTION_PER_SEC * dt).exp();
        if self.omega.abs() < OMEGA_EPSILON {
            self.omega = 0.0;
        }
        self.angle = (self.angle + self.omega * dt).rem_euclid(TAU);

        if self.is_rainbow() {
            self.rainbow_phase += dt * RAINBOW_HUE_SPEED_DEG_PER_SEC / 360.0;
        } else {
            self.rainbow_phase = 0.0;
        }
    }

    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Left => {
                self.omega = (self.omega - ACCEL_STEP).clamp(-MAX_OMEGA, MAX_OMEGA);
            }
            KeyCode::Right => {
                self.omega = (self.omega + ACCEL_STEP).clamp(-MAX_OMEGA, MAX_OMEGA);
            }
            KeyCode::Char(' ') => {
                self.omega = 0.0;
            }
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
            }
            KeyCode::Esc if self.show_help => {
                self.show_help = false;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.running = false;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            _ => {}
        }
    }
}
