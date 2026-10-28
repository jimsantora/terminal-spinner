//! Terminal input.
//!
//! On Unix we read the tty ourselves instead of using crossterm's event
//! reader, because we also need replies crossterm doesn't understand: Kitty
//! graphics responses (APC) and visibility reports (DEC mode 2033).

use std::io;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, PartialEq)]
pub enum Input {
    Key(KeyCode, KeyModifiers),
    /// Visibility report: `true` if the terminal may be visible.
    Visible(bool),
    /// Kitty graphics response: image id and message (`OK` or an error).
    Graphics {
        id: u32,
        message: String,
    },
    /// Primary device attributes reply, used to end capability probes.
    DeviceAttributes,
}

#[cfg(not(unix))]
pub use fallback::Reader;
#[cfg(unix)]
pub use unix::Reader;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::AsRawFd;

    /// How long a lone ESC waits for the rest of a sequence before it
    /// counts as the Esc key.
    const ESC_TIMEOUT: Duration = Duration::from_millis(30);

    pub struct Reader {
        tty: File,
        buf: Vec<u8>,
    }

    impl Reader {
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                tty: File::open("/dev/tty")?,
                buf: Vec::new(),
            })
        }

        /// Wait up to `timeout` for input, then return everything parsed.
        pub fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Input>> {
            let mut events = Vec::new();
            if self.wait(timeout)? {
                self.fill()?;
            }
            loop {
                parse(&mut self.buf, &mut events);
                if self.buf != [0x1b] {
                    break;
                }
                // A lone ESC: either the Esc key, or the start of a sequence
                // whose remainder hasn't arrived yet.
                if self.wait(ESC_TIMEOUT)? {
                    self.fill()?;
                } else {
                    self.buf.clear();
                    events.push(Input::Key(KeyCode::Esc, KeyModifiers::NONE));
                    break;
                }
            }
            Ok(events)
        }

        /// Wait for the tty to become readable. Uses `select` rather than
        /// `poll`: on macOS, `poll` doesn't support `/dev/tty` and reports
        /// it readable immediately, which would make `read` block.
        fn wait(&self, timeout: Duration) -> io::Result<bool> {
            let fd = self.tty.as_raw_fd();
            let mut tv = libc::timeval {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_usec: timeout.subsec_micros() as libc::suseconds_t,
            };
            let ready = unsafe {
                let mut set: libc::fd_set = std::mem::zeroed();
                libc::FD_ZERO(&mut set);
                libc::FD_SET(fd, &mut set);
                libc::select(
                    fd + 1,
                    &mut set,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };
            match ready {
                -1 => {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        Ok(false)
                    } else {
                        Err(err)
                    }
                }
                n => Ok(n > 0),
            }
        }

        fn fill(&mut self) -> io::Result<()> {
            let mut chunk = [0u8; 64 * 1024];
            match self.tty.read(&mut chunk) {
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
            Ok(())
        }
    }
}

#[cfg(not(unix))]
mod fallback {
    use super::*;
    use crossterm::event::{self, Event, KeyEventKind};

    pub struct Reader;

    impl Reader {
        pub fn new() -> io::Result<Self> {
            Ok(Self)
        }

        pub fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Input>> {
            let mut events = Vec::new();
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        events.push(Input::Key(key.code, key.modifiers));
                    }
                }
            }
            Ok(events)
        }
    }
}

/// Parse as many complete inputs from `buf` as possible, leaving any
/// incomplete trailing sequence in place.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse(buf: &mut Vec<u8>, events: &mut Vec<Input>) {
    let mut i = 0;
    while i < buf.len() {
        let rest = &buf[i..];
        let consumed = match rest[0] {
            0x1b => match parse_escape(rest, events) {
                Some(n) => n,
                None => break,
            },
            0x03 => {
                events.push(Input::Key(KeyCode::Char('c'), KeyModifiers::CONTROL));
                1
            }
            b => {
                if b.is_ascii_graphic() || b == b' ' {
                    events.push(Input::Key(KeyCode::Char(b as char), KeyModifiers::NONE));
                }
                1
            }
        };
        i += consumed;
    }
    buf.drain(..i);
}

/// Returns the number of bytes consumed, or `None` if the sequence is
/// incomplete.
fn parse_escape(s: &[u8], events: &mut Vec<Input>) -> Option<usize> {
    match *s.get(1)? {
        b'[' => {
            // CSI: parameters, intermediates, then a final byte.
            let end = s[2..].iter().position(|b| (0x40..=0x7e).contains(b))? + 2;
            let params = &s[2..end];
            match (s[end], params) {
                (b'C', _) => events.push(Input::Key(KeyCode::Right, KeyModifiers::NONE)),
                (b'D', _) => events.push(Input::Key(KeyCode::Left, KeyModifiers::NONE)),
                (b'n', b"?999;1") => events.push(Input::Visible(true)),
                (b'n', b"?999;2") => events.push(Input::Visible(false)),
                (b'c', p) if p.starts_with(b"?") => events.push(Input::DeviceAttributes),
                _ => {}
            }
            Some(end + 1)
        }
        b'O' => {
            match *s.get(2)? {
                b'C' => events.push(Input::Key(KeyCode::Right, KeyModifiers::NONE)),
                b'D' => events.push(Input::Key(KeyCode::Left, KeyModifiers::NONE)),
                _ => {}
            }
            Some(3)
        }
        b'_' => {
            // APC, terminated by ST (ESC \).
            let end = s.windows(2).position(|w| w == b"\x1b\\")?;
            if let Some(g) = parse_graphics(&s[2..end]) {
                events.push(g);
            }
            Some(end + 2)
        }
        b'\x1b' => {
            events.push(Input::Key(KeyCode::Esc, KeyModifiers::NONE));
            Some(1)
        }
        // Alt+key and other two-byte sequences: ignore.
        _ => Some(2),
    }
}

/// Parse a Kitty graphics reply body like `Gi=31;OK`.
fn parse_graphics(body: &[u8]) -> Option<Input> {
    let body = std::str::from_utf8(body.strip_prefix(b"G")?).ok()?;
    let (keys, message) = body.split_once(';')?;
    let id = keys
        .split(',')
        .find_map(|kv| kv.strip_prefix("i="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some(Input::Graphics {
        id,
        message: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> (Vec<Input>, Vec<u8>) {
        let mut buf = bytes.to_vec();
        let mut events = Vec::new();
        parse(&mut buf, &mut events);
        (events, buf)
    }

    #[test]
    fn keys() {
        let (ev, rest) = parse_all(b"q? \x1b[C\x1bOD\x03");
        assert_eq!(
            ev,
            vec![
                Input::Key(KeyCode::Char('q'), KeyModifiers::NONE),
                Input::Key(KeyCode::Char('?'), KeyModifiers::NONE),
                Input::Key(KeyCode::Char(' '), KeyModifiers::NONE),
                Input::Key(KeyCode::Right, KeyModifiers::NONE),
                Input::Key(KeyCode::Left, KeyModifiers::NONE),
                Input::Key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ]
        );
        assert!(rest.is_empty());
    }

    #[test]
    fn replies() {
        let (ev, rest) =
            parse_all(b"\x1b_Gi=7;OK\x1b\\\x1b[?999;2n\x1b[?62;22c\x1b_Gi=9,p=1;ENOENT:x\x1b\\");
        assert_eq!(
            ev,
            vec![
                Input::Graphics {
                    id: 7,
                    message: "OK".into()
                },
                Input::Visible(false),
                Input::DeviceAttributes,
                Input::Graphics {
                    id: 9,
                    message: "ENOENT:x".into()
                },
            ]
        );
        assert!(rest.is_empty());
    }

    #[test]
    fn incomplete_sequences_wait() {
        let (ev, rest) = parse_all(b"q\x1b_Gi=7;O");
        assert_eq!(ev.len(), 1);
        assert_eq!(rest, b"\x1b_Gi=7;O");
        let (_, rest) = parse_all(b"\x1b");
        assert_eq!(rest, b"\x1b");
    }
}
