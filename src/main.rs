use axum::{
    Form, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{Duration, Local, NaiveDate, NaiveTime, Utc};
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
    weights: Vec<u32>,
    total_seats: u32,
    seats: Vec<u32>,
    is_departed: bool,
    is_selling: bool,
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
    created_at: i64,
    pay_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum OrderStatus {
    Pending,
    Paid,
    Cancelled,
    Refunded,
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
    sold_counts: RwLock<HashMap<(String, usize), u32>>,
    notifications: RwLock<Vec<Notification>>,
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
    date: String,
}

// ---------- 辅助函数 ----------
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

// 动态余票因子：基于天数差和小时差
fn compute_availability_factor(days_diff: i64, hours_diff: f64) -> f64 {
    if days_diff < 0 {
        return 0.0; // 已发车
    }
    if days_diff > 7 {
        return 0.0; // 超过7天不可售票，但余票计算中不直接用于可售性，仅供显示
    }
    if days_diff == 0 {
        // 当天
        if hours_diff < 0.5 {
            return 0.0; // 30分钟内停止售票
        } else if hours_diff >= 6.0 {
            return 1.0;
        } else {
            let base = (hours_diff - 0.5) / 5.5;
            let fluctuation = 0.1 * ((6.0 - hours_diff) * std::f64::consts::PI / 2.0).sin();
            (base + fluctuation).clamp(0.0, 1.0)
        }
    } else {
        // 1~7天：全量，但略微递减
        (1.0 - (days_diff as f64 * 0.01)).clamp(0.85, 1.0)
    }
}

fn update_train_status_with_date(train: &mut Train, base_date: &NaiveDate) {
    let now = Local::now().naive_local();
    let today = now.date();
    let days_diff = (*base_date - today).num_days();

    // 1. 已发车：选择日期早于今天
    if days_diff < 0 {
        train.is_departed = true;
        train.is_selling = false;
        return;
    }

    // 2. 超过7天不可售票
    if days_diff > 7 {
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    // 3. 今天：检查时间
    if days_diff == 0 {
        if let Some(first_time_str) = train.times.first() {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                // 已发车
                if now.time() >= first_time {
                    train.is_departed = true;
                    train.is_selling = false;
                    return;
                }
                // 距离发车少于30分钟停止售票
                let diff_secs = (first_time - now.time()).num_seconds();
                if diff_secs < 1800 {
                    train.is_selling = false;
                    train.is_departed = false;
                    return;
                }
                // ✅ 关键：可售票状态
                train.is_selling = true;
                train.is_departed = false;
                return;
            }
        }
        // 时间解析失败，安全起见不可售
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    // 4. 未来1~7天：可售票
    train.is_selling = true;
    train.is_departed = false;
}

// 计算区间余票（基于基准日期）
fn compute_interval_seats_with_date(
    train: &Train,
    sold_counts: &HashMap<(String, usize), u32>,
    base_date: &NaiveDate,
) -> Vec<u32> {
    let total = train.total_seats;
    let weights = &train.weights;
    let sum_w: u32 = weights.iter().sum();

    // 使用最大余数法分配席位
    let mut allocated = vec![0u32; weights.len()];
    if sum_w == 0 {
        // 所有权重为0，平均分配
        let base = total / (weights.len() as u32);
        let rem = total % (weights.len() as u32);
        for i in 0..weights.len() {
            allocated[i] = base;
            if i < rem as usize {
                allocated[i] += 1;
            }
        }
    } else {
        // 计算每个区间的基础分配和余数
        let mut floors = Vec::with_capacity(weights.len());
        let mut fractions = Vec::with_capacity(weights.len());
        for &w in weights {
            let share = (total as f64 * w as f64) / sum_w as f64;
            let floor = share.floor() as u32;
            floors.push(floor);
            fractions.push(share - floor as f64); // 修复：将 floor 转为 f64
        }
        // 先分配整数部分
        let mut allocated_sum: u32 = floors.iter().sum();
        for i in 0..weights.len() {
            allocated[i] = floors[i];
        }
        // 按余数大小分配剩余席位
        let mut remaining = total - allocated_sum;
        if remaining > 0 {
            // 创建索引并按余数降序排列
            let mut indices: Vec<usize> = (0..weights.len()).collect();
            indices.sort_by(|&a, &b| fractions[b].partial_cmp(&fractions[a]).unwrap());
            for &i in &indices {
                if remaining == 0 { break; }
                allocated[i] += 1;
                remaining -= 1;
            }
        }
    }

    let now = Local::now().naive_local();
    let days_diff = (*base_date - now.date()).num_days();

    // 时间因子（不含已售影响）
    let base_factor = if days_diff < 0 {
    0.0
} else if days_diff == 0 {
    // 当天因子：最高0.15，发车前2小时内线性下降
    let mut factor = 0.15;
    if let Some(first_time_str) = train.times.first() {
        if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
            let dep_datetime = base_date.and_time(first_time);
            let mins_left = (dep_datetime - now).num_minutes();
            if mins_left <= 0 {
                factor = 0.0;
            } else if mins_left < 120 {
                factor = 0.15 * (mins_left as f64 / 120.0);
            }
        }
    }
    factor
} else if days_diff >= 1 && days_diff <= 7 {
    match days_diff {
        1 => 0.2,  // 第二天 20%
        2 => 0.6,  // 第三天 60%
        _ => 1.0,  // 第四天起 100% (达到上限)
    }
} else {
    1.0
};

    // 余票 = max(0, 分配票数 * 因子 - 已售)
    let mut seats = Vec::new();
    for (i, &alloc) in allocated.iter().enumerate() {
        let key = (train.id.clone(), i);
        let sold = sold_counts.get(&key).cloned().unwrap_or(0);
        let max_avail = (alloc as f64 * base_factor) as u32;
        let avail = if max_avail >= sold { max_avail - sold } else { 0 };
        seats.push(avail);
    }

    seats
}

// 加载静态数据
fn load_trains_static(path: &str) -> Vec<Train> {
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

        trains.push(Train {
            id,
            model,
            formation,
            stations,
            times,
            prices,
            weights,
            total_seats,
            seats: vec![],
            is_departed: false,
            is_selling: false,
        });
    }
    trains
}

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
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    notifs
}

fn get_mtime(path: &str) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// 刷新列车数据（基于日期）
fn refresh_trains_with_date(state: &Arc<AppState>, base_date: &NaiveDate) {
    let sold_counts = state.sold_counts.read().unwrap().clone();
    let mut trains = state.trains.write().unwrap();
    for train in trains.iter_mut() {
        train.seats = compute_interval_seats_with_date(train, &sold_counts, base_date);
        update_train_status_with_date(train, base_date);
    }
}

// 热重载
fn reload_all(state: &Arc<AppState>) {
    let sold_counts = state.sold_counts.read().unwrap().clone();
    let new_trains = load_trains_static("trains.csv");
    let new_notifs = load_notifications("notifications.txt");
    let now_date = Local::now().date_naive();
    let mut updated_trains = Vec::new();
    for mut train in new_trains {
        train.seats = compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        update_train_status_with_date(&mut train, &now_date);
        updated_trains.push(train);
    }
    *state.trains.write().unwrap() = updated_trains;
    *state.notifications.write().unwrap() = new_notifs;
    *state.train_mtime.write().unwrap() = get_mtime("trains.csv");
    *state.notif_mtime.write().unwrap() = get_mtime("notifications.txt");
    println!("热重载完成");
}

// ---------- 路由处理 ----------

async fn index(State(state): State<Arc<AppState>>) -> Response {
    let today = Local::now().date_naive();
    refresh_trains_with_date(&state, &today);
    let mut ctx = Context::new();
    ctx.insert("page_title", "至冬国铁路 - 首页");
    let trains = state.trains.read().unwrap();

    let mut all_stations = HashSet::new();
    for train in trains.iter() {
        for station in &train.stations {
            all_stations.insert(station.clone());
        }
    }
    let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
    stations_vec.sort();
    ctx.insert("stations", &stations_vec);

    let today_str = today.format("%Y-%m-%d").to_string();
    ctx.insert("selected_date", &today_str);

    let mut display_trains: Vec<serde_json::Value> = trains
        .iter()
        .filter(|t| t.is_selling || !t.is_departed)
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
                "is_selling": t.is_selling,
                "is_departed": t.is_departed,
            })
        })
        .collect();

    display_trains.sort_by(|a, b| {
        let a_sell = a["is_selling"].as_bool().unwrap_or(false);
        let b_sell = b["is_selling"].as_bool().unwrap_or(false);
        let a_dep = a["is_departed"].as_bool().unwrap_or(false);
        let b_dep = b["is_departed"].as_bool().unwrap_or(false);
        if a_sell && !b_sell {
            return std::cmp::Ordering::Less;
        }
        if !a_sell && b_sell {
            return std::cmp::Ordering::Greater;
        }
        if a_dep && !b_dep {
            return std::cmp::Ordering::Greater;
        }
        if !a_dep && b_dep {
            return std::cmp::Ordering::Less;
        }
        a["id"].as_str().unwrap().cmp(b["id"].as_str().unwrap())
    });

    ctx.insert("display_trains", &display_trains);

    let html = state.tera.render("index.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Response {
    let date_str = params
        .date
        .clone()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let base_date =
        NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap_or(Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date);
    let trains = state.trains.read().unwrap();
    let from = params.from.trim();
    let to = params.to.trim();

    let mut results = Vec::new();
    for train in trains.iter() {
        if !train.is_selling && !train.is_departed {
            continue;
        }
        if let Some((fi, ti)) = find_interval(train, from, to) {
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let departure = &train.times[fi];
            let arrival = &train.times[ti];
            results.push(serde_json::json!({
                "id": train.id,
                "model": train.model,
                "from": from,
                "to": to,
                "departure": departure,
                "arrival": arrival,
                "price": total_price,
                "seats": min_seats,
                "is_selling": train.is_selling,
                "is_departed": train.is_departed,
            }));
        }
    }

    results.sort_by(|a, b| {
        let a_sell = a["is_selling"].as_bool().unwrap_or(false);
        let b_sell = b["is_selling"].as_bool().unwrap_or(false);
        let a_dep = a["is_departed"].as_bool().unwrap_or(false);
        let b_dep = b["is_departed"].as_bool().unwrap_or(false);
        if a_sell && !b_sell {
            return std::cmp::Ordering::Less;
        }
        if !a_sell && b_sell {
            return std::cmp::Ordering::Greater;
        }
        if a_dep && !b_dep {
            return std::cmp::Ordering::Greater;
        }
        if !a_dep && b_dep {
            return std::cmp::Ordering::Less;
        }
        a["id"].as_str().unwrap().cmp(b["id"].as_str().unwrap())
    });

    let mut ctx = Context::new();
    ctx.insert("page_title", "搜索结果 - 至冬国铁路");
    ctx.insert("results", &results);
    ctx.insert("from", from);
    ctx.insert("to", to);
    ctx.insert("selected_date", &date_str);

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
    let date_str = params
        .get("date")
        .cloned()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let base_date =
        NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap_or(Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date);

    if from.is_empty() || to.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少出发站或到达站").into_response();
    }

    let trains = state.trains.read().unwrap();
    let train = trains.iter().find(|t| t.id == train_id).cloned();
    if let Some(train) = train {
        if !train.is_selling {
            return (StatusCode::BAD_REQUEST, "该车次已停止售票或已发车").into_response();
        }
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
            ctx.insert("selected_date", &date_str);
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
    let base_date = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d")
        .unwrap_or(Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date);

    let mut trains = state.trains.write().unwrap();
    let train = trains.iter_mut().find(|t| t.id == form.train_id);
    let train = match train {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "车次不存在").into_response(),
    };
    if !train.is_selling {
        return (StatusCode::BAD_REQUEST, "该车次已停止售票").into_response();
    }

    if let Some((fi, ti)) = find_interval(train, &form.from, &form.to) {
        for i in fi..ti {
            if train.seats[i] < form.seat_count {
                return (StatusCode::BAD_REQUEST, "区间余票不足").into_response();
            }
        }
        for i in fi..ti {
            train.seats[i] -= form.seat_count;
        }
        let mut sold_counts = state.sold_counts.write().unwrap();
        for i in fi..ti {
            let key = (form.train_id.clone(), i);
            *sold_counts.entry(key).or_insert(0) += form.seat_count;
        }
    } else {
        return (StatusCode::BAD_REQUEST, "无效的站点区间").into_response();
    }

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
        created_at: Utc::now().timestamp(),
        pay_time: None,
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
        if order.status != OrderStatus::Pending {
            return (StatusCode::BAD_REQUEST, "订单状态异常").into_response();
        }
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
        if order.status == OrderStatus::Pending {
            order.status = OrderStatus::Paid;
            order.pay_time = Some(Utc::now().timestamp());
            Redirect::to("/").into_response()
        } else {
            (StatusCode::BAD_REQUEST, "订单已处理").into_response()
        }
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

async fn notifications_page(State(state): State<Arc<AppState>>) -> Response {
    let notifs = state.notifications.read().unwrap();
    let mut ctx = Context::new();
    ctx.insert("page_title", "通知 - 至冬国铁路");
    ctx.insert("notifications", &*notifs);
    let html = state.tera.render("notifications.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 改签/退签 ----------
async fn exchange_page(State(state): State<Arc<AppState>>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "改签/退签 - 至冬国铁路");
    let html = state.tera.render("exchange.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn exchange_query(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }
    let orders = state.orders.read().unwrap();
    if let Some(order) = orders.get(&order_id) {
        let mut ctx = Context::new();
        ctx.insert("page_title", "改签/退签 - 至冬国铁路");
        ctx.insert("order", order);
        // 判断是否可改签/退签：已支付且未发车
        let now = Local::now().naive_local();
        let trains = state.trains.read().unwrap();
        let train = trains.iter().find(|t| t.id == order.train_id);
        let can_operate = if let Some(t) = train {
            if let Some(first_time_str) = t.times.first() {
                if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                    let base_date = now.date();
                    let dep_datetime = base_date.and_time(first_time);
                    if now < dep_datetime && order.status == OrderStatus::Paid {
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        ctx.insert("can_operate", &can_operate);
        let html = state.tera.render("exchange_detail.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

async fn exchange_refund(
    State(state): State<Arc<AppState>>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }

    let mut orders = state.orders.write().unwrap();
    if let Some(order) = orders.get_mut(&order_id) {
        if order.status != OrderStatus::Paid {
            return (StatusCode::BAD_REQUEST, "订单状态不可退签").into_response();
        }
        // 检查是否已发车
        let now = Local::now().naive_local();
        let trains = state.trains.read().unwrap();
        let train = trains.iter().find(|t| t.id == order.train_id);
        if let Some(t) = train {
            if let Some(first_time_str) = t.times.first() {
                if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                    let base_date = now.date();
                    let dep_datetime = base_date.and_time(first_time);
                    if now >= dep_datetime {
                        return (StatusCode::BAD_REQUEST, "已发车，不可退签").into_response();
                    }
                }
            }
        }

        order.status = OrderStatus::Refunded;

        // 释放余票
        let mut sold_counts = state.sold_counts.write().unwrap();
        let train_id = order.train_id.clone();
        let from = &order.from_station;
        let to = &order.to_station;
        let trains = state.trains.read().unwrap();
        if let Some(train) = trains.iter().find(|t| t.id == train_id) {
            if let Some((fi, ti)) = find_interval(train, from, to) {
                for i in fi..ti {
                    let key = (train_id.clone(), i);
                    if let Some(sold) = sold_counts.get_mut(&key) {
                        if *sold >= order.seat_count {
                            *sold -= order.seat_count;
                        } else {
                            *sold = 0;
                        }
                    }
                }
            }
        }
        // 刷新余票（基于当前日期）
        let today = Local::now().date_naive();
        refresh_trains_with_date(&state, &today);

        let mut ctx = Context::new();
        ctx.insert("page_title", "退签成功 - 至冬国铁路");
        ctx.insert("message", "退签成功，已释放座位");
        ctx.insert("order_id", &order_id);
        let html = state.tera.render("exchange_result.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

async fn exchange_change(
    State(state): State<Arc<AppState>>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    let new_train_id = params.get("new_train_id").cloned().unwrap_or_default();
    let new_from = params.get("new_from").cloned().unwrap_or_default();
    let new_to = params.get("new_to").cloned().unwrap_or_default();
    let new_seat_count = params
        .get("new_seat_count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);

    if order_id.is_empty() || new_train_id.is_empty() || new_from.is_empty() || new_to.is_empty() {
        return (StatusCode::BAD_REQUEST, "参数不完整").into_response();
    }

    // 写锁原子操作
    let mut orders = state.orders.write().unwrap();
    let mut sold_counts = state.sold_counts.write().unwrap();
    let mut trains = state.trains.write().unwrap();

    // 1. 验证原订单
    let order = orders.get(&order_id).cloned();
    let order = match order {
        Some(o) => {
            if o.status != OrderStatus::Paid {
                return (StatusCode::BAD_REQUEST, "原订单状态不可改签").into_response();
            }
            o
        }
        None => return (StatusCode::NOT_FOUND, "原订单不存在").into_response(),
    };

    // 检查原订单是否已发车
    if let Some(train) = trains.iter().find(|t| t.id == order.train_id) {
        if let Some(first_time_str) = train.times.first() {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                let now = Local::now().naive_local();
                let base_date = now.date();
                let dep_datetime = base_date.and_time(first_time);
                if now >= dep_datetime {
                    return (StatusCode::BAD_REQUEST, "已发车，不可改签").into_response();
                }
            }
        }
    }

    // 2. 释放原票
    if let Some(train) = trains.iter().find(|t| t.id == order.train_id) {
        if let Some((fi, ti)) = find_interval(train, &order.from_station, &order.to_station) {
            for i in fi..ti {
                let key = (order.train_id.clone(), i);
                if let Some(sold) = sold_counts.get_mut(&key) {
                    if *sold >= order.seat_count {
                        *sold -= order.seat_count;
                    } else {
                        *sold = 0;
                    }
                }
            }
        }
    }

    // 3. 购买新车票（基于当前日期，改签只能今天以后）
    let today = Local::now().date_naive();
    let new_train = trains.iter_mut().find(|t| t.id == new_train_id);
    let new_train = match new_train {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "新车次不存在").into_response(),
    };
    // 刷新新车次状态（基于今天）
    let sold_counts_ref = sold_counts.clone();
    new_train.seats = compute_interval_seats_with_date(new_train, &sold_counts_ref, &today);
    update_train_status_with_date(new_train, &today);
    if !new_train.is_selling {
        return (StatusCode::BAD_REQUEST, "新车次已停止售票或已发车").into_response();
    }

    if let Some((fi, ti)) = find_interval(new_train, &new_from, &new_to) {
        for i in fi..ti {
            if new_train.seats[i] < new_seat_count {
                return (StatusCode::BAD_REQUEST, "新车次余票不足").into_response();
            }
        }
        for i in fi..ti {
            new_train.seats[i] -= new_seat_count;
        }
        for i in fi..ti {
            let key = (new_train_id.clone(), i);
            *sold_counts.entry(key).or_insert(0) += new_seat_count;
        }
        let total_price: u32 = new_train.prices[fi..ti].iter().sum();
        let new_price = total_price * new_seat_count;

        // 4. 更新原订单
        let order_mut = orders.get_mut(&order_id).unwrap();
        order_mut.train_id = new_train_id.clone();
        order_mut.from_station = new_from.clone();
        order_mut.to_station = new_to.clone();
        order_mut.seat_count = new_seat_count;
        order_mut.price = new_price;
    } else {
        return (StatusCode::BAD_REQUEST, "新车次无效区间").into_response();
    }

    // 刷新所有列车（基于今天）
    refresh_trains_with_date(&state, &today);

    let mut ctx = Context::new();
    ctx.insert("page_title", "改签成功 - 至冬国铁路");
    ctx.insert("message", "改签成功，请查看新订单");
    ctx.insert("order_id", &order_id);
    let html = state.tera.render("exchange_result.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 管理端点 ----------
async fn admin_reload(State(state): State<Arc<AppState>>) -> Response {
    reload_all(&state);
    (StatusCode::OK, "重载成功").into_response()
}

// ---------- 主函数 ----------
#[tokio::main]
async fn main() {
    let tera = match Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    let sold_counts = HashMap::new();
    let static_trains = load_trains_static("trains.csv");
    let now_date = Local::now().date_naive();
    let mut trains = Vec::new();
    for mut train in static_trains {
        train.seats = compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        update_train_status_with_date(&mut train, &now_date);
        trains.push(train);
    }

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

    // 路由
    let app = Router::new()
        .route("/", get(index))
        .route("/search", get(search))
        .route("/order/create", post(create_order))
        .route("/order/{train_id}", get(order_form))
        .route("/orders", get(orders_query))
        .route("/payment/{order_id}/pay", get(pay_order))
        .route("/payment/{order_id}", get(payment_page))
        .route("/notifications", get(notifications_page))
        .route("/exchange", get(exchange_page))
        .route("/exchange/query", get(exchange_query))
        .route("/exchange/refund", post(exchange_refund))
        .route("/exchange/change", post(exchange_change))
        .route("/admin/reload", get(admin_reload))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
