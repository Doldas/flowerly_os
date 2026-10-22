//! Typed read-only Linux telemetry adapter (Layer 1.5).
//! Never substitutes a missing kernel measurement with an invented value.

use std::io;

pub(crate) trait HostTelemetry {
    fn memory_info(&self) -> io::Result<String>;
    fn memory_pressure(&self) -> io::Result<String>;
    fn drm_cards(&self) -> io::Result<Vec<String>>;
}

pub(crate) struct LinuxHostTelemetry;

impl HostTelemetry for LinuxHostTelemetry {
    fn memory_info(&self) -> io::Result<String> {
        std::fs::read_to_string("/proc/meminfo")
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
