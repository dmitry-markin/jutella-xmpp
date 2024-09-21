use anyhow::Context as _;
use tracing_log::LogTracer;
use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

fn main() -> anyhow::Result<()> {
    setup_logging()?;

    tracing::error!("Error");
    tracing::warn!("Warning");
    tracing::info!("Hello, World!");
    tracing::debug!("Debug");
    tracing::trace!("Trace");

    Ok(())
}

fn setup_logging() -> anyhow::Result<()> {
    LogTracer::init().context("Failed to initialize `log` tracer")?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .context("Failed to parse env log filter")?;

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer());

    tracing::subscriber::set_global_default(subscriber)
        .context("Failed to set global tracing subscriber")
}
