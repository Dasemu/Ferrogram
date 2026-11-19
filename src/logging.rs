//! `tracing` setup with a redaction safety net (architecture §6).
//!
//! Primary protection is type-level (`Phone`, `Redacted`, manual `Debug`
//! impls on `Message`/`Chat`/`Secret`). As a second line of defence every
//! formatted line passes through [`scrub`], which masks long digit runs
//! (phone numbers, login codes are short and never logged at all).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Max log size before rotation (architecture §6: 5 MB).
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Mask any run of 7+ digits (optionally `+`-prefixed, with spaces/dashes)
/// keeping only the last two digits.
pub fn scrub(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let mut j = i;
        let mut digits = 0;
        // Separators are only accepted in `+`-prefixed (international) numbers,
        // so timestamps like 2026-11-18 are left alone.
        let plus = chars[j] == '+';
        if plus {
            j += 1;
        }
        while j < chars.len()
            && (chars[j].is_ascii_digit()
                || (plus
                    && (chars[j] == ' ' || chars[j] == '-')
                    && j + 1 < chars.len()
                    && chars[j + 1].is_ascii_digit()
                    && digits > 0))
        {
            if chars[j].is_ascii_digit() {
                digits += 1;
            }
            j += 1;
        }
        if digits >= 7 {
            let run: String = chars[start..j]
                .iter()
                .filter(|c| c.is_ascii_digit())
                .collect();
            out.push_str("[num•••");
            out.push_str(&run[run.len() - 2..]);
            out.push(']');
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Writer that scrubs each write, then forwards to stderr and the log file.
#[derive(Clone)]
pub struct ScrubMakeWriter {
    file: Option<Arc<Mutex<RotatingFile>>>,
    sink: Option<Arc<Mutex<Vec<u8>>>>,
    stderr: bool,
}

impl ScrubMakeWriter {
    /// Capture into a buffer (tests).
    pub fn buffer(buf: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            file: None,
            sink: Some(buf),
            stderr: false,
        }
    }
}

pub struct ScrubWriter(ScrubMakeWriter);

impl Write for ScrubWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = scrub(&String::from_utf8_lossy(buf));
        if self.0.stderr {
            let _ = io::stderr().write_all(text.as_bytes());
        }
        if let Some(f) = &self.0.file
            && let Ok(mut f) = f.lock()
        {
            let _ = f.write_all(text.as_bytes());
        }
        if let Some(s) = &self.0.sink
            && let Ok(mut s) = s.lock()
        {
            s.extend_from_slice(text.as_bytes());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for ScrubMakeWriter {
    type Writer = ScrubWriter;
    fn make_writer(&'a self) -> Self::Writer {
        ScrubWriter(self.clone())
    }
}

/// Size-capped log file with a single `.1` backup.
pub struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl RotatingFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            written,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = std::fs::rename(&self.path, self.path.with_extension("1"));
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?,
        );
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > MAX_LOG_BYTES {
            self.rotate()?;
        }
        let n = match &mut self.file {
            Some(f) => f.write(buf)?,
            None => buf.len(),
        };
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

const DEFAULT_FILTER: &str =
    "info,grammers=warn,wgpu=warn,naga=warn,cosmic_text=warn,iced_winit=warn,winit=warn";

/// grammers' debug/trace output dumps raw TL (message bodies, phone numbers,
/// auth material) that our redaction cannot see. Even if the user sets
/// `RUST_LOG=trace`, those targets stay at `warn` unless explicitly opted in
/// with `FERROGRAM_PROTOCOL_DEBUG=1`.
pub fn clamp_protocol_logs(filter: EnvFilter, allow: bool) -> EnvFilter {
    if allow {
        return filter;
    }
    let mut f = filter;
    for target in [
        "grammers_client",
        "grammers_mtsender",
        "grammers_mtproto",
        "grammers_session",
        "grammers_crypto",
        "grammers_tl_types",
    ] {
        if let Ok(d) = format!("{target}=warn").parse() {
            f = f.add_directive(d);
        }
    }
    f
}

/// Initialise the global subscriber. Safe to call once.
pub fn init(log_path: Option<&Path>) {
    let file = log_path
        .and_then(|p| RotatingFile::open(p).ok())
        .map(|f| Arc::new(Mutex::new(f)));
    let writer = ScrubMakeWriter {
        file,
        sink: None,
        stderr: true,
    };
    let filter = clamp_protocol_logs(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER)),
        std::env::var("FERROGRAM_PROTOCOL_DEBUG").is_ok_and(|v| v == "1"),
    );
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_masks_phone_numbers() {
        assert_eq!(scrub("call +34 600 123 412 now"), "call [num•••12] now");
        assert_eq!(scrub("id 15550109999"), "id [num•••99]");
        assert_eq!(scrub("chat 12345 msg 42"), "chat 12345 msg 42");
        assert_eq!(scrub("ünïcode 1234567"), "ünïcode [num•••67]");
        assert_eq!(scrub("2026-11-18T23:20:30Z"), "2026-11-18T23:20:30Z");
    }

    #[test]
    fn user_rust_log_cannot_enable_protocol_traces() {
        let f = clamp_protocol_logs(EnvFilter::new("trace"), false).to_string();
        assert!(f.contains("grammers_mtsender=warn"), "{f}");
        let f = clamp_protocol_logs(EnvFilter::new("trace"), true).to_string();
        assert!(!f.contains("grammers_mtsender=warn"));
    }

    #[test]
    fn rotating_file_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let mut f = RotatingFile::open(&p).unwrap();
        f.written = MAX_LOG_BYTES - 1;
        f.write_all(b"hello").unwrap();
        assert!(p.with_extension("1").exists());
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
    }
}
