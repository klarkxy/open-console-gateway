//! Severity semantics for persisted dashboard runtime rows.
//! Program diagnostics live in `process_log` and use the independent RUST_LOG filter.

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
                tracing::warn!("invalid OCG_LOG_LEVEL; using info for dashboard runtime rows");
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
}

#[cfg(test)]
mod tests;
