mod models;
mod state;
mod db;
mod seats;
mod ticket;
mod transfer;
mod routes;
mod utils;
mod cleanup;
mod config;

use state::AppState;
use db::order_store::{load_orders, load_sold_counts};
use db::csv_loader::{load_trains_static, load_notifications};
use routes::create_routes;
use utils::logging::init_logging;
use utils::helpers::get_mtime;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::time;
use tower_http::services::ServeDir;
use tracing::{info, error};
use chrono::Local;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    // 1. 加载配置（最先执行）
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置加载失败: {}", e);
            std::process::exit(1);
        }
    };
    config::init(cfg);
    let cfg = config::get();

    // 2. 初始化日志（依赖配置）
    let _guard = init_logging();

    // 3. 加载 Tera 模板
    let tera = match tera::Tera::new(&cfg.server.template_dir) {
        Ok(t) => t,
        Err(e) => {
            error!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    // 4. 加载已售座位与车次
    let sold_counts = load_sold_counts();
    let static_trains = load_trains_static();
    let now_date = Local::now().date_naive();
    let mut trains = Vec::new();
    for mut train in static_trains {
        for i in 0..train.available_seats.len() {
            let key = (train.id.clone(), i);
            if let Some(sold_set) = sold_counts.get(&key) {
                train.available_seats[i].retain(|s| !sold_set.contains(s));
            }
        }
        train.seats =
            db::csv_loader::compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        db::csv_loader::update_train_status_with_date(&mut train, &now_date);
        trains.push(train);
    }

    // 5. 加载通知、订单索引、订单
    let notifications = load_notifications();
    let order_index = if let Ok(data) = std::fs::read_to_string(&cfg.paths.order_index_json) {
        serde_json::from_str(&data).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    };
    let orders = load_orders();

    // 6. 构建全局状态
    let state = Arc::new(AppState {
        tera,
        trains: tokio::sync::RwLock::new(trains),
        orders: tokio::sync::RwLock::new(orders),
        sold_counts: tokio::sync::RwLock::new(sold_counts),
        notifications: tokio::sync::RwLock::new(notifications),
        train_mtime: tokio::sync::RwLock::new(get_mtime(&cfg.paths.trains_csv)),
        notif_mtime: tokio::sync::RwLock::new(get_mtime(&cfg.paths.notifications_txt)),
        order_index: tokio::sync::RwLock::new(order_index),
    });

    // 7. 热重载任务
    let state_clone = state.clone();
    let hot_reload_secs = cfg.timeouts.hot_reload_interval_seconds;
    let trains_csv_path = cfg.paths.trains_csv.clone();
    let notif_path = cfg.paths.notifications_txt.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(StdDuration::from_secs(hot_reload_secs));
        loop {
            interval.tick().await;
            let train_mtime = get_mtime(&trains_csv_path);
            let notif_mtime = get_mtime(&notif_path);
            let need_reload = {
                let t_mtime = state_clone.train_mtime.read().await;
                let n_mtime = state_clone.notif_mtime.read().await;
                train_mtime > *t_mtime || notif_mtime > *n_mtime
            };
            if need_reload {
                info!("检测到文件变更，执行热重载...");
                db::csv_loader::reload_all(&state_clone).await;
            }
        }
    });

    // 8. 订单超时清理任务
    let state_clone2 = state.clone();
    let cleanup_secs = cfg.timeouts.cleanup_interval_seconds;
    tokio::spawn(async move {
        cleanup::cleanup_expired_orders(&state_clone2).await;
        let mut interval = time::interval(StdDuration::from_secs(cleanup_secs));
        loop {
            interval.tick().await;
            cleanup::cleanup_expired_orders(&state_clone2).await;
        }
    });

    // 9. 构建路由
    let static_prefix = cfg.server.static_url_prefix.clone();
    let static_dir = cfg.server.static_dir.clone();
    let app = create_routes(state)
        .nest_service(&static_prefix, ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http());

    // 10. 绑定监听
    let bind_addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("无法绑定 {}: {}", bind_addr, e);
            std::process::exit(1);
        }
    };
    info!("至冬国铁路已启动：http://{}", bind_addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}