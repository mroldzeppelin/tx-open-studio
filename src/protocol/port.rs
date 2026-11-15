//! The serial port: discovery, opening + handshake, the framed link.

use super::*;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::path::Path;
use std::sync::MutexGuard;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use serial2::SerialPort;

pub(crate) const BAUD_RATE: u32 = 115_200;

pub(crate) const IK_VENDOR_ID: &str = "1963";

pub(crate) const READ_POLL: Duration = Duration::from_millis(50);

pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Serializes all access to the pedal's serial port.
pub(crate) static PORT_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn port_lock() -> MutexGuard<'static, ()> {
    PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct Link {
    port: SerialPort,
    /// Bytes of the frame being assembled (after an opening flag).
    pending: Vec<u8>,
    in_frame: bool,
    /// Bytes read from the port but not yet consumed.
    rx: Vec<u8>,
    rx_pos: usize,
}

impl Link {
    pub(crate) fn open(path: &Path) -> Result<Self, OpenFailure> {
        let mut port = SerialPort::open(path, BAUD_RATE).map_err(|e| {
            // Only a real EACCES (no rights on the node), not EPERM, which
            // Rust also files under PermissionDenied.
            if e.raw_os_error() == Some(EACCES) {
                OpenFailure::NoAccess(path.to_path_buf())
            } else {
                OpenFailure::Other(format!("open {}: {e}", path.display()))
            }
        })?;
        port.set_read_timeout(READ_POLL)
            .and_then(|_| port.set_write_timeout(Duration::from_secs(1)))
            .map_err(|e| format!("configure {}: {e}", path.display()))?;
        // Some CDC devices only talk once DTR is asserted; ignore failures.
        let _ = port.set_dtr(true);
        let _ = port.set_rts(true);
        let _ = port.discard_buffers();
        Ok(Self { port, pending: Vec::new(), in_frame: false, rx: Vec::with_capacity(4096), rx_pos: 0 })
    }

    pub(crate) fn send(&self, payload: &[u8]) -> Result<(), String> {
        self.port
            .write_all(&build_frame(payload))
            .map_err(|e| format!("write: {e}"))
    }

    /// Returns the next valid frame payload, or None when `timeout` (capped
    /// by `deadline`) elapses. Frames with a bad CRC are dropped.
    pub(crate) fn read_frame(&mut self, timeout: Duration, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        let until = (Instant::now() + timeout).min(deadline);
        let mut chunk = [0u8; 4096];
        loop {
            while self.rx_pos < self.rx.len() {
                let b = self.rx[self.rx_pos];
                self.rx_pos += 1;
                if b == FLAG {
                    // A flag both closes the current frame and may open the
                    // next one; empty bodies (back-to-back flags) are skipped.
                    if self.in_frame && !self.pending.is_empty() {
                        let body = std::mem::take(&mut self.pending);
                        if let Some(payload) = decode_frame(&body) {
                            return Ok(Some(payload));
                        }
                    }
                    self.in_frame = true;
                } else if self.in_frame {
                    if self.pending.len() >= MAX_FRAME_LEN {
                        self.pending.clear();
                        self.in_frame = false;
                    } else {
                        self.pending.push(b);
                    }
                }
            }
            self.rx.clear();
            self.rx_pos = 0;

            if Instant::now() >= until {
                return Ok(None);
            }
            match self.port.read(&mut chunk) {
                // poll() said readable but nothing came: hang-up (unplugged).
                Ok(0) => return Err("device disconnected".into()),
                Ok(n) => self.rx.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }
}

/// USB vendor ID of the device behind /dev/ttyACMx, read from sysfs.
pub(crate) fn usb_vendor_id(dev_name: &str) -> Option<String> {
    let iface = fs::canonicalize(format!("/sys/class/tty/{dev_name}/device")).ok()?;
    let vid = fs::read_to_string(iface.parent()?.join("idVendor")).ok()?;
    Some(vid.trim().to_lowercase())
}

/// /dev/ttyACM* ports, IK Multimedia devices first. Ports whose vendor is
/// known to be something else are skipped; unknown ones are kept as a
/// fallback (e.g. when sysfs is unavailable).
pub(crate) fn candidate_ports() -> Vec<PathBuf> {
    let mut ik = Vec::new();
    let mut unknown = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("ttyACM") {
                continue;
            }
            match usb_vendor_id(&name).as_deref() {
                Some(IK_VENDOR_ID) => ik.push(entry.path()),
                Some(_) => {}
                None => unknown.push(entry.path()),
            }
        }
    }
    ik.sort();
    unknown.sort();
    ik.extend(unknown);
    ik
}

/// Cheap presence check for the reconnect poll: is there a port a session
/// could try (same discovery as `Pedal::start`)? Opens nothing.
pub fn port_available() -> bool {
    !candidate_ports().is_empty()
}

/// Linux errno for "permission denied" on open (no libc dependency).
pub(crate) const EACCES: i32 = 13;

/// Why no session could be opened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenFailure {
    /// The device node exists but may not be opened (EACCES): typically
    /// the udev rule granting access is not installed.
    NoAccess(PathBuf),
    Other(String),
}

impl From<String> for OpenFailure {
    fn from(e: String) -> Self {
        OpenFailure::Other(e)
    }
}

impl OpenFailure {
    pub(crate) fn message(&self) -> String {
        match self {
            OpenFailure::NoAccess(path) => format!("open {}: permission denied", path.display()),
            OpenFailure::Other(e) => e.clone(),
        }
    }
}

/// Combines the per-port failures of one open attempt: if none of the
/// ports could be used and one of them refused access, that is the
/// actionable problem to report; otherwise all messages.
pub(crate) fn combine_failures(failures: Vec<OpenFailure>) -> OpenFailure {
    if let Some(i) = failures.iter().position(|f| matches!(f, OpenFailure::NoAccess(_))) {
        return failures.into_iter().nth(i).expect("index from position");
    }
    OpenFailure::Other(failures.iter().map(OpenFailure::message).collect::<Vec<_>>().join("; "))
}

/// Opens the first candidate port that answers the handshake. Also returns
/// the state reply (if one arrived) for the current preset.
pub(crate) fn open_any() -> Result<(Link, PathBuf, Option<Vec<u8>>), OpenFailure> {
    let ports = candidate_ports();
    if ports.is_empty() {
        return Err(OpenFailure::Other("no /dev/ttyACM* port found".into()));
    }
    let mut failures = Vec::new();
    for path in ports {
        match open_pedal(&path, Instant::now() + HANDSHAKE_TIMEOUT * 2) {
            Ok((link, state)) => return Ok((link, path, state)),
            Err(OpenFailure::Other(e)) => failures.push(OpenFailure::Other(format!("{}: {e}", path.display()))),
            Err(denied) => failures.push(denied),
        }
    }
    Err(combine_failures(failures))
}

pub(crate) fn open_pedal(path: &Path, deadline: Instant) -> Result<(Link, Option<Vec<u8>>), OpenFailure> {
    let mut link = Link::open(path)?;

    // Handshake. Replies are not strictly required, but the pedal expects
    // hello + state before serving preset details.
    link.send(&HELLO_CMD)?;
    let hello = link.read_frame(HANDSHAKE_TIMEOUT, deadline)?;
    if hello.is_none() {
        return Err(OpenFailure::Other("no reply to hello (not a TONEX Pedal?)".into()));
    }
    link.send(&REQUEST_STATE_CMD)?;
    // Keep the state reply (type 0x0201): it carries the current preset.
    let until = (Instant::now() + HANDSHAKE_TIMEOUT).min(deadline);
    let mut state = None;
    while let Some(payload) = link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
        if frame_type(&payload) == Some(TYPE_STATE) {
            state = Some(payload);
            break;
        }
    }
    Ok((link, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_failures_report_permission_problem_first() {
        let denied = PathBuf::from("/dev/ttyACM1");
        let f = combine_failures(vec![
            OpenFailure::Other("/dev/ttyACM0: no reply to hello".into()),
            OpenFailure::NoAccess(denied.clone()),
        ]);
        assert_eq!(f, OpenFailure::NoAccess(denied));
        assert_eq!(f.message(), "open /dev/ttyACM1: permission denied");

        let f = combine_failures(vec![OpenFailure::Other("a".into()), OpenFailure::Other("b".into())]);
        assert_eq!(f, OpenFailure::Other("a; b".into()));
    }
}
