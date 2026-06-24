//! In-process diagnostics ring for the embedded daemon.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_RING_CAPACITY: usize = 1_000;

static LOG_RING: OnceLock<Mutex<LogRing>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

struct LogRing {
    capacity: usize,
    lines: VecDeque<String>,
}

impl LogRing {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lines: VecDeque::with_capacity(capacity),
        }
    }

    fn push(&mut self, line: String) {
        if self.capacity == 0 {
            return;
        }
        while self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn joined(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line);
        }
        text
    }
}

fn ring() -> &'static Mutex<LogRing> {
    LOG_RING.get_or_init(|| Mutex::new(LogRing::new(LOG_RING_CAPACITY)))
}

fn lock_ring() -> MutexGuard<'static, LogRing> {
    ring().lock().unwrap_or_else(PoisonError::into_inner)
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

pub fn record(level: Level, args: fmt::Arguments<'_>) {
    let line = format!("{} {:<5} {}", timestamp(), level.as_str(), args);
    eprintln!("{line}");
    lock_ring().push(line);
}

pub fn tail(max_bytes: usize) -> String {
    let text = lock_ring().joined();
    if text.len() <= max_bytes {
        return text;
    }

    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    let newline_start = text
        .as_bytes()
        .get(start..)
        .and_then(|tail| tail.iter().position(|byte| *byte == b'\n'))
        .map(|pos| start.saturating_add(pos).saturating_add(1))
        .filter(|pos| *pos < text.len())
        .unwrap_or(start);
    text.get(newline_start..).unwrap_or_default().to_string()
}

#[macro_export]
macro_rules! diag {
    (error, $($arg:tt)*) => {
        $crate::diagnostics::record($crate::diagnostics::Level::Error, format_args!($($arg)*))
    };
    (warn, $($arg:tt)*) => {
        $crate::diagnostics::record($crate::diagnostics::Level::Warn, format_args!($($arg)*))
    };
    (info, $($arg:tt)*) => {
        $crate::diagnostics::record($crate::diagnostics::Level::Info, format_args!($($arg)*))
    };
    (debug, $($arg:tt)*) => {
        $crate::diagnostics::record($crate::diagnostics::Level::Debug, format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::LogRing;

    #[test]
    fn log_ring_evicts_oldest_lines() {
        let mut ring = LogRing::new(3);
        for line in ["one", "two", "three", "four"] {
            ring.push(line.to_string());
        }

        let lines: Vec<String> = ring.lines.iter().cloned().collect();
        assert_eq!(lines, vec!["two", "three", "four"]);
    }
}
