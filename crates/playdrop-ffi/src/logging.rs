//! Logging for the host app. On Android, `tracing` events go to logcat under the tag
//! `playdrop`; elsewhere they go to stderr. Secrets never reach a log line by construction
//! (`Secret<T>` redacts `Debug`, SR-32); device IDs are logged truncated by the core.

use std::sync::OnceLock;

static INIT: OnceLock<()> = OnceLock::new();

/// Minimum level to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational (default for release builds).
    Info,
    /// Debug (default for debug builds). File names appear at this level.
    Debug,
    /// Everything.
    Trace,
}

impl From<LogLevel> for tracing::Level {
    fn from(l: LogLevel) -> Self {
        match l {
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

/// Installs the global logger once. Later calls are no-ops.
#[uniffi::export]
pub fn init_logging(level: LogLevel) {
    INIT.get_or_init(|| install(level.into()));
}

#[cfg(target_os = "android")]
fn install(level: tracing::Level) {
    use tracing_subscriber::prelude::*;
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log_level(level))
            .with_tag("playdrop"),
    );
    let _ = tracing_subscriber::registry()
        .with(android::LogcatLayer)
        .with(tracing_subscriber::filter::LevelFilter::from_level(level))
        .try_init();
}

#[cfg(target_os = "android")]
fn log_level(level: tracing::Level) -> log::LevelFilter {
    match level {
        tracing::Level::ERROR => log::LevelFilter::Error,
        tracing::Level::WARN => log::LevelFilter::Warn,
        tracing::Level::INFO => log::LevelFilter::Info,
        tracing::Level::DEBUG => log::LevelFilter::Debug,
        tracing::Level::TRACE => log::LevelFilter::Trace,
    }
}

#[cfg(not(target_os = "android"))]
fn install(level: tracing::Level) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .try_init();
}

/// A `tracing` layer that forwards each event to the `log` crate, which
/// `android_logger` writes to logcat. Fields are rendered as `key=value` after the
/// message, in the same shape as the daemon's stderr lines.
#[cfg(target_os = "android")]
mod android {
    use std::fmt::Write as _;

    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::Layer;

    pub(super) struct LogcatLayer;

    #[derive(Default)]
    struct Line {
        message: String,
        fields: String,
    }

    impl Visit for Line {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.message, "{value:?}");
            } else {
                let _ = write!(self.fields, " {}={value:?}", field.name());
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.message.push_str(value);
            } else {
                let _ = write!(self.fields, " {}={value}", field.name());
            }
        }
    }

    fn level_of(l: &Level) -> log::Level {
        match *l {
            Level::ERROR => log::Level::Error,
            Level::WARN => log::Level::Warn,
            Level::INFO => log::Level::Info,
            Level::DEBUG => log::Level::Debug,
            Level::TRACE => log::Level::Trace,
        }
    }

    impl<S: Subscriber> Layer<S> for LogcatLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut line = Line::default();
            event.record(&mut line);
            let meta = event.metadata();
            log::logger().log(
                &log::Record::builder()
                    .level(level_of(meta.level()))
                    .target(meta.target())
                    .args(format_args!("{}{}", line.message, line.fields))
                    .build(),
            );
        }
    }
}
