//! A start-up failure is never silent.
//!
//! On Windows `playdropd` and `playdrop-desktop` are GUI programs
//! (`windows_subsystem = "windows"`), so no console window flashes up at sign-in or behind
//! the window. The price is that an error returned from `main` or a panic has nowhere to go:
//! started from the Start Menu, the Run key or the installer, the program just vanishes.
//! This module gives both programs the same three things:
//!
//! - [`LogFile`]: a size-capped log at `%LOCALAPPDATA%\PlayDrop\logs\<program>.log` that the
//!   `tracing` subscriber writes to ([`open_program_log`]). The programs log what they logged
//!   before, nothing more, so SR-32 holds for the file as it did for the console.
//! - [`report_fatal`] and [`install_panic_hook`]: the error goes to that log and into a
//!   message box that says what failed and names the log file to send. The box is skipped
//!   when [`NO_DIALOG_ENV`] is set, so a CI run can never hang on it.
//! - [`attach_parent_console`]: started from a terminal, output still reaches it.
//!
//! On Linux nothing changes: there is no log file and no box, an error from `main` prints
//! `Error: ...` to stderr exactly as a `main` returning `Result` does, and the default panic
//! hook stays.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// Set to anything but `0` or empty: [`report_fatal`] and the panic hook log and exit
/// without the message box. For CI and scripted test runs, where nobody could press OK.
pub const NO_DIALOG_ENV: &str = "PLAYDROP_NO_FATAL_DIALOG";

/// Size at which a log file is moved to `<name>.old` and started again. Two files of this
/// size are kept at most, so a daemon that runs for months cannot fill the disk.
pub const LOG_CAP: u64 = 1024 * 1024;

/// The marker on the line [`report_fatal`] writes. CI fails a run whose logs contain it.
pub const FATAL_MARKER: &str = "FATAL";

/// The marker on the line the panic hook writes for a panic on any other thread (one that
/// tokio catches, for example). Also a CI failure.
pub const PANIC_MARKER: &str = "PANIC";

/// The folder the logs go to: `%LOCALAPPDATA%\PlayDrop\logs` on Windows, next to the data
/// dir (`%LOCALAPPDATA%\PlayDrop\data`). `None` elsewhere: Linux keeps logging to the
/// console and the journal only.
pub fn log_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("PlayDrop").join("logs"))
    } else {
        None
    }
}

/// The log file of `program` (`playdropd`, `playdrop-desktop`), where [`log_dir`] has one.
pub fn log_path(program: &str) -> Option<PathBuf> {
    log_dir().map(|d| d.join(format!("{program}.log")))
}

/// The process's log, set by the first [`open_program_log`], so a fatal report lands in the
/// same file the subscriber writes to.
static PROGRAM_LOG: OnceLock<LogFile> = OnceLock::new();

/// Opens this program's log file and remembers it for [`report_fatal`]. `None` on platforms
/// without a log file, or when it cannot be created (the program then runs without one:
/// a read-only profile must not stop it).
pub fn open_program_log(program: &str) -> Option<LogFile> {
    if let Some(log) = PROGRAM_LOG.get() {
        return Some(log.clone());
    }
    let path = log_path(program)?;
    match LogFile::open(&path, LOG_CAP) {
        Ok(log) => {
            // SR-32 wants log files readable by their user only. The profile folder already
            // keeps other users out; this also drops the Administrators entry, as the data
            // dir does. Best effort: a log nobody else can read is not worth not starting.
            #[cfg(windows)]
            if let Some(dir) = path.parent() {
                if let Err(e) = crate::at_rest::protect_dir(dir) {
                    let _ = writeln!(io::stderr(), "cannot make the log folder private: {e}");
                }
            }
            Some(PROGRAM_LOG.get_or_init(|| log).clone())
        }
        Err(e) => {
            let _ = writeln!(io::stderr(), "cannot open the log {}: {e}", path.display());
            None
        }
    }
}

/// An append-only log file that moves itself to `<name>.old` once it would grow past its
/// cap. Cheap to clone (every clone writes to the same file under one lock), and an
/// [`io::Write`], so `move || log.clone()` is a `tracing_subscriber` writer.
///
/// Each `write` call is written whole, so one log record never straddles a rotation.
#[derive(Clone, Debug)]
pub struct LogFile {
    inner: Arc<Mutex<Inner>>,
    path: Arc<PathBuf>,
}

#[derive(Debug)]
struct Inner {
    /// `None` after a failed reopen; the next write tries again.
    file: Option<File>,
    /// Bytes in the current file, as far as this process knows.
    len: u64,
    cap: u64,
}

impl LogFile {
    /// Opens (creating it and its folder if needed) the log at `path`, rotating first if it
    /// is already at `cap` bytes.
    pub fn open(path: &Path, cap: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = open_append(path)?;
        let len = file.metadata()?.len();
        let log = Self {
            inner: Arc::new(Mutex::new(Inner {
                file: Some(file),
                len,
                cap,
            })),
            path: Arc::new(path.to_owned()),
        };
        if len >= cap {
            let mut inner = log.lock();
            log.rotate(&mut inner);
        }
        Ok(log)
    }

    /// Where the log is written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where a full log is moved: `<name>.old` (`playdropd.log.old`).
    pub fn old_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_owned();
        name.push(".old");
        self.path.with_file_name(name)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves at worst a half-written line.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves the current file to `.old` and starts an empty one. If the move fails (another
    /// program holds the old file open without sharing), the current file is emptied instead,
    /// so the cap holds either way.
    fn rotate(&self, inner: &mut Inner) {
        inner.file = None;
        let old = self.old_path();
        // `rename` replaces an existing target on both Windows and Unix.
        if std::fs::rename(&*self.path, &old).is_err() {
            let _ = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&*self.path);
        }
        inner.file = open_append(&self.path).ok();
        inner.len = 0;
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.lock();
        let incoming = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if inner.len > 0 && inner.len.saturating_add(incoming) > inner.cap {
            self.rotate(&mut inner);
        }
        if inner.file.is_none() {
            inner.file = Some(open_append(&self.path)?);
        }
        let Some(file) = inner.file.as_mut() else {
            return Err(io::Error::other("log file not open"));
        };
        file.write_all(buf)?;
        inner.len = inner.len.saturating_add(incoming);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.lock().file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// Connects a GUI-subsystem program started from a terminal to that terminal, so `--help`,
/// `--version` and the log still show there. Call first thing in `main`. Returns whether
/// standard output now leads somewhere ([`has_console`] answers the same later).
///
/// Elsewhere programs always have their standard streams, so this is `true` and does
/// nothing.
pub fn attach_parent_console() -> bool {
    #[cfg(windows)]
    {
        let attached = win::attach_parent_console();
        CONSOLE.store(attached, std::sync::atomic::Ordering::Relaxed);
        attached
    }
    #[cfg(not(windows))]
    true
}

#[cfg(windows)]
static CONSOLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether [`attach_parent_console`] found a console or an inherited output stream. Always
/// `true` off Windows.
pub fn has_console() -> bool {
    #[cfg(windows)]
    {
        CONSOLE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    true
}

/// Whether [`NO_DIALOG_ENV`] turns the message box off.
pub fn dialogs_disabled() -> bool {
    std::env::var_os(NO_DIALOG_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Reports an error that ends `program`. Call it with `format!("{e:?}")` of the error `main`
/// would have returned, then exit with a failure code.
///
/// Everywhere: prints `Error: <error>` to stderr, byte for byte what a `main` returning
/// `Result` prints. Windows also: a `FATAL` line in the program's log and, unless
/// [`dialogs_disabled`], a message box with what failed, the error and the log's path.
pub fn report_fatal(program: &str, error: &str) {
    let _ = writeln!(io::stderr(), "Error: {error}");
    record_fatal(program, error);
}

/// The Windows half of [`report_fatal`] without the stderr line: the `FATAL` log line and the
/// message box. For an error that already printed itself to the console in its own words (a
/// bad command line, which clap prints), so it is not printed twice. Does nothing elsewhere.
pub fn record_fatal(program: &str, error: &str) {
    #[cfg(windows)]
    {
        let log = open_program_log(program);
        if let Some(mut l) = log.clone() {
            let _ = l.write_all(log_line(FATAL_MARKER, program, error).as_bytes());
        }
        if !dialogs_disabled() {
            win::error_box(&fatal_message(
                program,
                error,
                log.as_ref().map(LogFile::path),
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = (program, error);
}

/// Installs a panic hook for `program`. Windows: the panic message and location go to the
/// log; a panic on the main thread, which ends the program, also shows the message box (no
/// backtrace in it). A panic on another thread is logged only: tokio catches those and the
/// program carries on, and a modal box would stall that thread. The previous hook still runs,
/// so a console, when there is one, sees the usual output.
///
/// Elsewhere this does nothing and the default hook stays.
pub fn install_panic_hook(program: &'static str) {
    #[cfg(windows)]
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let text = panic_text(info);
            let on_main = std::thread::current().name() == Some("main");
            let marker = if on_main { FATAL_MARKER } else { PANIC_MARKER };
            let log = open_program_log(program);
            if let Some(mut l) = log.clone() {
                let _ = l.write_all(log_line(marker, program, &text).as_bytes());
            }
            previous(info);
            if on_main && !dialogs_disabled() {
                win::error_box(&panic_message(
                    program,
                    &text,
                    log.as_ref().map(LogFile::path),
                ));
            }
        }));
    }
    #[cfg(not(windows))]
    let _ = program;
}

/// The panic's message and where it happened, without a backtrace.
// Used by the Windows bodies only; kept and tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn panic_text(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned());
    match info.location() {
        Some(at) => format!("{message} (at {}:{}:{})", at.file(), at.line(), at.column()),
        None => message,
    }
}

/// What the user calls `program`.
fn friendly_name(program: &str) -> String {
    match program {
        "playdropd" => "The PlayDrop background service (playdropd)".to_owned(),
        "playdrop-desktop" => "The PlayDrop window".to_owned(),
        other => format!("PlayDrop ({other})"),
    }
}

/// Where to look next, worded for the box.
fn log_hint(log: Option<&Path>) -> String {
    match log {
        Some(path) => format!(
            "The details are in this log file. Please attach it when you report the problem:\n{}",
            path.display()
        ),
        None => {
            "No log file could be written. Run the program from a terminal to see more.".to_owned()
        }
    }
}

/// The message box text for an error that ended `program`.
pub fn fatal_message(program: &str, error: &str, log: Option<&Path>) -> String {
    format!(
        "{} stopped because of an error.\n\n{}\n\n{}",
        friendly_name(program),
        error.trim_end(),
        log_hint(log)
    )
}

/// The message box text for a panic that ended `program`.
pub fn panic_message(program: &str, text: &str, log: Option<&Path>) -> String {
    format!(
        "{} stopped because of a problem in PlayDrop itself.\n\n{}\n\n{}",
        friendly_name(program),
        text.trim_end(),
        log_hint(log)
    )
}

/// One log record in the shape the `tracing` lines around it have: UTC time, level-like
/// marker, the text. Exactly one line, ending with a newline: an `anyhow` chain (`{e:?}`) or
/// a clap error spans several lines, and a reader that greps for the marker (CI, a person
/// with `findstr`) must get the whole record. Its lines are joined with ` | `, blank ones
/// dropped. The message box keeps the real line breaks.
// Used by the Windows bodies only; kept and tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn log_line(marker: &str, program: &str, text: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let folded = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    format!("{} {marker} {program}: {folded}\n", utc_stamp(secs))
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`, without a date crate (Howard
/// Hinnant's days-to-civil algorithm).
// Used by the Windows bodies only; kept and tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn utc_stamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(windows)]
mod win {
    //! The two Win32 calls this module needs, each under a scoped `#[allow(unsafe_code)]`.

    use windows::core::HSTRING;
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };

    /// Whether standard output leads somewhere (a console or an inherited file or pipe).
    #[allow(unsafe_code)]
    fn stdout_usable() -> bool {
        // SAFETY: GetStdHandle only reads this process's standard handle table.
        unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid())
    }

    #[allow(unsafe_code)]
    pub(super) fn attach_parent_console() -> bool {
        // Started with redirected output (a script, CI): the inherited handles already work.
        if stdout_usable() {
            return true;
        }
        // SAFETY: plain call with a constant argument. It fails harmlessly when the parent
        // has no console (Explorer, the Run key, the installer).
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
            return false;
        }
        stdout_usable()
    }

    /// A modal error box with no owner window. Blocks until it is closed.
    #[allow(unsafe_code)]
    pub(super) fn error_box(text: &str) {
        let text = HSTRING::from(text);
        let caption = HSTRING::from("PlayDrop");
        // SAFETY: both strings are NUL-terminated wide strings that live for the whole call;
        // no owner window is passed.
        unsafe {
            MessageBoxW(
                None,
                &text,
                &caption,
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pd-startup-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn log_creates_its_folder_and_appends() {
        let dir = temp_dir("append");
        let path = dir.join("logs").join("p.log");
        let mut log = LogFile::open(&path, 1024).unwrap();
        log.write_all(b"one\n").unwrap();
        // A clone writes to the same file, as the subscriber's writers do.
        log.clone().write_all(b"two\n").unwrap();
        drop(log);
        let mut again = LogFile::open(&path, 1024).unwrap();
        again.write_all(b"three\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
        assert!(!again.old_path().exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn log_rotates_to_old_at_the_cap() {
        let dir = temp_dir("rotate");
        let path = dir.join("p.log");
        let mut log = LogFile::open(&path, 10).unwrap();
        log.write_all(b"12345678\n").unwrap();
        // Would pass the cap: the first record moves to .old, this one starts a fresh file.
        log.write_all(b"abc\n").unwrap();
        assert_eq!(log.old_path(), dir.join("p.log.old"));
        assert_eq!(
            std::fs::read_to_string(log.old_path()).unwrap(),
            "12345678\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc\n");
        // A second rotation replaces the old .old.
        log.write_all(b"defghij\n").unwrap();
        assert_eq!(std::fs::read_to_string(log.old_path()).unwrap(), "abc\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "defghij\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_record_larger_than_the_cap_is_still_written_whole() {
        let dir = temp_dir("big");
        let path = dir.join("p.log");
        let mut log = LogFile::open(&path, 4).unwrap();
        log.write_all(b"0123456789\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0123456789\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_full_log_is_rotated_when_opened() {
        let dir = temp_dir("full");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.log");
        std::fs::write(&path, b"0123456789").unwrap();
        let mut log = LogFile::open(&path, 10).unwrap();
        log.write_all(b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(
            std::fs::read_to_string(log.old_path()).unwrap(),
            "0123456789"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_log_file_off_windows() {
        if cfg!(not(windows)) {
            assert!(log_dir().is_none());
            assert!(log_path("playdropd").is_none());
            assert!(open_program_log("playdropd").is_none());
        }
    }

    #[test]
    fn fatal_message_says_what_failed_the_error_and_the_log() {
        let log = Path::new(r"C:\Users\a\AppData\Local\PlayDrop\logs\playdropd.log");
        let text = fatal_message("playdropd", "preparing C:\\x for keys (SR-28)\n", Some(log));
        assert!(text.starts_with(
            "The PlayDrop background service (playdropd) stopped because of an error."
        ));
        assert!(text.contains("\n\npreparing C:\\x for keys (SR-28)\n\n"));
        assert!(text.ends_with(&log.display().to_string()));
        assert!(text.contains("Please attach it"));

        let window = fatal_message("playdrop-desktop", "no renderer", None);
        assert!(window.starts_with("The PlayDrop window stopped because of an error."));
        assert!(window.contains("No log file could be written"));

        let panic = panic_message("playdrop-desktop", "boom (at src/x.rs:1:2)", Some(log));
        assert!(panic.contains("a problem in PlayDrop itself"));
        assert!(panic.contains("boom (at src/x.rs:1:2)"));
        assert!(!panic.contains("stack backtrace"));
    }

    #[test]
    fn log_line_carries_the_marker_once_per_record() {
        let line = log_line(FATAL_MARKER, "playdropd", "bad\n");
        assert!(line.ends_with(" FATAL playdropd: bad\n"), "{line}");
        assert_eq!(line.matches('\n').count(), 1);

        // An anyhow chain as `{e:?}` prints it, with Windows line ends in the mix.
        let chain =
            "preparing C:\\x\\not-a-folder\\data\r\n\r\nCaused by:\n    0: inner\n    1: root\n";
        let line = log_line(FATAL_MARKER, "playdropd", chain);
        assert!(
            line.ends_with(
                " FATAL playdropd: preparing C:\\x\\not-a-folder\\data | Caused by: | 0: inner | 1: root\n"
            ),
            "{line}"
        );
        assert_eq!(line.matches('\n').count(), 1);
        assert!(!line.contains('\r'), "{line}");
    }

    #[test]
    fn utc_stamps() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_stamp(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn the_dialog_switch_reads_the_variable() {
        // Only reads: tests run in parallel, so the variable is never set here.
        if std::env::var_os(NO_DIALOG_ENV).is_none() {
            assert!(!dialogs_disabled());
        }
    }
}
