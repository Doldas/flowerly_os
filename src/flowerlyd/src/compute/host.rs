//! Typed read-only Linux telemetry adapter (Layer 1.5).
//! Never substitutes a missing kernel measurement with an invented value.

use std::io;

/// Type-safe classification used by future host facade callers. Never
/// converts a kernel failure to a fabricated successful observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostReadFailure {
    Unavailable,
    PermissionDenied,
    Interrupted,
    Io,
}

impl From<&io::Error> for HostReadFailure {
    fn from(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::Unavailable,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::Interrupted | io::ErrorKind::TimedOut => Self::Interrupted,
            _ => Self::Io,
        }
    }
}

pub(crate) trait HostTelemetry {
    fn memory_info(&self) -> io::Result<String>;
    fn memory_pressure(&self) -> io::Result<String>;
    fn drm_cards(&self) -> io::Result<Vec<String>>;
}

pub(crate) struct LinuxHostTelemetry;

impl HostTelemetry for LinuxHostTelemetry {
    fn memory_info(&self) -> io::Result<String> {
        std::fs::read_to_string("/proc/meminfo").inspect_err(|error| {
            tracing::debug!(
                failure = ?HostReadFailure::from(error),
                "Linux memory telemetry read failed"
            );
        })
    }

    fn memory_pressure(&self) -> io::Result<String> {
        std::fs::read_to_string("/proc/pressure/memory")
    }

    fn drm_cards(&self) -> io::Result<Vec<String>> {
        let entries = std::fs::read_dir("/sys/class/drm")?;
        let mut cards = Vec::new();
        for entry in entries {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("card") {
                    cards.push(name.to_owned());
                }
            }
        }
        cards.sort();
        Ok(cards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unavailable;
    impl HostTelemetry for Unavailable {
        fn memory_info(&self) -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no meminfo"))
        }
        fn memory_pressure(&self) -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "no psi"))
        }
        fn drm_cards(&self) -> io::Result<Vec<String>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no drm"))
        }
    }

    #[test]
    fn host_failures_remain_typed_and_never_look_like_success() {
        for (kind, expected) in [
            (io::ErrorKind::NotFound, HostReadFailure::Unavailable),
            (
                io::ErrorKind::PermissionDenied,
                HostReadFailure::PermissionDenied,
            ),
            (io::ErrorKind::TimedOut, HostReadFailure::Interrupted),
            (io::ErrorKind::Other, HostReadFailure::Io),
        ] {
            let error = io::Error::from(kind);
            assert_eq!(HostReadFailure::from(&error), expected);
        }
    }

    #[test]
    fn unavailable_host_does_not_fabricate_measurements() {
        let host = Unavailable;
        assert!(host.memory_info().is_err());
        assert!(host.memory_pressure().is_err());
        assert!(host.drm_cards().is_err());
        let budget = super::super::DeviceBudget::measure_with(&host);
        assert_eq!(budget.total_memory_bytes, None);
        assert_eq!(budget.available_memory_bytes, None);
        assert!(!budget.meminfo_source_available);
        assert!(budget.gpu_detected.is_empty());
        let pressure = super::super::admission::MemoryPressure::measure_with(&host);
        assert_eq!(pressure.avg10_pct, None);
    }
}
