// src/config.rs
//! 全局配置：从 config.toml + 环境变量 + 默认值三层合并。

use serde::Deserialize;
use std::sync::OnceLock;

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

/// 获取全局配置。必须在 init 之后调用。
pub fn get() -> &'static AppConfig {
    CONFIG.get().expect("配置未初始化，请先调用 config::init")
}

/// 初始化全局配置。
pub fn init(cfg: AppConfig) {
    let _ = CONFIG.set(cfg);
}

/// 按优先级加载配置：环境变量 > config.toml > 默认值。
pub fn load() -> Result<AppConfig, config::ConfigError> {
    let mut builder = config::Config::builder();

    // 配置文件路径可由 SNEZ_CONFIG_FILE 指定，默认 config.toml
    let config_file =
        std::env::var("SNEZ_CONFIG_FILE").unwrap_or_else(|_| "config.toml".to_string());
    if std::path::Path::new(&config_file).exists() {
        builder = builder.add_source(
            config::File::with_name(&config_file).required(false),
        );
    }

    // 环境变量前缀 SNEZ_，层级分隔符 __
    builder = builder.add_source(
        config::Environment::with_prefix("SNEZ")
            // 文档约定：前缀后用单个下划线（SNEZ_SERVER__PORT），层级用双下划线。
            .prefix_separator("_")
            .separator("__")
            .try_parsing(true),
    );

    builder.build()?.try_deserialize::<AppConfig>()
}

// ============ 顶层结构 ============

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    #[serde(default)]
    pub business: BusinessConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

// ============ 服务 ============

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub template_dir: String,
    pub static_dir: String,
    pub static_url_prefix: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            template_dir: "templates/**/*.html".to_string(),
            static_dir: "static".to_string(),
            static_url_prefix: "/static".to_string(),
        }
    }
}

// ============ 路径 ============

#[derive(Debug, Clone, Deserialize)]
pub struct PathsConfig {
    pub trains_csv: String,
    pub notifications_txt: String,
    pub orders_json: String,
    pub order_index_json: String,
    pub sold_counts_json: String,
    pub log_dir: String,
    pub font_paths: Vec<String>,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            trains_csv: "trains.csv".to_string(),
            notifications_txt: "notifications.txt".to_string(),
            orders_json: "orders.json".to_string(),
            order_index_json: "order_index.json".to_string(),
            sold_counts_json: "sold_counts.json".to_string(),
            log_dir: "logs".to_string(),
            font_paths: vec![
                "C:/Windows/Fonts/simhei.ttf".to_string(),
                "C:/Windows/Fonts/msyh.ttc".to_string(),
                "C:/Windows/Fonts/yahei.ttf".to_string(),
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string(),
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_string(),
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_string(),
                "/System/Library/Fonts/PingFang.ttc".to_string(),
                "/System/Library/Fonts/STHeiti Light.ttc".to_string(),
                "static/fonts/msyh.ttf".to_string(),
            ],
        }
    }
}

// ============ 超时 ============

#[derive(Debug, Clone, Deserialize)]
pub struct TimeoutsConfig {
    pub order_expire_minutes: i64,
    pub cleanup_interval_seconds: u64,
    pub hot_reload_interval_seconds: u64,
    pub min_transfer_minutes: i64,
    pub stop_selling_before_departure_minutes: i64,
    pub near_departure_window_minutes: i64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            order_expire_minutes: 30,
            cleanup_interval_seconds: 30,
            hot_reload_interval_seconds: 30,
            min_transfer_minutes: 8,
            stop_selling_before_departure_minutes: 30,
            near_departure_window_minutes: 120,
        }
    }
}

// ============ 业务规则 ============

#[derive(Debug, Clone, Deserialize)]
pub struct BusinessConfig {
    pub presale_days: i64,
    pub max_transfer_plans: usize,
    pub seats_per_carriage_default: u32,
    pub seats_per_carriage_zhite: u32,
    #[serde(default)]
    pub discount_factors: DiscountFactors,
}

impl Default for BusinessConfig {
    fn default() -> Self {
        Self {
            presale_days: 7,
            max_transfer_plans: 50,
            seats_per_carriage_default: 40,
            seats_per_carriage_zhite: 35,
            discount_factors: DiscountFactors::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscountFactors {
    pub today_default: f64,
    pub near_departure_min: f64,
    pub day_1: f64,
    pub day_2: f64,
    pub day_3: f64,
    pub day_4: f64,
    pub day_5: f64,
    pub day_6: f64,
    pub day_7: f64,
    pub future: f64,
}

impl Default for DiscountFactors {
    fn default() -> Self {
        Self {
            today_default: 0.15,
            near_departure_min: 0.0,
            day_1: 0.2,
            day_2: 0.6,
            day_3: 0.9,
            day_4: 0.925,
            day_5: 0.95,
            day_6: 0.975,
            day_7: 1.0,
            future: 1.0,
        }
    }
}

// ============ 安全 ============

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    pub admin_token: String,
    pub rate_limit_per_second: u64,
    pub rate_limit_burst: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            admin_token: String::new(),
            rate_limit_per_second: 10,
            rate_limit_burst: 30,
        }
    }
}

// ============ 日志 ============

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub console_enabled: bool,
    pub file_enabled: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info,snezhnaya_rail=debug".to_string(),
            console_enabled: true,
            file_enabled: true,
        }
    }
}