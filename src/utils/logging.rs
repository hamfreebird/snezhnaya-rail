use tracing::info;
use tracing_subscriber::{
    layer::SubscriberExt,
    util::SubscriberInitExt,
    filter::EnvFilter,
};
use tracing_appender::non_blocking::WorkerGuard;

pub fn init_logging() -> WorkerGuard {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,snezhnaya_rail=debug"));

    let console_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_target(true)
        .with_thread_names(true)
        .with_level(true)
        .pretty();

    let file_appender = tracing_appender::rolling::daily("logs", "snezhnaya-rail.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .json();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    info!("日志系统初始化完成（控制台 + 文件 logs/snezhnaya-rail.log）");
    guard
}