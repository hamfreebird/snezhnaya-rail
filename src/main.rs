use axum::{
    Form, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{Duration, Local, Utc};
use csv::ReaderBuilder; // 用于读取 CSV
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File; // 文件操作
use std::sync::{Arc, RwLock};
use tera::{Context, Tera};
use tower_http::services::ServeDir;
use uuid::Uuid; // 用于去重站点

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Train {
    id: String,
    model: String,
    formation: String,
    stations: Vec<String>, // 站点列表
    times: Vec<String>,    // 时间列表
    prices: Vec<u32>,      // 区间票价（长度 = stations.len()-1）
    seats: Vec<u32>,       // 区间余票（长度 = stations.len()-1）
    available_seats: u32,
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
    Pending, // 占座中
    Paid,
    Cancelled,
}

// ---------- 应用状态 ----------
struct AppState {
    tera: Tera,
    trains: RwLock<Vec<Train>>,
    orders: RwLock<HashMap<String, Order>>, // order_id -> Order
}

// ---------- 查询参数 ----------
#[derive(Debug, Deserialize)]
struct SearchParams {
    from: String,
    to: String,
    date: Option<String>, // 忽略日期，仅用于演示
}

// ---------- 下单表单 ----------
#[derive(Debug, Deserialize)]
struct CreateOrderForm {
    train_id: String,
    from: String,
    to: String,
    total_price: u32,
    passenger_name: String,
    seat_count: u32,
}

fn load_trains_from_csv(path: &str) -> Vec<Train> {
    let file = File::open(path).expect("无法打开 trains.csv 文件");
    let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(file);
    let mut trains = Vec::new();

    for result in rdr.records() {
        let record = result.expect("CSV 记录解析失败");
        let id = record[0].to_string();
        let model = record[1].to_string();
        let formation = record[2].to_string();

        // 解析列表字段，按 '-' 分割
        let stations: Vec<String> = record[3].split('-').map(|s| s.to_string()).collect();
        let times: Vec<String> = record[4].split('-').map(|s| s.to_string()).collect();
        let prices: Vec<u32> = record[5]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let seats: Vec<u32> = record[6]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let available_seats = seats.iter().min().cloned().unwrap_or(0);

        // 数据完整性校验（可选）
        assert_eq!(stations.len(), times.len(), "站点与时间数量不一致");
        assert_eq!(stations.len() - 1, prices.len(), "票价数量不匹配");
        assert_eq!(stations.len() - 1, seats.len(), "余票数量不匹配");

        trains.push(Train {
            id,
            model,
            formation,
            stations,
            times,
            prices,
            seats,
            available_seats,
        });
    }
    trains
}

// 查找有效的出发-到达区间，处理重复站点
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

    // 构造展示列表（首末站）
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
                "model": t.model,          // 新增
                "from": first,
                "to": last,
                "departure": dep,
                "arrival": arr,
                "seats": min_seats,
            })
        })
        .collect();
    ctx.insert("display_trains", &display_trains);

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
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

    // 模板渲染部分保持不变
    let mut ctx = Context::new();
    ctx.insert("page_title", "搜索结果 - 至冬国铁路");
    ctx.insert("results", &results);
    ctx.insert("from", from);
    ctx.insert("to", to);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
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
        // 扣减所有区间余票
        for i in fi..ti {
            train.seats[i] -= form.seat_count;
        }
        // 更新全局余票（最小值）
        train.available_seats = train.seats.iter().min().cloned().unwrap_or(0);
    } else {
        return (StatusCode::BAD_REQUEST, "无效的站点区间").into_response();
    }

    // 生成订单（不变）
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

    // 渲染成功页（不变）
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

// 模拟支付完成（简单重定向）
async fn pay_order(State(state): State<Arc<AppState>>, Path(order_id): Path<String>) -> Response {
    let mut orders = state.orders.write().unwrap();
    if let Some(order) = orders.get_mut(&order_id) {
        order.status = OrderStatus::Paid;
        Redirect::to("/").into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

// 订单查询（支持直接通过 ?order_id=xxx 查询）
async fn orders_query(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // 如果传入了 order_id，则尝试查询并显示结果
    if let Some(order_id) = params.get("order_id") {
        let orders = state.orders.read().unwrap();
        let order = orders.get(order_id).cloned();
        let mut ctx = Context::new();
        ctx.insert("page_title", "订单查询结果 - 至冬国铁路");
        if let Some(order) = order {
            ctx.insert("order", &order);
        } else {
            // 未找到，仍渲染 result 模板，但 order 为 None（模板中判断）
            // 注意：模板中需要处理 order 为空的情况，我们传一个空值或不传
        }
        let html = state.tera.render("order_result.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        let mut ctx = Context::new();
        // 插入 page_title（关键！）
        ctx.insert("page_title", "订单查询 - 至冬国铁路");
        let html = state.tera.render("order_query.html", &ctx).unwrap();
        Html(html).into_response()
    }
}

// ---------- 启动服务 ----------
#[tokio::main]
async fn main() {
    // 直接在 new 时传入模板路径的 glob 模式
    let tera = match Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    let trains = load_trains_from_csv("trains.csv");

    let state = Arc::new(AppState {
        tera,
        trains: RwLock::new(trains),
        orders: RwLock::new(HashMap::new()),
    });

    // 构建路由
    let app = Router::new()
        .route("/", get(index))
        .route("/search", get(search))
        .route("/order/create", post(create_order)) // 具体路径优先
        .route("/order/{train_id}", get(order_form))
        .route("/orders", get(orders_query)) // 显示表单，若带参数则直接查询
        .route("/payment/{order_id}/pay", get(pay_order)) // 更具体的路径优先
        .route("/payment/{order_id}", get(payment_page))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
