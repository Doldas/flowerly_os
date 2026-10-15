use crate::config::LogFormat;
use tracing_subscriber::EnvFilter;

pub fn init_tracing(service: &str, format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format {
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_target(true)
                .try_init();
        }
        LogFormat::Text => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .try_init();
        }
    }

    tracing::info!(
        service,
        rust_version = env!("CARGO_PKG_RUST_VERSION"),
        "structured logging initialised"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_can_initialise_twice_safely() {
        init_tracing("test", LogFormat::Text);
        init_tracing("test", LogFormat::Json);
    }
}
