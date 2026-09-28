//! Shared severity semantics for the existing runtime log sink.
//!
//! Process stderr uses one human line shape: local time, a fixed-width
//! level, then the message. Extra lines stay aligned under the message.
//! Persisted runtime rows are unchanged.

const CONSOLE_LINE_BYTES: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    pub(crate) fn from_env() -> Self {
        match std::env::var("OCG_LOG_LEVEL") {
            Ok(value) => Self::parse(&value).unwrap_or_else(|| {
                eprintln!("WARN invalid OCG_LOG_LEVEL; using info");
                Self::Info
            }),
            Err(_) => Self::Info,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn console_label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Write one operational line to stderr. Callers pass an already-sanitized
/// message: never include Keys, request bodies, or credential-bearing URLs.
/// The callers are the local hosts, which compile only under
/// `dsh-local-host`, so a default-feature build has none to see.
#[allow(dead_code)]
pub fn console_log(level: &str, message: &str) {
    let level = Level::parse(level).unwrap_or(Level::Info);
    console(level, message);
}

#[allow(dead_code)]
pub(crate) fn console(level: Level, message: impl std::fmt::Display) {
    write_console(level, &split_console_lines(&message.to_string()));
}

#[allow(dead_code)]
pub(crate) fn write_console(level: Level, lines: &[impl AsRef<str>]) {
    let lines: Vec<&str> = lines
        .iter()
        .map(AsRef::as_ref)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return;
    }
    for line in render_console_block(level, &lines, chrono::Local::now()) {
        eprintln!("{line}");
    }
}

/// One stamped line plus continuation lines aligned under the message column.
/// The caller supplies the stamp so the rendered shape is testable.
pub(crate) fn render_console_block(
    level: Level,
    lines: &[&str],
    stamp: chrono::DateTime<chrono::Local>,
) -> Vec<String> {
    let prefix = format!("{} {:<5} ", stamp.format("%H:%M:%S"), level.console_label());
    let indent = " ".repeat(prefix.len());
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{indent}{line}")
            }
        })
        .collect()
}

fn split_console_lines(message: &str) -> Vec<String> {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| truncate_console_line(line, CONSOLE_LINE_BYTES))
        .collect()
}

fn truncate_console_line(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.saturating_sub(1).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests;
