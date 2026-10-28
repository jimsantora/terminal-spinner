//! Kitty graphics renderer.
//!
//! The spinner looks identical every 120°, so each "set" of pre-rendered
//! frames covers one third of a turn. Sets differ by motion blur level and
//! (in rainbow mode) hue. Every set is uploaded once as a single animated
//! Kitty image; after that, showing a frame costs a ~25 byte `a=a,c=N`
//! command instead of repainting the whole terminal.
//!
//! Frames are rendered by background workers. Whatever the spinner needs
//! right now jumps the queue; everything else is pre-warmed behind it.
//! Until a frame arrives we show the closest one we already have.

use std::collections::{HashMap, HashSet, VecDeque};
use std::f64::consts::TAU;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use flate2::{write::ZlibEncoder, Compression};

use crate::input::{Input, Reader};
use crate::render::{render_rgba, Look, View};

/// One third of a turn: the spinner's rotational symmetry.
const PERIOD: f64 = TAU / 3.0;

/// Motion blur levels. A level is used once the spinner moves at least
/// `min_arc` degrees per frame, and is rendered with `arc` degrees of blur.
/// Blurrier levels need fewer frames and less resolution.
struct Level {
    min_arc: f64,
    arc: f64,
    buckets: usize,
    max_side: f64,
}

const LEVELS: [Level; 6] = [
    Level {
        min_arc: 0.0,
        arc: 0.0,
        buckets: 120,
        max_side: 512.0,
    },
    Level {
        min_arc: 1.5,
        arc: 2.5,
        buckets: 60,
        max_side: 400.0,
    },
    Level {
        min_arc: 3.5,
        arc: 4.5,
        buckets: 40,
        max_side: 352.0,
    },
    Level {
        min_arc: 5.5,
        arc: 7.0,
        buckets: 30,
        max_side: 320.0,
    },
    Level {
        min_arc: 8.5,
        arc: 10.0,
        buckets: 24,
        max_side: 288.0,
    },
    Level {
        min_arc: 11.5,
        arc: 15.0,
        buckets: 24,
        max_side: 256.0,
    },
];
/// Rainbow mode only happens at top speed, so it always uses the blurriest
/// level, at lower resolution since it's heavily blurred anyway.
const RAINBOW_LEVEL: u8 = 5;
const RAINBOW_MAX_SIDE: f64 = 224.0;
const RAINBOW_HUES: u8 = 16;
const RAINBOW_BUCKETS: usize = 12;

/// Frame rate the blur levels are tuned for (independent of the tick rate).
const FPS: f64 = 60.0;
/// Upload at most this much image data per tick, so pre-warming never
/// stalls the animation.
const UPLOAD_BUDGET: usize = 512 << 10;
/// Kitty's protocol limit for one escape code's payload chunk.
const CHUNK: usize = 4096;
/// Image ids we use; high enough to stay clear of other programs.
const FIRST_IMAGE_ID: u32 = 0x5350_0000;
const PROBE_IMAGE_ID: u32 = FIRST_IMAGE_ID - 1;
/// Give up on Kitty graphics after this many terminal errors.
const MAX_FAILURES: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SetKey {
    level: u8,
    hue: Option<u8>,
}

impl SetKey {
    fn buckets(self) -> usize {
        if self.hue.is_some() {
            RAINBOW_BUCKETS
        } else {
            LEVELS[self.level as usize].buckets
        }
    }

    fn max_side(self) -> f64 {
        if self.hue.is_some() {
            RAINBOW_MAX_SIDE
        } else {
            LEVELS[self.level as usize].max_side
        }
    }

    fn bucket_angle(self, bucket: usize) -> f64 {
        bucket as f64 / self.buckets() as f64 * PERIOD
    }

    fn look(self, bucket: usize) -> Look {
        Look {
            angle: self.bucket_angle(bucket),
            blur: LEVELS[self.level as usize].arc.to_radians(),
            hue: self.hue.map(|h| h as f64 * 360.0 / RAINBOW_HUES as f64),
            shadow: true,
        }
    }

    /// Every set, most useful first.
    fn all() -> impl Iterator<Item = SetKey> {
        let plain = (0..LEVELS.len() as u8).map(|level| SetKey { level, hue: None });
        let rainbow = (0..RAINBOW_HUES).map(|h| SetKey {
            level: RAINBOW_LEVEL,
            hue: Some(h),
        });
        plain.chain(rainbow)
    }
}

/// Where the image goes on screen, derived from the terminal's pixel size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    px_w: f64,
    px_h: f64,
    cell_w: f64,
    cell_h: f64,
    col0: u16,
    row0: u16,
    cols: u16,
    rows: u16,
}

impl Geometry {
    pub fn query() -> Option<Self> {
        let ws = crossterm::terminal::window_size().ok()?;
        if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
            return None;
        }
        let (px_w, px_h) = (ws.width as f64, ws.height as f64);
        let cell_w = px_w / ws.columns as f64;
        let cell_h = px_h / ws.rows as f64;
        // A centered square of cells big enough for the spinner and its shadow.
        let side = px_w.min(px_h);
        let cols = ((side / cell_w).ceil() as u16).min(ws.columns);
        let rows = ((side / cell_h).ceil() as u16).min(ws.rows);
        Some(Self {
            px_w,
            px_h,
            cell_w,
            cell_h,
            col0: (ws.columns - cols) / 2,
            row0: (ws.rows - rows) / 2,
            cols,
            rows,
        })
    }

    /// Image size for a set, matching the placement's aspect ratio so the
    /// terminal never distorts it.
    fn image(&self, max_side: f64) -> (u32, u32, View) {
        let rect_w = self.cols as f64 * self.cell_w;
        let rect_h = self.rows as f64 * self.cell_h;
        let q = (max_side / rect_w.max(rect_h)).min(1.0);
        let view = View {
            cx: (self.px_w / 2.0 - self.col0 as f64 * self.cell_w) * q,
            cy: (self.px_h / 2.0 - self.row0 as f64 * self.cell_h) * q,
            scale: self.px_w.min(self.px_h) / 2.0 * 0.92 * q,
        };
        let w = (rect_w * q).round().max(1.0) as u32;
        let h = (rect_h * q).round().max(1.0) as u32;
        (w, h, view)
    }
}

/// Ask the terminal whether it supports Kitty graphics *animation*: upload a
/// 1×1 image, add a frame to it, and see if both succeed before the reply to
/// a device attributes request. Terminals without animation (like Ghostty
/// before 1.4) reject the frame.
pub fn detect(reader: &mut Reader, out: &mut impl Write) -> io::Result<bool> {
    let id = PROBE_IMAGE_ID;
    write!(
        out,
        "\x1b_Ga=t,i={id},s=1,v=1,f=24,t=d;AAAA\x1b\\\
         \x1b_Ga=f,i={id},s=1,v=1,f=24;AAAA\x1b\\\
         \x1b[c"
    )?;
    out.flush()?;

    let deadline = Instant::now() + Duration::from_millis(1000);
    let mut oks = 0;
    'wait: while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        for event in reader.poll(left)? {
            match event {
                Input::Graphics { id: got, message } if got == id && message == "OK" => oks += 1,
                Input::DeviceAttributes => break 'wait,
                _ => {}
            }
        }
    }

    write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
    out.flush()?;
    Ok(oks == 2)
}

struct Job {
    gen: u64,
    key: SetKey,
    bucket: usize,
    urgent: bool,
}

struct Encoded {
    gen: u64,
    key: SetKey,
    bucket: usize,
    width: u32,
    height: u32,
    /// Base64 of zlib-compressed RGBA.
    payload: String,
}

#[derive(Default)]
struct Queue {
    gen: u64,
    geometry: Option<Geometry>,
    urgent: VecDeque<Job>,
    background: VecDeque<Job>,
    /// Frames a worker has started on (this generation).
    claimed: HashSet<(SetKey, usize)>,
    shutdown: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

fn worker(shared: Shared, tx: Sender<(Encoded, bool)>) {
    loop {
        let (job, geometry) = {
            let (lock, cvar) = &*shared;
            let mut q = lock.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                let job = q.urgent.pop_front().or_else(|| q.background.pop_front());
                match job {
                    Some(job) if job.gen != q.gen => continue,
                    Some(job) if !q.claimed.insert((job.key, job.bucket)) => continue,
                    Some(job) => break (job, q.geometry.unwrap()),
                    None => q = cvar.wait(q).unwrap(),
                }
            }
        };

        let (width, height, view) = geometry.image(job.key.max_side());
        let rgba = render_rgba(width, height, view, job.key.look(job.bucket));
        let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
        z.write_all(&rgba).unwrap();
        let payload = base64::engine::general_purpose::STANDARD.encode(z.finish().unwrap());

        let done = Encoded {
            gen: job.gen,
            key: job.key,
            bucket: job.bucket,
            width,
            height,
            payload,
        };
        if tx.send((done, job.urgent)).is_err() {
            return;
        }
    }
}

struct SetState {
    id: u32,
    /// Bucket -> 1-based frame number within the image.
    frames: Vec<Option<u32>>,
    count: u32,
    /// Frame last selected with `a=a,c=`.
    current: u32,
}

/// What the spinner looks like this tick.
pub struct Pose {
    pub angle: f64,
    pub omega: f64,
    pub rainbow: bool,
    pub rainbow_phase: f64,
}

pub struct KittyRenderer {
    shared: Shared,
    rx: Receiver<(Encoded, bool)>,
    workers: Vec<JoinHandle<()>>,
    gen: u64,
    geometry: Option<Geometry>,
    sets: HashMap<SetKey, SetState>,
    next_id: u32,
    /// Rendered frames waiting to be uploaded.
    ready: VecDeque<Encoded>,
    requested: HashSet<(SetKey, usize)>,
    /// Image that currently has a placement on screen.
    placed: Option<u32>,
    failures: u32,
    use_transient: bool,
}

impl KittyRenderer {
    pub fn new() -> Self {
        let shared: Shared = Arc::default();
        let (tx, rx) = mpsc::channel();
        let n = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .clamp(1, 6);
        let workers = (0..n)
            .map(|_| {
                let (shared, tx) = (shared.clone(), tx.clone());
                std::thread::spawn(move || worker(shared, tx))
            })
            .collect();
        Self {
            shared,
            rx,
            workers,
            gen: 0,
            geometry: None,
            sets: HashMap::new(),
            next_id: FIRST_IMAGE_ID,
            ready: VecDeque::new(),
            requested: HashSet::new(),
            placed: None,
            failures: 0,
            use_transient: true,
        }
    }

    /// True once the terminal has rejected too many of our commands.
    pub fn failed(&self) -> bool {
        self.failures >= MAX_FAILURES
    }

    /// Re-read the terminal size; on change, drop every image and start over.
    /// Returns false if the terminal doesn't report its pixel size.
    pub fn sync_geometry(&mut self, out: &mut Vec<u8>) -> bool {
        let geometry = Geometry::query();
        if geometry != self.geometry {
            self.geometry = geometry;
            self.reset(out);
        }
        self.geometry.is_some()
    }

    fn reset(&mut self, out: &mut Vec<u8>) {
        self.delete_all(out);
        self.gen += 1;
        self.ready.clear();
        self.requested.clear();

        let (lock, cvar) = &*self.shared;
        let mut q = lock.lock().unwrap();
        q.gen = self.gen;
        q.geometry = self.geometry;
        q.urgent.clear();
        q.background.clear();
        q.claimed.clear();
        if self.geometry.is_some() {
            for key in SetKey::all() {
                for bucket in 0..key.buckets() {
                    q.background.push_back(Job {
                        gen: self.gen,
                        key,
                        bucket,
                        urgent: false,
                    });
                }
            }
        }
        cvar.notify_all();
    }

    /// Upload new frames and show the right one for `pose`, or hide the
    /// spinner (e.g. while the help popup is open).
    pub fn draw(&mut self, pose: &Pose, hidden: bool, out: &mut Vec<u8>) {
        self.upload(out);
        if hidden {
            self.hide(out);
            return;
        }

        let key = desired_set(pose);
        let n = key.buckets();
        let bucket = (pose.angle.rem_euclid(PERIOD) / PERIOD * n as f64).round() as usize % n;
        self.request(key, bucket);

        if let Some((id, frame)) = self.pick(key, pose.angle) {
            self.show(id, frame, out);
        }
    }

    /// Keep uploading pre-warmed frames while nothing is being drawn.
    pub fn idle(&mut self, out: &mut Vec<u8>) {
        self.upload(out);
    }

    /// Handle a Kitty graphics reply. We only ask for errors, so anything
    /// that isn't `OK` means the terminal lost or rejected an image (for
    /// example, evicted under storage pressure): forget it and re-render.
    pub fn on_response(&mut self, id: u32, message: &str, out: &mut Vec<u8>) {
        if message == "OK" {
            return;
        }
        let Some(&key) = self.sets.iter().find(|(_, s)| s.id == id).map(|(k, _)| k) else {
            return;
        };
        self.failures += 1;
        if key.hue.is_some() {
            // Maybe the terminal doesn't understand the transient hint.
            self.use_transient = false;
        }
        write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\").unwrap();
        self.sets.remove(&key);
        if self.placed == Some(id) {
            self.placed = None;
        }
        self.requested.retain(|(k, _)| *k != key);
        self.shared
            .0
            .lock()
            .unwrap()
            .claimed
            .retain(|(k, _)| *k != key);
    }

    /// Delete every image we created and stop the workers.
    pub fn shutdown(mut self, out: &mut Vec<u8>) {
        self.delete_all(out);
        self.shared.0.lock().unwrap().shutdown = true;
        self.shared.1.notify_all();
        drop(self.rx);
        for w in self.workers {
            let _ = w.join();
        }
    }

    fn delete_all(&mut self, out: &mut Vec<u8>) {
        for set in self.sets.values() {
            write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", set.id).unwrap();
        }
        self.sets.clear();
        self.placed = None;
    }

    fn request(&mut self, key: SetKey, bucket: usize) {
        let have = self
            .sets
            .get(&key)
            .is_some_and(|s| s.frames[bucket].is_some());
        if have || !self.requested.insert((key, bucket)) {
            return;
        }
        let (lock, cvar) = &*self.shared;
        lock.lock().unwrap().urgent.push_back(Job {
            gen: self.gen,
            key,
            bucket,
            urgent: true,
        });
        cvar.notify_one();
    }

    fn upload(&mut self, out: &mut Vec<u8>) {
        while let Ok((frame, urgent)) = self.rx.try_recv() {
            if frame.gen != self.gen {
                continue;
            }
            if urgent {
                self.ready.push_front(frame);
            } else {
                self.ready.push_back(frame);
            }
        }

        let start = out.len();
        while out.len() - start < UPLOAD_BUDGET {
            let Some(frame) = self.ready.pop_front() else {
                break;
            };
            self.transmit(frame, out);
        }
    }

    fn transmit(&mut self, f: Encoded, out: &mut Vec<u8>) {
        let next_id = &mut self.next_id;
        let set = self.sets.entry(f.key).or_insert_with(|| {
            *next_id += 1;
            SetState {
                id: *next_id,
                frames: vec![None; f.key.buckets()],
                count: 0,
                current: 1,
            }
        });
        if set.frames[f.bucket].is_some() {
            return;
        }

        let (id, w, h) = (set.id, f.width, f.height);
        // The first frame creates the image; the rest are added to it.
        let (first, cont) = if set.count == 0 {
            let hint = if f.key.hue.is_some() && self.use_transient {
                ",N=1"
            } else {
                ""
            };
            (format!("a=t,i={id},f=32,s={w},v={h},o=z,q=1{hint}"), "")
        } else {
            (format!("a=f,i={id},f=32,s={w},v={h},o=z,X=1,q=1"), "a=f,")
        };

        let bytes = f.payload.as_bytes();
        let mut chunks = bytes.chunks(CHUNK).peekable();
        let mut head = true;
        while let Some(chunk) = chunks.next() {
            let more = chunks.peek().is_some() as u8;
            let keys = if head { first.as_str() } else { cont };
            let sep = if head { "," } else { "" };
            write!(out, "\x1b_G{keys}{sep}m={more}").unwrap();
            if !head {
                out.extend_from_slice(b",q=1");
            }
            out.push(b';');
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
            head = false;
        }

        set.count += 1;
        set.frames[f.bucket] = Some(set.count);
    }

    /// The best frame we have for `key` at `angle`: the exact set if it has
    /// something close, otherwise the nearest blur level (same hue first).
    fn pick(&self, key: SetKey, angle: f64) -> Option<(u32, u32)> {
        let mut candidates: Vec<SetKey> = SetKey::all().collect();
        candidates.sort_by_key(|k| {
            (
                k.hue != key.hue,
                (k.level as i32 - key.level as i32).abs(),
                k.hue.map_or(0, |h| {
                    (h as i32 - key.hue.unwrap_or(0) as i32).rem_euclid(RAINBOW_HUES as i32)
                }),
            )
        });

        let mut best: Option<(f64, u32, u32)> = None;
        for k in candidates {
            let Some(set) = self.sets.get(&k) else {
                continue;
            };
            let span = PERIOD / k.buckets() as f64;
            let nearest = set
                .frames
                .iter()
                .enumerate()
                .filter_map(|(b, f)| f.map(|f| (angle_diff(k.bucket_angle(b), angle), f)))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((err, frame)) = nearest {
                if err <= span {
                    return Some((set.id, frame));
                }
                if !best.is_some_and(|b| err >= b.0) {
                    best = Some((err, set.id, frame));
                }
            }
        }
        best.map(|(_, id, frame)| (id, frame))
    }

    fn show(&mut self, id: u32, frame: u32, out: &mut Vec<u8>) {
        let Some(set) = self.sets.values_mut().find(|s| s.id == id) else {
            return;
        };
        if set.current != frame {
            write!(out, "\x1b_Ga=a,i={id},c={frame},q=1\x1b\\").unwrap();
            set.current = frame;
        }
        if self.placed != Some(id) {
            let g = self.geometry.unwrap();
            write!(
                out,
                "\x1b[{};{}H\x1b_Ga=p,i={id},p=1,c={},r={},C=1,z=-1,q=1\x1b\\",
                g.row0 + 1,
                g.col0 + 1,
                g.cols,
                g.rows
            )
            .unwrap();
            self.hide(out);
            self.placed = Some(id);
        }
    }

    fn hide(&mut self, out: &mut Vec<u8>) {
        if let Some(old) = self.placed.take() {
            // Lowercase `d=i`: remove the placement but keep the image.
            write!(out, "\x1b_Ga=d,d=i,i={old},q=2\x1b\\").unwrap();
        }
    }
}

fn desired_set(pose: &Pose) -> SetKey {
    if pose.rainbow {
        let hue = (pose.rainbow_phase.rem_euclid(1.0) * RAINBOW_HUES as f64) as u8 % RAINBOW_HUES;
        return SetKey {
            level: RAINBOW_LEVEL,
            hue: Some(hue),
        };
    }
    let arc = pose.omega.abs().to_degrees() / FPS;
    let level = LEVELS.iter().rposition(|l| arc >= l.min_arc).unwrap_or(0);
    SetKey {
        level: level as u8,
        hue: None,
    }
}

/// Angular distance within one symmetry period.
fn angle_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(PERIOD);
    d.min(PERIOD - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_levels_follow_speed() {
        let pose = |omega: f64, rainbow| Pose {
            angle: 0.0,
            omega,
            rainbow,
            rainbow_phase: 0.5,
        };
        assert_eq!(desired_set(&pose(0.5, false)).level, 0);
        assert_eq!(desired_set(&pose(2.0, false)).level, 1);
        assert_eq!(desired_set(&pose(-4.0, false)).level, 2);
        assert_eq!(desired_set(&pose(8.0, false)).level, 3);
        assert_eq!(desired_set(&pose(10.0, false)).level, 4);
        assert_eq!(desired_set(&pose(14.0, false)).level, 5);
        assert_eq!(
            desired_set(&pose(17.0, true)),
            SetKey {
                level: RAINBOW_LEVEL,
                hue: Some(8)
            }
        );
    }

    #[test]
    fn transmit_chunks_and_numbers_frames() {
        let mut r = KittyRenderer::new();
        r.geometry = Some(Geometry {
            px_w: 800.0,
            px_h: 600.0,
            cell_w: 10.0,
            cell_h: 20.0,
            col0: 10,
            row0: 0,
            cols: 60,
            rows: 30,
        });
        let key = SetKey {
            level: 0,
            hue: None,
        };
        let frame = |bucket, len| Encoded {
            gen: 0,
            key,
            bucket,
            width: 2,
            height: 2,
            payload: "A".repeat(len),
        };
        let mut out = Vec::new();
        r.transmit(frame(5, CHUNK + 10), &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with(&format!(
            "\x1b_Ga=t,i={},f=32,s=2,v=2,o=z,q=1,m=1;",
            FIRST_IMAGE_ID + 1
        )));
        assert!(s.contains("\x1b\\\x1b_Gm=0,q=1;AAAAAAAAAA\x1b\\"));

        let mut out = Vec::new();
        r.transmit(frame(9, 4), &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=f,"));

        assert_eq!(
            r.pick(key, key.bucket_angle(9)),
            Some((FIRST_IMAGE_ID + 1, 2))
        );
        assert_eq!(
            r.pick(key, key.bucket_angle(5)),
            Some((FIRST_IMAGE_ID + 1, 1))
        );
        r.shutdown(&mut Vec::new());
    }
}
