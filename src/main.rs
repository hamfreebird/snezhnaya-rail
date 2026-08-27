mod models;
mod state;
mod db;
mod seats;
mod ticket;
mod transfer;
mod routes;
mod utils;

use axum::Router;
use state::AppState;
use db::order_store::load_orders;
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
    let _guard = init_logging();

    let tera = match tera::Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            error!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    let sold_counts: HashMap<(String, usize), std::collections::HashSet<u32>> = HashMap::new();
    let static_trains = load_trains_static("trains.csv");
    let now_date = Local::now().date_naive();
    let mut trains = Vec::new();
    for mut train in static_trains {
        train.seats = db::csv_loader::compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        db::csv_loader::update_train_status_with_date(&mut train, &now_date);
        trains.push(train);
    }

    let notifications = load_notifications("notifications.txt");
    let order_index = if let Ok(data) = std::fs::read_to_string("order_index.json") {
        serde_json::from_str(&data).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    };
    let orders = load_orders();

    let state = Arc::new(AppState {
        tera,
        trains: tokio::sync::RwLock::new(trains),
        orders: tokio::sync::RwLock::new(orders),
        sold_counts: tokio::sync::RwLock::new(sold_counts),
        notifications: tokio::sync::RwLock::new(notifications),
        train_mtime: tokio::sync::RwLock::new(get_mtime("trains.csv")),
        notif_mtime: tokio::sync::RwLock::new(get_mtime("notifications.txt")),
        order_index: tokio::sync::RwLock::new(order_index),
    });

    // 热重载任务
    let state_clone = state.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(StdDuration::from_secs(30));
        loop {
            interval.tick().await;
            let train_mtime = get_mtime("trains.csv");
            let notif_mtime = get_mtime("notifications.txt");
            let mut need_reload = false;
            {
                let t_mtime = state_clone.train_mtime.read().await;
                let n_mtime = state_clone.notif_mtime.read().await;
                if train_mtime > *t_mtime || notif_mtime > *n_mtime {
                    need_reload = true;
                }
            }
            if need_reload {
                info!("检测到文件变更，执行热重载...");
                db::csv_loader::reload_all(&state_clone).await;
            }
        }
    });

    let app = create_routes(state)
        .nest_service("/static", ServeDir::new("static"))
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}