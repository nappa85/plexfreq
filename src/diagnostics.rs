//! Timestamp and rotate framework/Rust stdout/stderr independently of the GUI loop.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, fs::PermissionsExt, io::FromRawFd},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

static ENABLED: AtomicBool = AtomicBool::new(false);
const MAX_LINE: usize = 16 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const KEEP_DAYS: usize = 7;

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}
pub(crate) fn event(message: std::fmt::Arguments<'_>) {
    if enabled() {
        eprintln!("PlexFreq {message}");
    }
}

fn local_stamp() -> (String, String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as libc::time_t;
    // localtime_r uses the actual local calendar, including DST/time-zone changes.
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    let date = format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    );
    let offset = tm.tm_gmtoff;
    let stamp = format!(
        "{date}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        now.subsec_millis(),
        if offset < 0 { '-' } else { '+' },
        offset.abs() / 3600,
        offset.abs() % 3600 / 60
    );
    (date, stamp)
}

// Framework diagnostics are not inherently redacted. Remove complete URLs (also
// local cache paths) and token assignments before they reach persistent storage.
pub(crate) fn redact(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        let start = [
            "http://",
            "https://",
            "file://",
            "x-plex-token",
            "authorization:",
        ]
        .iter()
        .filter_map(|key| lower.find(key).map(|index| (index, *key)))
        .min_by_key(|(index, _)| *index);
        let Some((index, key)) = start else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..index]);
        let tail = &rest[index + key.len()..];
        let end = if key.ends_with("://") {
            tail.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
                .unwrap_or(tail.len())
        } else {
            // Discard the rest of a token/header-bearing line rather than guess
            // at escaping/quoting conventions used by third-party libraries.
            tail.len()
        };
        result.push_str("[redacted]");
        rest = &tail[end..];
    }
    result
}

struct DailyWriter {
    directory: PathBuf,
    date: String,
    file: Option<File>,
    size: u64,
    started: Instant,
}
impl DailyWriter {
    fn new(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            directory,
            date: String::new(),
            file: None,
            size: 0,
            started: Instant::now(),
        })
    }
    fn open(&mut self, date: &str) -> io::Result<()> {
        let path = self.directory.join(format!("plexfreq-{date}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        self.date = date.into();
        self.prune();
        Ok(())
    }
    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        let mut dates: Vec<String> = entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let date = name.strip_prefix("plexfreq-")?.strip_suffix(".log")?;
                (date.len() == 10
                    && date.bytes().enumerate().all(|(i, b)| {
                        if i == 4 || i == 7 {
                            b == b'-'
                        } else {
                            b.is_ascii_digit()
                        }
                    }))
                .then(|| date.to_owned())
            })
            .collect();
        dates.sort();
        // Preserve the current date even after a backwards clock correction.
        dates.retain(|date| date != &self.date);
        let remove = dates.len().saturating_sub(KEEP_DAYS - 1);
        for date in dates.into_iter().take(remove) {
            for suffix in ["log", "log.1"] {
                let _ = fs::remove_file(self.directory.join(format!("plexfreq-{date}.{suffix}")));
            }
        }
    }
    fn line(&mut self, date: &str, stamp: &str, bytes: &[u8]) -> io::Result<()> {
        if self.date != date || self.file.is_none() {
            self.open(date)?;
        }
        let text = redact(&String::from_utf8_lossy(bytes));
        let line = format!(
            "[{stamp} +{:.3}s pid={}] {text}\n",
            self.started.elapsed().as_secs_f64(),
            std::process::id()
        );
        if self.size + line.len() as u64 > MAX_FILE {
            self.file.take();
            fs::rename(
                self.directory.join(format!("plexfreq-{date}.log")),
                self.directory.join(format!("plexfreq-{date}.log.1")),
            )?;
            self.open(date)?;
        }
        self.file.as_mut().unwrap().write_all(line.as_bytes())?;
        self.size += line.len() as u64;
        Ok(())
    }
}

pub struct Log {
    saved: [File; 2],
    worker: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}
impl Log {
    pub fn install(directory: PathBuf) -> io::Result<Self> {
        if ENABLED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "logging already installed",
            ));
        }
        let result = Self::start(directory);
        if result.is_err() {
            ENABLED.store(false, Ordering::SeqCst);
        }
        result
    }
    fn start(directory: PathBuf) -> io::Result<Self> {
        let mut writer = DailyWriter::new(directory)?;
        let (date, stamp) = local_stamp();
        writer.line(
            &date,
            &stamp,
            b"=== PlexFreq started; diagnostics v2; daily rotation; 8 MiB x2/day, 7 dates ===",
        )?;
        fn duplicate(fd: i32) -> io::Result<File> {
            let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_fd(duplicate) })
        }
        let saved = [duplicate(1)?, duplicate(2)?];
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let reader = unsafe { File::from_raw_fd(fds[0]) };
        let write = unsafe { File::from_raw_fd(fds[1]) };
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("plexfreq-log".into())
            .spawn(move || capture(reader, writer, stopping))?;
        use std::os::fd::AsRawFd;
        unsafe {
            libc::fflush(std::ptr::null_mut());
        }
        for fd in [1, 2] {
            if unsafe { libc::dup2(write.as_raw_fd(), fd) } < 0 {
                for (index, original) in saved.iter().enumerate() {
                    unsafe {
                        libc::dup2(original.as_raw_fd(), index as i32 + 1);
                    }
                }
                drop(write);
                let _ = worker.join();
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            saved,
            worker: Some(worker),
            stop,
        })
    }
}
impl Drop for Log {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        event(format_args!("=== shutdown; draining diagnostics ==="));
        unsafe {
            libc::fflush(std::ptr::null_mut());
        }
        for (index, original) in self.saved.iter().enumerate() {
            unsafe {
                libc::dup2(original.as_raw_fd(), index as i32 + 1);
            }
        }
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        ENABLED.store(false, Ordering::SeqCst);
    }
}
fn capture(mut reader: File, mut writer: DailyWriter, stop: Arc<AtomicBool>) {
    use std::os::fd::AsRawFd;
    let mut pending = Vec::new();
    let mut bytes = [0; 4096];
    let mut discarding = false;
    loop {
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready == 0 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            continue;
        }
        if ready < 0 {
            continue;
        }
        let count = match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut chunk = &bytes[..count];
        if discarding {
            let Some(end) = chunk.iter().position(|b| *b == b'\n') else {
                continue;
            };
            chunk = &chunk[end + 1..];
            discarding = false;
        }
        pending.extend_from_slice(chunk);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let (date, stamp) = local_stamp();
            let record = if end > MAX_LINE {
                b"[oversized diagnostic record discarded]".as_slice()
            } else {
                &pending[..end]
            };
            let _ = writer.line(&date, &stamp, record);
            pending.drain(..=end);
        }
        if pending.len() >= MAX_LINE {
            // Do not split a URL/token across persisted chunks. Discard an
            // oversized unterminated record instead, retaining a bounded marker.
            let (date, stamp) = local_stamp();
            let _ = writer.line(&date, &stamp, b"[oversized diagnostic record discarded]");
            pending.clear();
            discarding = true;
        }
    }
    if !pending.is_empty() {
        let (date, stamp) = local_stamp();
        let _ = writer.line(&date, &stamp, &pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fd_capture_shutdown() {
        const ENV: &str = "PLEXFREQ_DIAGNOSTIC_CHILD_DIR";
        if let Some(directory) = std::env::var_os(ENV) {
            let log = Log::install(directory.into()).unwrap();
            eprintln!("framework warning https://fixture.invalid/?X-Plex-Token=secret");
            std::io::stdout().write_all(b"stdout marker\n").unwrap();
            drop(log);
            eprintln!("restored stderr");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostics::tests::fd_capture_shutdown",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ENV, root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("restored stderr"));
        let log = fs::read_to_string(
            root.path()
                .join(format!("plexfreq-{}.log", local_stamp().0)),
        )
        .unwrap();
        assert!(
            log.contains("stdout marker")
                && log.contains("framework warning [redacted]")
                && log.contains("shutdown; draining")
        );
        assert!(!log.contains("secret") && !log.contains("restored stderr"));
    }
    #[test]
    fn midnight_rotation_clock_reversal_and_private_redaction() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = DailyWriter::new(root.path().join("logs")).unwrap();
        for (date, stamp, text) in [
            ("2026-10-06", "2026-10-06T23:59:59", "before midnight"),
            (
                "2026-10-07",
                "2026-10-07T00:00:01",
                "https://fixture.invalid/audio?X-Plex-Token=secret warning",
            ),
            ("2026-10-06", "2026-10-06T23:59:58", "clock corrected"),
        ] {
            writer.line(date, stamp, text.as_bytes()).unwrap();
        }
        let yesterday =
            fs::read_to_string(writer.directory.join("plexfreq-2026-10-06.log")).unwrap();
        let today = fs::read_to_string(writer.directory.join("plexfreq-2026-10-07.log")).unwrap();
        assert!(yesterday.contains("before midnight") && yesterday.contains("clock corrected"));
        assert!(!yesterday.contains("warning"));
        assert!(today.contains("2026-10-07T00:00:01") && today.contains("[redacted] warning"));
        assert!(!today.contains("secret") && !today.contains("fixture.invalid"));
        assert_eq!(
            fs::metadata(writer.directory.join("plexfreq-2026-10-07.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&writer.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(redact("X-Plex-Token = \"secret\""), "[redacted]");
        assert_eq!(redact("Authorization: Bearer secret"), "[redacted]");
    }
    #[test]
    fn size_rollover_and_date_retention_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = DailyWriter::new(root.path().into()).unwrap();
        for day in 1..=10 {
            let date = format!("2026-10-{day:02}");
            writer.line(&date, "fixture", b"first").unwrap();
            writer.size = MAX_FILE;
            writer.line(&date, "fixture", b"second").unwrap();
        }
        assert!(!root.path().join("plexfreq-2026-10-03.log").exists());
        assert!(!root.path().join("plexfreq-2026-10-03.log.1").exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), KEEP_DAYS * 2);
        assert!(
            fs::read_to_string(root.path().join("plexfreq-2026-10-10.log.1"))
                .unwrap()
                .contains("first")
        );
        assert!(
            fs::read_to_string(root.path().join("plexfreq-2026-10-10.log"))
                .unwrap()
                .contains("second")
        );
    }
    #[test]
    fn capture_reassembles_split_secrets_and_discards_oversized_tails() {
        use std::os::fd::FromRawFd;
        let root = tempfile::tempdir().unwrap();
        let writer = DailyWriter::new(root.path().into()).unwrap();
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let read = unsafe { File::from_raw_fd(fds[0]) };
        let mut write = unsafe { File::from_raw_fd(fds[1]) };
        let worker = thread::spawn(move || capture(read, writer, Arc::new(AtomicBool::new(false))));
        write
            .write_all(b"framework https://fixture.invalid/?X-Plex-To")
            .unwrap();
        write.write_all(b"ken=secret warning\n").unwrap();
        write.write_all(&vec![b'a'; MAX_LINE + 8192]).unwrap();
        write
            .write_all(b"sensitive-tail\nafter oversized\nfinal partial")
            .unwrap();
        drop(write);
        worker.join().unwrap();
        let log = fs::read_to_string(
            root.path()
                .join(format!("plexfreq-{}.log", local_stamp().0)),
        )
        .unwrap();
        assert!(log.contains("[redacted] warning"));
        assert!(!log.contains("secret") && !log.contains("sensitive-tail"));
        assert!(log.contains("after oversized") && log.contains("final partial"));
        assert_eq!(
            log.matches("oversized diagnostic record discarded").count(),
            1
        );
    }
}
