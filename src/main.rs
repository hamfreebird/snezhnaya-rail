use axum::{
    Form, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{Duration, Local, Utc};
use csv::ReaderBuilder;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::sync::{Arc, RwLock};
use std::time::{Duration as StdDuration, SystemTime};
use tera::{Context, Tera};
use tokio::time;
use tower_http::services::ServeDir;
use uuid::Uuid;

// ---------- 数据结构 ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Train {
    id: String,
    model: String,
    formation: String,
    stations: Vec<String>,
    times: Vec<String>,
    prices: Vec<u32>,
    weights: Vec<u32>, // 权重列表，与区间一一对应
    total_seats: u32,
    // 动态计算的余票（初始分配减去已售），运行时维护
    seats: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    train_id: String,
    from_station: String,
    to_station: String,
    passenger_name: String,
    seat_count: u32,
    price: u32,
    status: OrderStatus,
    expire_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum OrderStatus {
    Pending,
    Paid,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct Notification {
    title: String,
    content: String,
}

// ---------- 应用状态 ----------
struct AppState {
    tera: Tera,
    trains: RwLock<Vec<Train>>,
    orders: RwLock<HashMap<String, Order>>,
    // 记录每个区间已售出的票数，用于热重载时重新计算余票
    sold_counts: RwLock<HashMap<(String, usize), u32>>,
    notifications: RwLock<Vec<Notification>>,
    // 文件修改时间记录，用于热重载检测
    train_mtime: RwLock<SystemTime>,
    notif_mtime: RwLock<SystemTime>,
}

// ---------- 查询参数 ----------
#[derive(Debug, Deserialize)]
struct SearchParams {
    from: String,
    to: String,
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateOrderForm {
    train_id: String,
    from: String,
    to: String,
    total_price: u32,
    passenger_name: String,
    seat_count: u32,
}

// ---------- 辅助函数 ----------
// 查找有效区间（处理重复站点）
fn find_interval(train: &Train, from: &str, to: &str) -> Option<(usize, usize)> {
    let from_indices: Vec<usize> = train
        .stations
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == from)
        .map(|(i, _)| i)
        .collect();
    let to_indices: Vec<usize> = train
        .stations
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == to)
        .map(|(i, _)| i)
        .collect();
    for &fi in &from_indices {
        for &ti in &to_indices {
            if fi < ti {
                return Some((fi, ti));
            }
        }
    }
    None
}

// 根据权重和总座位数计算每个区间的初始分配票数
fn compute_initial_seats(weights: &[u32], total_seats: u32) -> Vec<u32> {
    if weights.is_empty() {
        return Vec::new();
    }
    let sum_weights: u32 = weights.iter().sum();
    if sum_weights == 0 {
        // 全零权重，平均分配
        let base = total_seats / (weights.len() as u32);
        let remainder = total_seats % (weights.len() as u32);
        let mut seats = vec![base; weights.len()];
        for i in 0..remainder as usize {
            seats[i] += 1;
        }
        return seats;
    }
    // 按权重比例分配
    let mut seats = Vec::with_capacity(weights.len());
    let mut allocated = 0;
    for (_i, &w) in weights.iter().enumerate() {
        let share = (total_seats * w) / sum_weights;
        seats.push(share);
        allocated += share;
    }
    // 将剩余票数（由于取整丢失）分配到权重最大的区间
    let mut diff = total_seats - allocated;
    if diff > 0 {
        // 按权重降序分配
        let mut indices: Vec<usize> = (0..weights.len()).collect();
        indices.sort_by_key(|&i| weights[i]);
        indices.reverse();
        for &i in &indices {
            if diff == 0 {
                break;
            }
            seats[i] += 1;
            diff -= 1;
        }
    }
    seats
}

// 加载 CSV 并构建 Train 列表（含动态余票初始化）
fn load_trains_from_csv(path: &str, sold_counts: &HashMap<(String, usize), u32>) -> Vec<Train> {
    let file = File::open(path).expect("无法打开 trains.csv 文件");
    let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(file);
    let mut trains = Vec::new();

    for result in rdr.records() {
        let record = result.expect("CSV 记录解析失败");
        let id = record[0].to_string();
        let model = record[1].to_string();
        let formation = record[2].to_string();

        let stations: Vec<String> = record[3].split('-').map(|s| s.to_string()).collect();
        let times: Vec<String> = record[4].split('-').map(|s| s.to_string()).collect();
        let prices: Vec<u32> = record[5]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let weights: Vec<u32> = record[6]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let total_seats: u32 = record[7].parse().unwrap_or(0);

        // 根据权重计算初始分配
        let initial_seats = compute_initial_seats(&weights, total_seats);

        // 减去已售
        let mut seats = initial_seats;
        for (i, seat) in seats.iter_mut().enumerate() {
            let key = (id.clone(), i);
            if let Some(&sold) = sold_counts.get(&key) {
                if sold >= *seat {
                    *seat = 0;
                } else {
                    *seat -= sold;
                }
            }
        }

        trains.push(Train {
            id,
            model,
            formation,
            stations,
            times,
            prices,
            weights,
            total_seats,
            seats,
        });
    }
    trains
}

// 加载通知文件
fn load_notifications(path: &str) -> Vec<Notification> {
    let content = fs::read_to_string(path).unwrap_or_else(|_| "".to_string());
    let lines: Vec<&str> = content.lines().collect();
    let mut notifs = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        let title = lines[i].trim().to_string();
        let content = lines[i + 1].trim().to_string();
        if !title.is_empty() || !content.is_empty() {
            notifs.push(Notification { title, content });
        }
        i += 2;
        // 跳过空行
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    notifs
}

// 获取文件修改时间
fn get_mtime(path: &str) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// 热重载逻辑
fn reload_all(state: &Arc<AppState>) {
    // 读锁获取已售数据
    let sold_counts = state.sold_counts.read().unwrap().clone();
    // 加载新 trains
    let new_trains = load_trains_from_csv("trains.csv", &sold_counts);
    // 加载新通知
    let new_notifs = load_notifications("notifications.txt");

    // 写锁更新状态
    {
        let mut trains = state.trains.write().unwrap();
        *trains = new_trains;
    }
    {
        let mut notifs = state.notifications.write().unwrap();
        *notifs = new_notifs;
    }
    // 更新文件修改时间记录
    {
        let mut mtime = state.train_mtime.write().unwrap();
        *mtime = get_mtime("trains.csv");
    }
    {
        let mut mtime = state.notif_mtime.write().unwrap();
        *mtime = get_mtime("notifications.txt");
    }
    println!("热重载完成: 更新了车次和通知数据");
}

// ---------- 路由处理 ----------

async fn index(State(state): State<Arc<AppState>>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "至冬国铁路 - 首页");
    let trains = state.trains.read().unwrap();

    // 收集所有站点
    let mut all_stations = HashSet::new();
    for train in trains.iter() {
        for station in &train.stations {
            all_stations.insert(station.clone());
        }
    }
    let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
    stations_vec.sort();
    ctx.insert("stations", &stations_vec);

    // 构造展示列表（首末站，取整体最小余票）
    let display_trains: Vec<serde_json::Value> = trains
        .iter()
        .map(|t| {
            let first = t.stations.first().unwrap();
            let last = t.stations.last().unwrap();
            let dep = t.times.first().unwrap();
            let arr = t.times.last().unwrap();
            let min_seats = t.seats.iter().min().cloned().unwrap_or(0);
            serde_json::json!({
                "id": t.id,
                "model": t.model,
                "from": first,
                "to": last,
                "departure": dep,
                "arrival": arr,
                "seats": min_seats,
            })
        })
        .collect();
    ctx.insert("display_trains", &display_trains);

    let today = Local::now().format("%Y-%m-%d").to_string();
    ctx.insert("today", &today);

    let html = state.tera.render("index.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Response {
    let trains = state.trains.read().unwrap();
    let from = params.from.trim();
    let to = params.to.trim();

    let mut results = Vec::new();
    for train in trains.iter() {
        if let Some((fi, ti)) = find_interval(train, from, to) {
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let departure = &train.times[fi];
            let arrival = &train.times[ti];
            results.push(serde_json::json!({
                "id": train.id,
                "name": train.id,
                "model": train.model,
                "formation": train.formation,
                "from": from,
                "to": to,
                "departure": departure,
                "arrival": arrival,
                "price": total_price,
                "seats": min_seats,
            }));
        }
    }

    let mut ctx = Context::new();
    ctx.insert("page_title", "搜索结果 - 至冬国铁路");
    ctx.insert("results", &results);
    ctx.insert("from", from);
    ctx.insert("to", to);
    let today = Local::now().format("%Y-%m-%d").to_string();
    ctx.insert("today", &today);

    let mut all_stations = HashSet::new();
    for train in trains.iter() {
        for station in &train.stations {
            all_stations.insert(station.clone());
        }
    }
    let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
    stations_vec.sort();
    ctx.insert("stations", &stations_vec);

    let html = state.tera.render("search_results.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn order_form(
    State(state): State<Arc<AppState>>,
    Path(train_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let from = params.get("from").cloned().unwrap_or_default();
    let to = params.get("to").cloned().unwrap_or_default();
    if from.is_empty() || to.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少出发站或到达站").into_response();
    }

    let trains = state.trains.read().unwrap();
    let train = trains.iter().find(|t| t.id == train_id).cloned();
    if let Some(train) = train {
        if let Some((fi, ti)) = find_interval(&train, &from, &to) {
            let departure = &train.times[fi];
            let arrival = &train.times[ti];
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let mut ctx = Context::new();
            ctx.insert("page_title", "填写乘客信息 - 至冬国铁路");
            ctx.insert("train", &train);
            ctx.insert("from", &from);
            ctx.insert("to", &to);
            ctx.insert("departure", departure);
            ctx.insert("arrival", arrival);
            ctx.insert("total_price", &total_price);
            ctx.insert("min_seats", &min_seats);
            let html = state.tera.render("order_form.html", &ctx).unwrap();
            return Html(html).into_response();
        } else {
            return (StatusCode::BAD_REQUEST, "无效的出发或到达站").into_response();
        }
    }
    (StatusCode::NOT_FOUND, "车次不存在").into_response()
}

async fn create_order(
    State(state): State<Arc<AppState>>,
    Form(form): Form<CreateOrderForm>,
) -> Response {
    // 余票检查和扣减
    let mut trains = state.trains.write().unwrap();
    let train = trains.iter_mut().find(|t| t.id == form.train_id);
    let train = match train {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "车次不存在").into_response(),
    };

    if let Some((fi, ti)) = find_interval(train, &form.from, &form.to) {
        // 检查每个区间余票是否足够
        for i in fi..ti {
            if train.seats[i] < form.seat_count {
                return (StatusCode::BAD_REQUEST, "区间余票不足").into_response();
            }
        }
        // 扣减区间余票
        for i in fi..ti {
            train.seats[i] -= form.seat_count;
        }
        // 更新已售计数
        let mut sold_counts = state.sold_counts.write().unwrap();
        for i in fi..ti {
            let key = (form.train_id.clone(), i);
            *sold_counts.entry(key).or_insert(0) += form.seat_count;
        }
    } else {
        return (StatusCode::BAD_REQUEST, "无效的站点区间").into_response();
    }

    // 生成订单
    let order_id = Uuid::new_v4().to_string();
    let expire_at = Utc::now() + Duration::minutes(30);
    let order = Order {
        id: order_id.clone(),
        train_id: form.train_id,
        from_station: form.from,
        to_station: form.to,
        passenger_name: form.passenger_name,
        seat_count: form.seat_count,
        price: form.total_price * form.seat_count,
        status: OrderStatus::Pending,
        expire_at: expire_at.timestamp(),
    };

    let mut orders = state.orders.write().unwrap();
    orders.insert(order_id.clone(), order);

    let mut ctx = Context::new();
    ctx.insert("page_title", "占座成功 - 至冬国铁路");
    ctx.insert("order_id", &order_id);
    ctx.insert("expire_at", &expire_at.timestamp());
    let html = state.tera.render("order_success.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn payment_page(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> Response {
    let orders = state.orders.read().unwrap();
    if let Some(order) = orders.get(&order_id) {
        let mut ctx = Context::new();
        ctx.insert("page_title", "支付 - 至冬国铁路");
        ctx.insert("order", order);
        let html = state.tera.render("payment.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

async fn pay_order(State(state): State<Arc<AppState>>, Path(order_id): Path<String>) -> Response {
    let mut orders = state.orders.write().unwrap();
    if let Some(order) = orders.get_mut(&order_id) {
        order.status = OrderStatus::Paid;
        Redirect::to("/").into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

async fn orders_query(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(order_id) = params.get("order_id") {
        let orders = state.orders.read().unwrap();
        let order = orders.get(order_id).cloned();
        let mut ctx = Context::new();
        ctx.insert("page_title", "订单查询结果 - 至冬国铁路");
        if let Some(order) = order {
            ctx.insert("order", &order);
        }
        let html = state.tera.render("order_result.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        let mut ctx = Context::new();
        ctx.insert("page_title", "订单查询 - 至冬国铁路");
        let html = state.tera.render("order_query.html", &ctx).unwrap();
        Html(html).into_response()
    }
}

// ---------- 通知页面 ----------
async fn notifications_page(State(state): State<Arc<AppState>>) -> Response {
    let notifs = state.notifications.read().unwrap();
    let mut ctx = Context::new();
    ctx.insert("page_title", "通知 - 至冬国铁路");
    ctx.insert("notifications", &*notifs);
    let html = state.tera.render("notifications.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 手动重载端点 ----------
async fn admin_reload(State(state): State<Arc<AppState>>) -> Response {
    reload_all(&state);
    (StatusCode::OK, "重载成功").into_response()
}

// ---------- 主函数 ----------
#[tokio::main]
async fn main() {
    // 初始化 Tera
    let tera = match Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    // 初始加载数据
    let sold_counts = HashMap::new();
    let trains = load_trains_from_csv("trains.csv", &sold_counts);
    let notifications = load_notifications("notifications.txt");

    let state = Arc::new(AppState {
        tera,
        trains: RwLock::new(trains),
        orders: RwLock::new(HashMap::new()),
        sold_counts: RwLock::new(sold_counts),
        notifications: RwLock::new(notifications),
        train_mtime: RwLock::new(get_mtime("trains.csv")),
        notif_mtime: RwLock::new(get_mtime("notifications.txt")),
    });

    // 启动热重载后台任务（每30秒检查一次文件变更）
    let state_clone = state.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(StdDuration::from_secs(30));
        loop {
            interval.tick().await;
            // 检查文件修改时间
            let train_mtime = get_mtime("trains.csv");
            let notif_mtime = get_mtime("notifications.txt");
            let mut need_reload = false;
            {
                let t_mtime = state_clone.train_mtime.read().unwrap();
                let n_mtime = state_clone.notif_mtime.read().unwrap();
                if train_mtime > *t_mtime || notif_mtime > *n_mtime {
                    need_reload = true;
                }
            }
            if need_reload {
                println!("检测到文件变更，执行热重载...");
                reload_all(&state_clone);
            }
        }
    });

    // 路由配置
    let app = Router::new()
        .route("/", get(index))
        .route("/search", get(search))
        .route("/order/create", post(create_order))
        .route("/order/{train_id}", get(order_form))
        .route("/orders", get(orders_query))
        .route("/payment/{order_id}/pay", get(pay_order))
        .route("/payment/{order_id}", get(payment_page))
        .route("/notifications", get(notifications_page))
        .route("/admin/reload", get(admin_reload))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
