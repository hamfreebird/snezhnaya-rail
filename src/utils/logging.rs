// src/utils/logging.rs
use tracing::info;
use tracing_subscriber::{
    layer::SubscriberExt,
    util::SubscriberInitExt,
    filter::EnvFilter,
};
use tracing_appender::non_blocking::WorkerGuard;

/// 根据配置初始化日志系统。
/// 若文件日志被禁用，返回 None；否则返回 WorkerGuard 需保持存活。
pub fn init_logging() -> Option<WorkerGuard> {
    let cfg = &crate::config::get().logging;

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.level));

    let console_layer = if cfg.console_enabled {
        Some(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_ansi(true)
                .with_target(true)
                .with_thread_names(true)
                .with_level(true)
                .pretty(),
        )
    } else {
        None
    };

    let (file_layer, guard) = if cfg.file_enabled {
        let log_dir = &crate::config::get().paths.log_dir;
        let file_appender = tracing_appender::rolling::daily(log_dir, "snezhnaya-rail.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_target(true)
            .with_level(true)
            .json();
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    info!("日志系统初始化完成");
    guard
}