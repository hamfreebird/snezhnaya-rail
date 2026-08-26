use ab_glyph::{FontArc, PxScale};
use axum::{
    Form, Router, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveTime, Utc};
use csv::ReaderBuilder;
use image::{ImageFormat, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use qrcode::QrCode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration as StdDuration, SystemTime};
use tera::{Context, Tera};
use tokio::time;
use tower_governor::{governor::GovernorConfigBuilder};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use uuid::Uuid;
use tracing::{info, debug, error};
use tracing_subscriber::{
    layer::SubscriberExt, 
    util::SubscriberInitExt, 
    filter::EnvFilter
};
use tracing_appender::non_blocking::WorkerGuard;

// ---------- 数据结构 ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Train {
    // 唯一ID
    pub id: String,
    // 原始车次号
    pub base_train_id: String,
    // 列车名称
    pub name: String,
    pub model: String,
    pub formation: String,
    // 站点列表
    pub stations: Vec<String>,
    // 站台列表
    pub platforms: Vec<String>,
    // 到达/出发时间
    pub arrival_times: Vec<Option<String>>,
    pub departure_times: Vec<Option<String>>,
    // 区间票价
    pub prices: Vec<u32>,
    // 权重
    pub weights: Vec<u32>,
    // 总座位数（所有车厢总和）
    pub total_seats: u32,
    // 车厢数
    pub carriage_count: u32,
    // 每车厢座位数
    pub seats_per_carriage: u32,
    // 每个区间的可用座位号列表（初始按权重分配，后动态调整）
    pub available_seats: Vec<Vec<u32>>,
    // 每个区间的余票数量（动态计算）
    pub seats: Vec<u32>, 
    // 状态
    pub is_departed: bool,
    pub is_selling: bool,
    // 运行日
    pub run_day: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    train_id: String,
    from_station: String,
    to_station: String,
    from_platform: String,
    passenger_name: String,
    seat_count: u32,
    price: u32,
    status: OrderStatus,
    expire_at: i64,
    created_at: i64,
    pay_time: Option<i64>,
    travel_date: String,
    pub group_id: Option<String>,
    #[serde(default)]
    seat_numbers: Vec<u32>,
    #[serde(default = "default_category")]
    seat_category: String,
    #[serde(default)]
    carriage_index: u32,
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

#[derive(Debug, Clone, Serialize)]
pub struct ArrivalInfo {
    train_id: String,
    base_train_id: String,
    from_station: String,
    to_station: String,
    departure_time: String,
    arrival_time: String,
    price: u32,
    min_seats: u32,
    fi: usize,
    ti: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DepartureInfo {
    train_id: String,
    base_train_id: String,
    from_station: String,
    to_station: String,
    departure_time: String,
    arrival_time: String,
    price: u32,
    min_seats: u32,
    fi: usize,
    ti: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferPlan {
    pub first_train: ArrivalInfo,
    pub second_train: DepartureInfo,
    pub transfer_station: String,
    pub transfer_wait: i64,
    pub total_duration: i64,
    pub total_price: u32,
    pub min_seats: u32,
}

// ---------- 应用状态 ----------
struct AppState {
    tera: Tera,
    trains: RwLock<Vec<Train>>,
    orders: RwLock<HashMap<String, Order>>,
    sold_counts: RwLock<HashMap<(String, usize), HashSet<u32>>>,
    notifications: RwLock<Vec<Notification>>,
    train_mtime: RwLock<SystemTime>,
    notif_mtime: RwLock<SystemTime>,
    order_index: RwLock<HashMap<String, Vec<String>>>,
}

// ---------- 查询参数 ----------
#[derive(Debug, Deserialize)]
struct SearchParams {
    from: String,
    to: String,
    date: Option<String>,
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateOrderForm {
    pub train_id: String,
    pub from: String,
    pub from_platform: String,
    pub to: String,
    pub total_price: u32,
    pub passenger_name: String,
    pub seat_count: u32,
    pub date: String,
    pub carriage_index: u32, 
    pub seat_category: String
}

#[derive(Debug, Deserialize)]
struct TransferOrderForm {
    pub first_train_id: String,
    pub first_from: String,
    pub first_to: String,
    pub second_train_id: String,
    pub second_from: String,
    pub second_to: String,
    pub seat_count: u32,
    pub passenger_name: String,
    pub date: String,
    pub carriage_index: u32,
    pub seat_category: String,
}

// ---------- 辅助函数 ----------
fn default_category() -> String {
    "corridor".to_string() 
}

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

/// 查找所有可行的中转换乘方案
/// 返回 Vec<TransferPlan>，按总时长升序
fn find_transfers(
    trains: &[Train],
    from: &str,
    to: &str,
    travel_date: &NaiveDate,
    min_transfer: i64, // 分钟
) -> Vec<TransferPlan> {
    let weekday = travel_date.weekday().number_from_monday() as u8;
    let now = Local::now().naive_local();

    // 筛选当日可售车次
    let available: Vec<&Train> = trains
        .iter()
        .filter(|t| t.run_day == weekday && t.is_selling && !t.is_departed)
        .collect();

    // 1. 构建到达换乘站索引: Map<换乘站, Vec<(车次, 到达时间, 区间票价, 最小余票, 区间索引)>>
    let mut arrival_map: HashMap<String, Vec<ArrivalInfo>> = HashMap::new();
    for train in &available {
        if let Some(fi) = train.stations.iter().position(|s| s == from) {
            for i in (fi + 1)..train.stations.len() {
                let x = &train.stations[i];
                let arrival_time = train.arrival_times[i].as_ref().unwrap();
                let price: u32 = train.prices[fi..i].iter().sum();
                let min_seats = train.seats[fi..i].iter().min().cloned().unwrap_or(0);
                // 检查是否已发车（对应当天日期）
                if let Ok(arr_dt) = NaiveTime::parse_from_str(arrival_time, "%H:%M") {
                    let dep_datetime = travel_date.and_time(arr_dt);
                    if now >= dep_datetime {
                        continue; // 已过到达时间，不可用
                    }
                }
                arrival_map.entry(x.clone()).or_default().push(ArrivalInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: from.to_string(),
                    to_station: x.clone(),
                    departure_time: train.departure_times[fi].as_ref().unwrap().clone(),
                    arrival_time: arrival_time.clone(),
                    price,
                    min_seats,
                    fi,
                    ti: i,
                });
            }
        }
    }

    // 2. 构建出发换乘站索引: Map<换乘站, Vec<(车次, 出发时间, 区间票价, 最小余票, 区间索引)>>
    let mut departure_map: HashMap<String, Vec<DepartureInfo>> = HashMap::new();
    for train in &available {
        if let Some(ti) = train.stations.iter().position(|s| s == to) {
            for i in 0..ti {
                let x = &train.stations[i];
                let dep_time = train.departure_times[i].as_ref().unwrap();
                let price: u32 = train.prices[i..ti].iter().sum();
                let min_seats = train.seats[i..ti].iter().min().cloned().unwrap_or(0);
                // 检查是否已发车
                if let Ok(dep_dt) = NaiveTime::parse_from_str(dep_time, "%H:%M") {
                    let dep_datetime = travel_date.and_time(dep_dt);
                    if now >= dep_datetime {
                        continue;
                    }
                }
                departure_map.entry(x.clone()).or_default().push(DepartureInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: x.clone(),
                    to_station: to.to_string(),
                    departure_time: dep_time.clone(),
                    arrival_time: train.arrival_times[ti].as_ref().unwrap().clone(),
                    price,
                    min_seats,
                    fi: i,
                    ti,
                });
            }
        }
    }

    // 3. 匹配换乘组合
    let mut plans = Vec::new();
    for (x, arrivals) in arrival_map {
        if let Some(departures) = departure_map.get(&x) {
            for a in arrivals {
                for d in departures {
                    // 同一车次不允许（避免同一班次折返）
                    if a.train_id == d.train_id {
                        continue;
                    }
                    // 计算换乘等待时间
                    let arr_time = NaiveTime::parse_from_str(&a.arrival_time, "%H:%M").unwrap();
                    let dep_time = NaiveTime::parse_from_str(&d.departure_time, "%H:%M").unwrap();
                    // 假设同一天，若跨天则需额外处理，但此处简化
                    let wait_minutes = (dep_time - arr_time).num_minutes();
                    if wait_minutes < min_transfer {
                        continue;
                    }
                    // 总时长 = 第一段运行时间 + 等待 + 第二段运行时间
                    let first_dep = NaiveTime::parse_from_str(&a.departure_time, "%H:%M").unwrap();
                    let first_dur = (NaiveTime::parse_from_str(&a.arrival_time, "%H:%M").unwrap() - first_dep).num_minutes();
                    let second_dep = NaiveTime::parse_from_str(&d.departure_time, "%H:%M").unwrap();
                    let second_dur = (NaiveTime::parse_from_str(&d.arrival_time, "%H:%M").unwrap() - second_dep).num_minutes();
                    let total_duration = first_dur + wait_minutes + second_dur;

                    let total_price = a.price + d.price;
                    let min_seats = a.min_seats.min(d.min_seats);

                    plans.push(TransferPlan {
                        first_train: a.clone(),
                        second_train: d.clone(),
                        transfer_station: x.clone(),
                        transfer_wait: wait_minutes,
                        total_duration,
                        total_price,
                        min_seats,
                    });
                }
            }
        }
    }

    // 按总时长升序
    plans.sort_by_key(|p| p.total_duration);
    plans.retain(|p| p.min_seats > 0);
    plans.truncate(50); // 最多返回20条
    plans
}

/// 生成一个8位数字订单号，确保与现有订单不重复
fn generate_unique_order_id(orders: &HashMap<String, Order>) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let id = format!("{:08}", rng.gen_range(0..100000000));
        if !orders.contains_key(&id) {
            return id;
        }
    }
}

// 初始化日志系统，返回 _guard 用于保持日志写入器存活
fn init_logging() -> WorkerGuard {
    // 1. 日志级别（可通过环境变量 RUST_LOG 覆盖）
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,snezhnaya_rail=debug"));

    // 2. 控制台输出（漂亮格式）
    let console_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_target(true)
        .with_thread_names(true)
        .with_level(true)
        .pretty();

    // 3. 文件输出（JSON 格式，按日轮转）
    let file_appender = tracing_appender::rolling::daily("logs", "snezhnaya-rail.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .json();  // 现在可以调用了

    // 4. 注册
    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    info!("日志系统初始化完成");
    guard
}

/// 在指定区间内分配座位，返回分配的座位号列表
/// 若余票不足，返回 None
fn allocate_seats_for_interval(
    train: &mut Train,
    fi: usize,
    ti: usize,
    carriage_index: u32,
    seat_category: &str,
    seat_count: u32,
) -> Option<Vec<u32>> {
    let seats_per_carriage = train.seats_per_carriage;
    // 收集所有区间座位
    let mut all_seats: Vec<u32> = Vec::new();
    for i in fi..ti {
        all_seats.extend(train.available_seats[i].iter().cloned());
    }
    // 筛选车厢
    let in_carriage: Vec<u32> = all_seats
        .into_iter()
        .filter(|&s| (s - 1) / seats_per_carriage + 1 == carriage_index)
        .collect();
    // 筛选类别（单数靠走廊，双数靠窗）
    let filtered: Vec<u32> = if seat_category == "corridor" {
        in_carriage.into_iter().filter(|&s| s % 2 == 1).collect()
    } else {
        in_carriage.into_iter().filter(|&s| s % 2 == 0).collect()
    };
    if filtered.len() < seat_count as usize {
        return None;
    }
    let assigned: Vec<u32> = filtered.into_iter().take(seat_count as usize).collect();
    // 从各区间中移除已分配座位
    for i in fi..ti {
        train.available_seats[i].retain(|s| !assigned.contains(s));
    }
    Some(assigned)
}

fn release_seats(
    train_id: &str,
    from: &str,
    to: &str,
    seat_numbers: &[u32],
    trains: &mut [Train],
    sold_counts: &mut HashMap<(String, usize), HashSet<u32>>,
) {
    if let Some(train) = trains.iter_mut().find(|t| t.id == train_id) {
        if let Some((fi, ti)) = find_interval(train, from, to) {
            for i in fi..ti {
                // 将座位加回 available_seats
                for &s in seat_numbers {
                    train.available_seats[i].push(s);
                }
                train.available_seats[i].sort();
                train.available_seats[i].dedup();
                // 从 sold_counts 中移除
                let key = (train_id.to_string(), i);
                if let Some(set) = sold_counts.get_mut(&key) {
                    for &s in seat_numbers {
                        set.remove(&s);
                    }
                }
            }
        }
    }
}

fn save_order_index(index: &HashMap<String, Vec<String>>) {
    let data = serde_json::to_string_pretty(index).expect("序列化索引失败");
    fs::write("order_index.json", data).expect("写入索引文件失败");
}

// 加载所有订单
fn load_orders() -> HashMap<String, Order> {
    let path = "orders.json";
    if let Ok(data) = fs::read_to_string(path) {
        serde_json::from_str(&data).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    }
}

// 保存所有订单
fn save_orders(orders: &HashMap<String, Order>) {
    let data = serde_json::to_string_pretty(orders).expect("序列化订单失败");
    fs::write("orders.json", data).expect("写入订单文件失败");
}

fn compute_interval_seats_with_date(
    train: &Train,
    sold_counts: &HashMap<(String, usize), HashSet<u32>>,
    base_date: &NaiveDate,
) -> Vec<u32> {
    let total = train.total_seats;
    let weights = &train.weights;
    let sum_w: u32 = weights.iter().sum();

    // 最大余数法分配席位（与 load_trains_static 相同）
    let mut allocated = vec![0u32; weights.len()];
    if sum_w == 0 {
        let base = total / (weights.len() as u32);
        let rem = total % (weights.len() as u32);
        for i in 0..weights.len() {
            allocated[i] = base;
            if i < rem as usize {
                allocated[i] += 1;
            }
        }
    } else {
        let mut floors = Vec::with_capacity(weights.len());
        let mut fractions = Vec::with_capacity(weights.len());
        for &w in weights {
            let share = (total as f64 * w as f64) / sum_w as f64;
            let floor = share.floor() as u32;
            floors.push(floor);
            fractions.push(share - floor as f64);
        }
        let allocated_sum: u32 = floors.iter().sum();
        for i in 0..weights.len() {
            allocated[i] = floors[i];
        }
        let mut remaining = total - allocated_sum;
        if remaining > 0 {
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
    let first_dep_str = train.departure_times.first().and_then(|o| o.as_deref()).unwrap_or("00:00");
    let factor = if days_diff < 0 {
        0.0
    } else if days_diff == 0 {
        let mut factor = 0.15;
        if let Ok(first_time) = NaiveTime::parse_from_str(first_dep_str, "%H:%M") {
            let dep_datetime = base_date.and_time(first_time);
            let mins_left = (dep_datetime - now).num_minutes();
            if mins_left <= 0 {
                factor = 0.0;
            } else if mins_left < 120 {
                factor = 0.15 * (mins_left as f64 / 120.0);
            }
        }
        factor
    } else if days_diff >= 1 && days_diff <= 7 {
        match days_diff {
            1 => 0.2,
            2 => 0.6,
            3 => 0.9,
            _ => 0.9 + 0.1 * ((days_diff - 3) as f64 / 4.0),
        }
    } else {
        1.0
    };

    let mut seats = Vec::new();
    for i in 0..weights.len() {
        let key = (train.id.clone(), i);
        let sold_count = sold_counts.get(&key).map(|set| set.len()).unwrap_or(0) as u32;
        let max_avail = (allocated[i] as f64 * factor) as u32;
        let avail = if max_avail >= sold_count { max_avail - sold_count } else { 0 };
        seats.push(avail);
    }
    seats
}

fn update_train_status_with_date(train: &mut Train, base_date: &NaiveDate) {
    let now = Local::now().naive_local();
    let today = now.date();
    let days_diff = (*base_date - today).num_days();

    if days_diff < 0 {
        train.is_departed = true;
        train.is_selling = false;
        return;
    }
    if days_diff > 7 {
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    // 检查运行日是否匹配
    let base_weekday = base_date.weekday().number_from_monday() as u8;
    if base_weekday != train.run_day {
        // 如果不匹配，则该实例今天不可售
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    if days_diff == 0 {
        // 使用 departure_times 的第一个非None
        if let Some(Some(first_time_str)) = train.departure_times.first() {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                if now.time() >= first_time {
                    train.is_departed = true;
                    train.is_selling = false;
                    return;
                }
                let diff_secs = (first_time - now.time()).num_seconds();
                if diff_secs < 1800 {
                    train.is_selling = false;
                    train.is_departed = false;
                    return;
                }
                train.is_selling = true;
                train.is_departed = false;
                return;
            }
        }
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    // 未来1~7天且运行日匹配
    train.is_selling = true;
    train.is_departed = false;
}

fn load_trains_static(path: &str) -> Vec<Train> {
    let file = File::open(path).expect("无法打开 trains.csv 文件");
    let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(file);
    let mut trains = Vec::new();

    for result in rdr.records() {
        let record = result.expect("CSV 记录解析失败");
        let base_id = record[0].to_string();
        let model = record[1].to_string();
        let formation = record[2].to_string();
        let stations: Vec<String> = record[3].split('-').map(|s| s.to_string()).collect();
        let times: Vec<String> = record[4].split('-').map(|s| s.to_string()).collect();
        let prices: Vec<u32> = record[5].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let weights: Vec<u32> = record[6].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let _total_seats: u32 = record[7].parse().unwrap_or(0);
        let run_days_str = record[8].to_string();
        let platforms: Vec<String> = record[9].split('-').map(|s| s.to_string()).collect();
        let stop_mins: Vec<u32> = record[10].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let name = record[11].to_string();

        // 解析车厢数（从 formation 提取数字）
        let carriage_count = formation
            .chars()
            .take_while(|c| c.is_digit(10))
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(1);
        let seats_per_carriage = if model.contains("直特") { 35 } else { 40 };
        let total_seats_actual = carriage_count * seats_per_carriage;

        // 生成所有座位号列表 (1..=total_seats_actual)
        let all_seats: Vec<u32> = (1..=total_seats_actual).collect();

        // 按权重分配座位给各区间
        let sum_w: u32 = weights.iter().sum();
        let mut allocated = vec![Vec::new(); weights.len()];
        if sum_w == 0 {
            // 平均分配
            let base = total_seats_actual / (weights.len() as u32);
            let rem = total_seats_actual % (weights.len() as u32);
            let mut start = 0;
            for i in 0..weights.len() {
                let count = if i < rem as usize { base + 1 } else { base };
                allocated[i] = all_seats[start..start + count as usize].to_vec();
                start += count as usize;
            }
        } else {
            // 比例分配（最大余数法）
            let mut floors = Vec::new();
            let mut fractions = Vec::new();
            for w in &weights {
    let         share = (total_seats_actual as f64 * (*w as f64)) / sum_w as f64;
                let floor = share.floor() as u32;
                floors.push(floor);
                fractions.push(share - floor as f64);
            }
            let mut allocated_counts: Vec<u32> = floors.iter().cloned().collect();
            let mut remaining = total_seats_actual - allocated_counts.iter().sum::<u32>();
            if remaining > 0 {
                let mut indices: Vec<usize> = (0..weights.len()).collect();
                indices.sort_by(|&a, &b| fractions[b].partial_cmp(&fractions[a]).unwrap());
                for &i in &indices {
                    if remaining == 0 { break; }
                    allocated_counts[i] += 1;
                    remaining -= 1;
                }
            }
            let mut start = 0;
            for i in 0..weights.len() {
                let count = allocated_counts[i] as usize;
                allocated[i] = all_seats[start..start + count].to_vec();
                start += count;
            }
        }

        let run_days: Vec<u8> = run_days_str
            .split('-')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        // 为每个运行日生成一个实例
        for &day in &run_days {
            let id = format!("{}-{}", base_id, day);

            // 计算到达和出发时间
            let mut arrival_times = Vec::new();
            let mut departure_times = Vec::new();
            for i in 0..stations.len() {
                if i == 0 {
                    // 首站：只有出发时间
                    arrival_times.push(None);
                    departure_times.push(Some(times[i].clone()));
                } else if i == stations.len() - 1 {
                    // 终点站：只有到达时间
                    arrival_times.push(Some(times[i].clone()));
                    departure_times.push(None);
                } else {
                    // 中间站：到达时间为 times[i]，出发时间 = times[i] + stop_mins[i]
                    arrival_times.push(Some(times[i].clone()));
                    let dep_time = if let Ok(t) = NaiveTime::parse_from_str(&times[i], "%H:%M") {
                        let stop = stop_mins.get(i).cloned().unwrap_or(0);
                        let dep = t + chrono::Duration::minutes(stop as i64);
                        Some(dep.format("%H:%M").to_string())
                    } else {
                        Some(times[i].clone()) // fallback
                    };
                    departure_times.push(dep_time);
                }
            }

            // 校验数据一致性
            if stations.len() - 1 != prices.len() {
                panic!(
                    "车次 {} 站点数 {} 票价数 {} 不匹配",
                    base_id,
                    stations.len(),
                    prices.len()
                );
            }
            if stations.len() - 1 != weights.len() {
                panic!(
                    "车次 {} 站点数 {} 权重数 {} 不匹配",
                    base_id,
                    stations.len(),
                    weights.len()
                );
            }
            if stations.len() != platforms.len() {
                panic!(
                    "车次 {} 站点数 {} 站台数 {} 不匹配",
                    base_id,
                    stations.len(),
                    platforms.len()
                );
            }
            if stations.len() != stop_mins.len() {
                panic!(
                    "车次 {} 站点数 {} 停靠时间数 {} 不匹配",
                    base_id,
                    stations.len(),
                    stop_mins.len()
                );
            }

            trains.push(Train {
                id,
                base_train_id: base_id.clone(),
                name: name.clone(),
                model: model.clone(),
                formation: formation.clone(),
                stations: stations.clone(),
                platforms: platforms.clone(),
                arrival_times,
                departure_times,
                prices: prices.clone(),
                weights: weights.clone(),
                total_seats: total_seats_actual,
                carriage_count,
                seats_per_carriage,
                available_seats: allocated.clone(), // 初始分配
                seats: vec![],
                is_departed: false,
                is_selling: false,
                run_day: day,
            });
        }
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

fn refresh_trains_with_date(state: &Arc<AppState>, base_date: &NaiveDate) {
    let sold_counts = state.sold_counts.read().unwrap().clone();
    let mut trains = state.trains.write().unwrap();
    for train in trains.iter_mut() {
        train.seats = compute_interval_seats_with_date(train, &sold_counts, base_date);
        update_train_status_with_date(train, base_date);
    }
}

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
    info!("热重载完成");
}

// ---------- 车票生成辅助函数 ----------
fn load_font() -> FontArc {
    let font_paths = [
        // Windows
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/yahei.ttf",
        // Linux
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        // 项目内置
        "static/fonts/msyh.ttf",
    ];
    for path in font_paths.iter() {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(data) {
                // info!("成功加载字体: {}", path);
                return font;
            }
        }
    }
    panic!("未找到任何可用字体！当前搜索路径: {:?}", font_paths);
}

fn generate_seat_numbers(train: &Train, count: u32) -> Vec<String> {
    let seats_per_carriage = if train.model.contains("直特") {
        35
    } else {
        40
    };
    let carriages = train
        .formation
        .chars()
        .take_while(|c| c.is_digit(10))
        .collect::<String>()
        .parse::<u32>()
        .unwrap_or(1);
    let total_seats = carriages * seats_per_carriage;
    let count = count.min(total_seats);
    let mut rng = rand::thread_rng(); // 使用完全限定名
    let start = if total_seats > count {
        rng.gen_range(0..=total_seats - count)
    } else {
        0
    };
    let mut seats = Vec::with_capacity(count as usize);
    for i in 0..count {
        let seat_idx = start + i;
        let carriage = seat_idx / seats_per_carriage + 1;
        let seat = seat_idx % seats_per_carriage + 1;
        seats.push(format!("{}车{}号", carriage, seat));
    }
    seats
}

fn generate_ticket_image(order: &Order, seat_number: &str, train: &Train) -> Vec<u8> {
    let width = 400;
    let height = 220;
    let mut img = RgbImage::new(width, height);

    // 1. 蓝白渐变背景
    let top_color = Rgb([50, 90, 170]);
    let bottom_color = Rgb([220, 235, 255]);
    for y in 0..height {
        let t = y as f32 / (height - 1) as f32;
        let r = (top_color[0] as f32 * (1.0 - t) + bottom_color[0] as f32 * t) as u8;
        let g = (top_color[1] as f32 * (1.0 - t) + bottom_color[1] as f32 * t) as u8;
        let b = (top_color[2] as f32 * (1.0 - t) + bottom_color[2] as f32 * t) as u8;
        for x in 0..width {
            img.put_pixel(x, y, Rgb([r, g, b]));
        }
    }

    let font = load_font();
    let red = Rgb([255, 0, 0]);
    let dark_blue = Rgb([0, 0, 128]);
    let black = Rgb([0, 0, 0]);

    // 2. 左上角：红色订单号
    let scale_small = PxScale { x: 12.0, y: 12.0 };
    draw_text_mut(&mut img, red, 10, 10, scale_small, &font, &format!("订单号: {}", order.id));

    // 3. 右上角：售票站
    draw_text_mut(&mut img, dark_blue, 280, 10, scale_small, &font, "中央车站售票处");

    // 4. 顶部横排：日期、发车时间、座位号
    let date = &order.travel_date;
    let departure_time = train.departure_times[0].as_deref().unwrap_or("");
    let seat_info = seat_number;
    let info_line = format!("{}  {}开                             {}", date, departure_time, seat_info);
    let scale_mid = PxScale { x: 20.0, y: 20.0 };
    draw_text_mut(&mut img, dark_blue, 10, 35, scale_mid, &font, &info_line);

    // 5. 中部大字：出发站 → 到达站
    let scale_big = PxScale { x: 36.0, y: 36.0 };
    let scale_big_line = PxScale { x: 72.0, y: 36.0 };
    draw_text_mut(&mut img, black, 20, 65, scale_big, &font, &format!("{}", order.from_station));
    draw_text_mut(&mut img, black, 140, 65, scale_big_line, &font, "→");
    draw_text_mut(&mut img, black, 220, 65, scale_big, &font, &format!("{}", order.to_station));

    // 6. 车次
    let scale_mid2 = PxScale { x: 18.0, y: 18.0 };
    draw_text_mut(&mut img, dark_blue, 10, 120, scale_mid2, &font, &format!("车次: {}", train.base_train_id));

    // 7. 票价与规定
    let price = order.price / order.seat_count;
    draw_text_mut(&mut img, dark_blue, 10, 140, scale_mid2, &font, &format!("票价: {} 摩拉", price));
    draw_text_mut(&mut img, dark_blue, 10, 160, scale_mid2, &font, "限乘当日当次车  中途下车失效");
    draw_text_mut(&mut img, dark_blue, 10, 180, scale_mid2, &font, "唯我皇安娜丝塔夏将带来无垢的黎明!");

    // 8. 二维码（右下角）
    let qr = QrCode::new(order.id.as_bytes()).unwrap();
    let qr_image = qr.render::<image::Luma<u8>>().build();
    let qr_resized = image::imageops::resize(&qr_image, 60, 60, image::imageops::FilterType::Nearest);
    let qr_rgb = image::DynamicImage::ImageLuma8(qr_resized).into_rgb8();
    image::imageops::overlay(&mut img, &qr_rgb, (width - 70) as i64, (height - 70) as i64);

    // 9. 编码为PNG
    let mut buffer = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png).unwrap();
    buffer
}

// ---------- 路由处理 ----------
async fn index(State(state): State<Arc<AppState>>) -> Response {
    let today = Local::now().date_naive();
    refresh_trains_with_date(&state, &today);
    let mut ctx = Context::new();
    ctx.insert("page_title", "至冬国铁路 - 首页");
    ctx.insert("mode", "direct");
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
            let dep = t.departure_times.first().and_then(|o| o.as_ref());
            let arr = t.arrival_times.last().and_then(|o| o.as_ref());
            let min_seats = t.seats.iter().min().cloned().unwrap_or(0);
            serde_json::json!({
                "id": t.id,
                "base_train_id": t.base_train_id,
                "model": t.model,
                "from": first,
                "to": last,
                "departure": dep,
                "arrival": arr,
                "seats": min_seats,
                "is_selling": t.is_selling,
                "is_departed": t.is_departed,
                "run_day": t.run_day,
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

    let mode = params.mode.clone().unwrap_or_else(|| "direct".to_string());
    let trains = state.trains.read().unwrap();
    let from = params.from.trim();
    let to = params.to.trim();

    if mode == "transfer" {
        // 换乘搜索
        let trains = state.trains.read().unwrap();
        let plans = find_transfers(&trains, from, to, &base_date, 8);
        let mut ctx = Context::new();
        ctx.insert("page_title", "中转换乘方案 - 至冬国铁路");
        ctx.insert("plans", &plans);
        ctx.insert("from", from);
        ctx.insert("to", to);
        ctx.insert("selected_date", &date_str);
        // 站点列表（复用）
        let mut all_stations = HashSet::new();
        for train in trains.iter() {
            for station in &train.stations {
                all_stations.insert(station.clone());
            }
        }
        let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
        stations_vec.sort();
        ctx.insert("stations", &stations_vec);
        ctx.insert("mode", "transfer");
        let html = state.tera.render("search_results.html", &ctx).unwrap();
        return Html(html).into_response();
    }

    let mut results = Vec::new();
    for train in trains.iter() {
        if !train.is_selling && !train.is_departed {
            continue;
        }
        if let Some((fi, ti)) = find_interval(train, from, to) {
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let departure = &train.departure_times[fi].as_ref();
            let arrival = &train.arrival_times[ti].as_ref();
            results.push(serde_json::json!({
                "id": train.id,
                "base_train_id": train.base_train_id,
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
    ctx.insert("mode", "direct");

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
            let departure = &train.departure_times[fi].as_ref();
            let arrival = &train.arrival_times[ti].as_ref();
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let mut ctx = Context::new();
            let from_platform = &train.platforms[fi];
            ctx.insert("from_platform", from_platform);
            ctx.insert("page_title", "填写乘客信息 - 至冬国铁路");
            ctx.insert("train", &train);
            ctx.insert("from", &from);
            ctx.insert("to", &to);
            ctx.insert("departure", departure);
            ctx.insert("arrival", arrival);
            ctx.insert("total_price", &total_price);
            ctx.insert("min_seats", &min_seats);
            ctx.insert("selected_date", &date_str);
            ctx.insert("carriage_count", &train.carriage_count);
            let html = state.tera.render("order_form.html", &ctx).unwrap();
            return Html(html).into_response();
        } else {
            return (StatusCode::BAD_REQUEST, "无效的出发或到达站").into_response();
        }
    }
    (StatusCode::NOT_FOUND, "车次不存在").into_response()
}

async fn order_transfer(
    State(state): State<Arc<AppState>>,
    Form(form): Form<TransferOrderForm>,
) -> Response {
    let base_date = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date);

    let (_order1_id, _order2_id, group_id) = {
        let mut trains = state.trains.write().unwrap();
        let mut sold_counts = state.sold_counts.write().unwrap();
        let mut orders = state.orders.write().unwrap();

        // 获取两个车次的索引
        let train1_idx = match trains.iter().position(|t| t.id == form.first_train_id) {
            Some(idx) => idx,
            None => return (StatusCode::BAD_REQUEST, "第一段车次不存在").into_response(),
        };
        let train2_idx = match trains.iter().position(|t| t.id == form.second_train_id) {
            Some(idx) => idx,
            None => return (StatusCode::BAD_REQUEST, "第二段车次不存在").into_response(),
        };

        // --- 第一段 ---
        let (fi1, _ti1, price1, assigned1) = {
            let train1 = &mut trains[train1_idx];
            if !train1.is_selling {
                return (StatusCode::BAD_REQUEST, "第一段车次已停止售票").into_response();
            }
            let (fi, ti) = match find_interval(train1, &form.first_from, &form.first_to) {
                Some((f, t)) => (f, t),
                None => return (StatusCode::BAD_REQUEST, "第一段无效区间").into_response(),
            };
            // 分配座位
            let assigned = match allocate_seats_for_interval(
                train1,
                fi,
                ti,
                form.carriage_index,
                &form.seat_category,
                form.seat_count,
            ) {
                Some(seats) => seats,
                None => return (StatusCode::BAD_REQUEST, "第一段该类坐席余票不足").into_response(),
            };
            // 更新 sold_counts
            for i in fi..ti {
                let key = (form.first_train_id.clone(), i);
                let set = sold_counts.entry(key).or_insert_with(HashSet::new);
                for &s in &assigned {
                    set.insert(s);
                }
            }
            let price: u32 = train1.prices[fi..ti].iter().sum();
            (fi, ti, price, assigned)
        };

        // --- 第二段 ---
        let (fi2, _ti2, price2, assigned2) = {
            let train2 = &mut trains[train2_idx];
            if !train2.is_selling {
                return (StatusCode::BAD_REQUEST, "第二段车次已停止售票").into_response();
            }
            let (fi, ti) = match find_interval(train2, &form.second_from, &form.second_to) {
                Some((f, t)) => (f, t),
                None => return (StatusCode::BAD_REQUEST, "第二段无效区间").into_response(),
            };
            let assigned = match allocate_seats_for_interval(
                train2,
                fi,
                ti,
                form.carriage_index,
                &form.seat_category,
                form.seat_count,
            ) {
                Some(seats) => seats,
                None => {
                    // 若第二段失败，需回滚第一段已分配的座位
                    // 回滚：释放已分配的座位
                    let mut trains_ref = trains;
                    release_seats(
                        &form.first_train_id,
                        &form.first_from,
                        &form.first_to,
                        &assigned1,
                        &mut trains_ref,
                        &mut sold_counts,
                    );
                    return (StatusCode::BAD_REQUEST, "第二段该类坐席余票不足").into_response();
                }
            };
            for i in fi..ti {
                let key = (form.second_train_id.clone(), i);
                let set = sold_counts.entry(key).or_insert_with(HashSet::new);
                for &s in &assigned {
                    set.insert(s);
                }
            }
            let price: u32 = train2.prices[fi..ti].iter().sum();
            (fi, ti, price, assigned)
        };

        // 计算总票价
        let total_price1 = price1 * form.seat_count;
        let total_price2 = price2 * form.seat_count;

        // 创建订单
        let group_id = Uuid::new_v4().to_string();
        let expire_at = Utc::now() + Duration::minutes(30);

        let order1_id = generate_unique_order_id(&orders);
        let order2_id = generate_unique_order_id(&orders);

        let order1 = Order {
            id: order1_id.clone(),
            train_id: form.first_train_id,
            from_station: form.first_from.clone(),
            to_station: form.first_to.clone(),
            from_platform: {
                let train1 = &trains[train1_idx];
                train1.platforms[fi1].clone()
            },
            passenger_name: form.passenger_name.clone(),
            seat_count: form.seat_count,
            price: total_price1,
            status: OrderStatus::Pending,
            expire_at: expire_at.timestamp(),
            created_at: Utc::now().timestamp(),
            pay_time: None,
            travel_date: form.date.clone(),
            group_id: Some(group_id.clone()),
            seat_numbers: assigned1,
            seat_category: form.seat_category.clone(),
            carriage_index: form.carriage_index,
        };
        let order2 = Order {
            id: order2_id.clone(),
            train_id: form.second_train_id,
            from_station: form.second_from.clone(),
            to_station: form.second_to.clone(),
            from_platform: {
                let train2 = &trains[train2_idx];
                train2.platforms[fi2].clone()
            },
            passenger_name: form.passenger_name.clone(),
            seat_count: form.seat_count,
            price: total_price2,
            status: OrderStatus::Pending,
            expire_at: expire_at.timestamp(),
            created_at: Utc::now().timestamp(),
            pay_time: None,
            travel_date: form.date.clone(),
            group_id: Some(group_id.clone()),
            seat_numbers: assigned2,
            seat_category: form.seat_category.clone(),
            carriage_index: form.carriage_index,
        };

        orders.insert(order1_id.clone(), order1);
        orders.insert(order2_id.clone(), order2);

        // 保存订单到文件
        let orders_clone = orders.clone();
        drop(orders);
        tokio::task::spawn_blocking(move || {
            save_orders(&orders_clone);
        });

        (order1_id, order2_id, group_id)
    };

    // 重定向到换乘确认页
    Redirect::to(&format!("/transfer_confirm?group_id={}", group_id)).into_response()
}

async fn order_transfer_form(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let first_train_id = params.get("first_train_id").cloned().unwrap_or_default();
    let first_from = params.get("first_from").cloned().unwrap_or_default();
    let first_to = params.get("first_to").cloned().unwrap_or_default();
    let second_train_id = params.get("second_train_id").cloned().unwrap_or_default();
    let second_from = params.get("second_from").cloned().unwrap_or_default();
    let second_to = params.get("second_to").cloned().unwrap_or_default();
    let date = params.get("date").cloned().unwrap_or_default();
    let total_price = params.get("total_price").cloned().unwrap_or_default();
    let min_seats = params.get("min_seats").cloned().unwrap_or_default();

    if first_train_id.is_empty() || second_train_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "参数不完整").into_response();
    }

    // 获取车厢数（取第一个车次）
    let trains = state.trains.read().unwrap();
    let carriage_count = trains
        .iter()
        .find(|t| t.id == first_train_id)
        .map(|t| t.carriage_count)
        .unwrap_or(1);

    let mut ctx = Context::new();
    ctx.insert("page_title", "填写换乘订单 - 至冬国铁路");
    ctx.insert("first_train_id", &first_train_id);
    ctx.insert("first_from", &first_from);
    ctx.insert("first_to", &first_to);
    ctx.insert("second_train_id", &second_train_id);
    ctx.insert("second_from", &second_from);
    ctx.insert("second_to", &second_to);
    ctx.insert("date", &date);
    ctx.insert("total_price", &total_price);
    ctx.insert("min_seats", &min_seats);
    ctx.insert("carriage_count", &carriage_count); // 新增
    let html = state.tera.render("order_transfer_form.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn create_order(
    State(state): State<Arc<AppState>>,
    Form(form): Form<CreateOrderForm>,
) -> Response {
    let base_date = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
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

    let (fi, ti) = match find_interval(train, &form.from, &form.to) {
        Some((f, t)) => (f, t),
        None => return (StatusCode::BAD_REQUEST, "无效的站点区间").into_response(),
    };

    // 分配座位
    let assigned = match allocate_seats_for_interval(
        train,
        fi,
        ti,
        form.carriage_index,
        &form.seat_category,
        form.seat_count,
    ) {
        Some(seats) => seats,
        None => return (StatusCode::BAD_REQUEST, "该类坐席余票不足").into_response(),
    };

    // 更新 sold_counts
    let mut sold_counts = state.sold_counts.write().unwrap();
    for i in fi..ti {
        let key = (form.train_id.clone(), i);
        let set = sold_counts.entry(key).or_insert_with(HashSet::new);
        for &s in &assigned {
            set.insert(s);
        }
    }
    drop(sold_counts);

    // 创建订单
    let order_id = generate_unique_order_id(&state.orders.read().unwrap());
    let expire_at = Utc::now() + Duration::minutes(30);
    let order = Order {
        id: order_id.clone(),
        train_id: form.train_id,
        from_station: form.from,
        to_station: form.to,
        from_platform: form.from_platform,
        passenger_name: form.passenger_name,
        seat_count: form.seat_count,
        price: form.total_price * form.seat_count,
        status: OrderStatus::Pending,
        expire_at: expire_at.timestamp(),
        created_at: Utc::now().timestamp(),
        pay_time: None,
        travel_date: form.date,
        group_id: None,
        seat_numbers: assigned,
        seat_category: form.seat_category,
        carriage_index: form.carriage_index,
    };

    let mut orders = state.orders.write().unwrap();
    orders.insert(order_id.clone(), order);
    let orders_clone = orders.clone();
    drop(orders);
    tokio::task::spawn_blocking(move || {
        save_orders(&orders_clone);
    });

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
        let trains = state.trains.read().unwrap();
        let base_train_id = trains
            .iter()
            .find(|t| t.id == order.train_id)
            .map(|t| t.base_train_id.clone())
            .unwrap_or_else(|| order.train_id.clone());
        let mut ctx = Context::new();
        ctx.insert("page_title", "支付 - 至冬国铁路");
        ctx.insert("order", order);
        ctx.insert("base_train_id", &base_train_id);
        let html = state.tera.render("payment.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

// 支付完成后跳转到车票打印页
async fn pay_order(State(state): State<Arc<AppState>>, Path(order_id): Path<String>) -> Response {
    // 1. 更新订单状态（短时持有写锁）
    let (passenger_name, order_data) = {
        let mut orders = state.orders.write().unwrap();
        if let Some(order) = orders.get_mut(&order_id) {
            if order.status != OrderStatus::Pending {
                return (StatusCode::BAD_REQUEST, "订单已处理").into_response();
            }
            order.status = OrderStatus::Paid;
            order.pay_time = Some(Utc::now().timestamp());
            // 提取数据，准备保存
            let name = order.passenger_name.clone();
            let data = order.clone();
            (name, data)
        } else {
            return (StatusCode::NOT_FOUND, "订单不存在").into_response();
        }
    }; // 写锁自动释放

    // 2. 更新订单索引（短时持有写锁）
    {
        let mut index = state.order_index.write().unwrap();
        index.entry(passenger_name.clone())
            .or_insert_with(Vec::new)
            .push(order_id.clone());
        save_order_index(&index);
    }

    // 3. 保存订单详细信息
    {
        let orders = state.orders.read().unwrap();
        save_orders(&orders);
    }

    // 4. 判断是否为换乘订单
    if let Some(group_id) = &order_data.group_id {
        // 重定向到换乘确认页
        Redirect::to(&format!("/transfer_confirm?group_id={}", group_id)).into_response()
    } else {
        // 普通订单跳转到车票打印
        Redirect::to(&format!("/tickets/{}", order_id)).into_response()
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
        let now = Local::now().naive_local();
        let trains = state.trains.read().unwrap();
        let train = trains.iter().find(|t| t.id == order.train_id);
        let can_operate = if let Some(t) = train {
            if let Some(first_time_str) = t.departure_times.first().and_then(|o| o.as_ref()) {
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

    // 1. 获取订单、更新状态、提取数据
    let (passenger_name, train_id, from, to, seat_numbers) = {
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
                if let Some(first_time_str) = t.departure_times.first().and_then(|o| o.as_ref()) {
                    if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                        let base_date = now.date();
                        let dep_datetime = base_date.and_time(first_time);
                        if now >= dep_datetime {
                            return (StatusCode::BAD_REQUEST, "已发车，不可退签").into_response();
                        }
                    }
                }
            }
            // 更新状态
            order.status = OrderStatus::Refunded;
            // 提取数据
            let data = (
                order.passenger_name.clone(),
                order.train_id.clone(),
                order.from_station.clone(),
                order.to_station.clone(),
                order.seat_numbers.clone(),
            );
            // 保存订单
            let orders_clone = orders.clone();
            drop(orders);
            tokio::task::spawn_blocking(move || {
                save_orders(&orders_clone);
            });
            data
        } else {
            return (StatusCode::NOT_FOUND, "订单不存在").into_response();
        }
    };

    // 2. 释放座位
    {
        let mut sold_counts = state.sold_counts.write().unwrap();
        let mut trains = state.trains.write().unwrap();
        release_seats(&train_id, &from, &to, &seat_numbers, &mut trains, &mut sold_counts);
    }

    // 3. 更新索引
    {
        let mut index = state.order_index.write().unwrap();
        if let Some(list) = index.get_mut(&passenger_name) {
            list.retain(|id| id != &order_id);
            if list.is_empty() {
                index.remove(&passenger_name);
            }
        }
        let index_clone = index.clone();
        drop(index);
        tokio::task::spawn_blocking(move || {
            save_order_index(&index_clone);
        });
    }

    // 4. 刷新余票
    let today = Local::now().date_naive();
    refresh_trains_with_date(&state, &today);

    // 5. 渲染结果
    let mut ctx = Context::new();
    ctx.insert("page_title", "退签成功 - 至冬国铁路");
    ctx.insert("message", "退签成功，已释放座位");
    ctx.insert("order_id", &order_id);
    let html = state.tera.render("exchange_result.html", &ctx).unwrap();
    Html(html).into_response()
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
    let new_carriage = params
        .get("new_carriage")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let new_category = params
        .get("new_category")
        .cloned()
        .unwrap_or_else(|| "corridor".to_string());
    let is_ajax = params.get("_ajax").map(|v| v == "1").unwrap_or(false);

    // 参数验证
    if order_id.is_empty() || new_train_id.is_empty() || new_from.is_empty() || new_to.is_empty() {
        return if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "参数不完整" }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, "参数不完整").into_response()
        };
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
                return if is_ajax {
                    (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "原订单状态不可改签" }))).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "原订单状态不可改签").into_response()
                };
            }
            o
        }
        None => {
            return if is_ajax {
                (StatusCode::NOT_FOUND, Json(json!({ "success": false, "message": "原订单不存在" }))).into_response()
            } else {
                (StatusCode::NOT_FOUND, "原订单不存在").into_response()
            };
        }
    };

    // 检查原订单是否已发车
    if let Some(train) = trains.iter().find(|t| t.id == order.train_id) {
        if let Some(first_time_str) = train.departure_times.first().and_then(|o| o.as_ref()) {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                let now = Local::now().naive_local();
                let travel_date =
                    NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d").unwrap_or(now.date());
                let dep_datetime = travel_date.and_time(first_time);
                if now >= dep_datetime {
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "已发车，不可改签" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "已发车，不可改签").into_response()
                    };
                }
            }
        }
    }

    // 2. 释放原票座位
    if let Some(train) = trains.iter_mut().find(|t| t.id == order.train_id) {
        if let Some((fi, ti)) = find_interval(train, &order.from_station, &order.to_station) {
            for i in fi..ti {
                let key = (order.train_id.clone(), i);
                if let Some(set) = sold_counts.get_mut(&key) {
                    for &s in &order.seat_numbers {
                        set.remove(&s);
                    }
                }
                for &s in &order.seat_numbers {
                    train.available_seats[i].push(s);
                }
                train.available_seats[i].sort();
                train.available_seats[i].dedup();
            }
        }
    }

    // 3. 检查新车次并分配座位
    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let new_train = trains.iter_mut().find(|t| t.id == new_train_id);
    let new_train = match new_train {
        Some(t) => t,
        None => {
            return if is_ajax {
                (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "新车次不存在" }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, "新车次不存在").into_response()
            };
        }
    };
    // 刷新新车次状态
    new_train.seats = compute_interval_seats_with_date(new_train, &sold_counts, &travel_date);
    update_train_status_with_date(new_train, &travel_date);
    if !new_train.is_selling {
        return if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "新车次已停止售票或已发车" }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, "新车次已停止售票或已发车").into_response()
        };
    }

    // 分配新车次座位
    let (new_price_total, from_platform, assigned_seats) = {
        if let Some((fi, ti)) = find_interval(new_train, &new_from, &new_to) {
            let assigned = match allocate_seats_for_interval(
                new_train,
                fi,
                ti,
                new_carriage,
                &new_category,
                new_seat_count,
            ) {
                Some(seats) => seats,
                None => {
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "该坐席类型余票不足，请选择其他车厢或类别" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "新车次该类坐席余票不足").into_response()
                    };
                }
            };
            for i in fi..ti {
                let key = (new_train_id.clone(), i);
                let set = sold_counts.entry(key).or_insert_with(HashSet::new);
                for &s in &assigned {
                    set.insert(s);
                }
            }
            let interval_price: u32 = new_train.prices[fi..ti].iter().sum();
            let new_total = interval_price * new_seat_count;
            let final_price = if new_total > order.price { new_total } else { order.price };
            let from_platform = new_train.platforms[fi].clone();
            (final_price, from_platform, assigned)
        } else {
            return if is_ajax {
                (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "新车次无效区间" }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, "新车次无效区间").into_response()
            };
        }
    };

    // 4. 创建新订单
    let new_order_id = generate_unique_order_id(&orders);
    let new_order = Order {
        id: new_order_id.clone(),
        train_id: new_train_id.clone(),
        from_station: new_from.clone(),
        to_station: new_to.clone(),
        from_platform,
        passenger_name: order.passenger_name.clone(),
        seat_count: new_seat_count,
        price: new_price_total,
        status: OrderStatus::Paid,
        expire_at: Utc::now().timestamp() + 1800,
        created_at: Utc::now().timestamp(),
        pay_time: Some(Utc::now().timestamp()),
        travel_date: order.travel_date.clone(),
        group_id: None,
        seat_numbers: assigned_seats,
        seat_category: new_category,
        carriage_index: new_carriage,
    };

    // 原订单标记为取消
    let old_order = orders.get_mut(&order_id).unwrap();
    old_order.status = OrderStatus::Cancelled;

    // 插入新订单
    orders.insert(new_order_id.clone(), new_order);

    // 更新索引
    let mut index = state.order_index.write().unwrap();
    let name = order.passenger_name.clone();
    if let Some(list) = index.get_mut(&name) {
        list.retain(|id| id != &order_id);
    }
    index.entry(name).or_insert_with(Vec::new).push(new_order_id.clone());
    let index_clone = index.clone();
    drop(index);
    tokio::task::spawn_blocking(move || {
        save_order_index(&index_clone);
    });

    // 保存订单
    let orders_clone = orders.clone();
    drop(orders);
    tokio::task::spawn_blocking(move || {
        save_orders(&orders_clone);
    });

    // 手动更新所有列车数据
    for t in trains.iter_mut() {
        t.seats = compute_interval_seats_with_date(t, &sold_counts, &travel_date);
        update_train_status_with_date(t, &travel_date);
    }

    // 返回 AJAX 或页面
    if is_ajax {
        (StatusCode::OK, Json(json!({ "success": true, "order_id": new_order_id }))).into_response()
    } else {
        let mut ctx = Context::new();
        ctx.insert("page_title", "改签成功 - 至冬国铁路");
        ctx.insert("message", "改签成功，请查看新订单");
        ctx.insert("order_id", &order_id);
        let html = state.tera.render("exchange_result.html", &ctx).unwrap();
        Html(html).into_response()
    }
}

async fn exchange_available(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }

    let orders = state.orders.read().unwrap();
    let order = orders.get(&order_id).cloned();
    let order = match order {
        Some(o) => o,
        None => return (StatusCode::NOT_FOUND, "订单不存在").into_response(),
    };

    // 解析乘车日期
    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let weekday = travel_date.weekday().number_from_monday() as u8; // 1=周一

    let from = &order.from_station;
    let to = &order.to_station;

    let trains = state.trains.read().unwrap();
    let now = Local::now().naive_local();
    let mut available = Vec::new();

    for train in trains.iter() {
        // 1. 运行日匹配
        if train.run_day != weekday {
            continue;
        }
        // 2. 出发/到达站匹配
        if let Some((fi, ti)) = find_interval(train, from, to) {
            // 3. 检查是否已发车（以首站出发时间为准）
            if let Some(Some(first_dep_str)) = train.departure_times.first() {
                if let Ok(first_dep) = NaiveTime::parse_from_str(first_dep_str, "%H:%M") {
                    let dep_datetime = travel_date.and_time(first_dep);
                    if now >= dep_datetime {
                        continue; // 已发车
                    }
                }
            }
            // 4. 检查余票（至少一张）
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            if min_seats == 0 {
                continue;
            }
            // 5. 计算区间总价
            let price: u32 = train.prices[fi..ti].iter().sum();
            let departure = train.departure_times[fi].as_deref().unwrap_or("");
            let arrival = train.arrival_times[ti].as_deref().unwrap_or("");

            available.push(serde_json::json!({
                "id": train.id,
                "base_train_id": train.base_train_id,
                "name": train.name,
                "departure": departure,
                "arrival": arrival,
                "price": price,
                "seats": min_seats,
                "carriage_count": train.carriage_count,
            }));
        }
    }

    // 按出发时间排序
    available.sort_by(|a, b| {
        a["departure"]
            .as_str()
            .unwrap()
            .cmp(b["departure"].as_str().unwrap())
    });

    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&available).unwrap(),
    )
        .into_response()
}

// ---------- 车票打印功能 ----------
async fn tickets_page(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> Response {
    let orders = state.orders.read().unwrap();
    let order = orders.get(&order_id).cloned();
    if let Some(order) = order {
        if order.status != OrderStatus::Paid {
            return (StatusCode::BAD_REQUEST, "订单未支付，无法打印车票").into_response();
        }
        // 直接使用订单中存储的座位号
        let seat_numbers: Vec<String> = order
            .seat_numbers
            .iter()
            .map(|&s| {
                let train = state.trains.read().unwrap();
                let train = train.iter().find(|t| t.id == order.train_id).unwrap();
                let carriage = (s - 1) / train.seats_per_carriage + 1;
                let seat = (s - 1) % train.seats_per_carriage + 1;
                format!("{}车{}号", carriage, seat)
            })
            .collect();
        let mut ctx = Context::new();
        let base_train_id = state.trains
            .read()
            .unwrap()
            .iter()
            .find(|t| t.id == order.train_id)
            .map(|t| t.base_train_id.clone())
            .unwrap_or_else(|| order.train_id.clone());
        ctx.insert("base_train_id", &base_train_id);
        ctx.insert("page_title", "车票打印 - 至冬国铁路");
        ctx.insert("order_id", &order_id);
        ctx.insert("seat_numbers", &seat_numbers);
        let html = state.tera.render("tickets.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

async fn ticket_image(
    State(state): State<Arc<AppState>>,
    Path((order_id, seat_index)): Path<(String, usize)>,
) -> Response {
    let orders = state.orders.read().unwrap();
    let order = orders.get(&order_id).cloned();
    if let Some(order) = order {
        if order.status != OrderStatus::Paid {
            return (StatusCode::BAD_REQUEST, "订单未支付").into_response();
        }
        // 使用订单中存储的座位号
        if seat_index < order.seat_numbers.len() {
            let seat = order.seat_numbers[seat_index];
            let trains = state.trains.read().unwrap();
            let train = trains.iter().find(|t| t.id == order.train_id).unwrap();
            let seat_str = format!(
                "{}车{}号",
                (seat - 1) / train.seats_per_carriage + 1,
                (seat - 1) % train.seats_per_carriage + 1
            );
            let img_bytes = generate_ticket_image(&order, &seat_str, train);
            ([(header::CONTENT_TYPE, "image/png")], img_bytes).into_response()
        } else {
            (StatusCode::NOT_FOUND, "座位索引无效").into_response()
        }
    } else {
        (StatusCode::NOT_FOUND, "订单不存在").into_response()
    }
}

// ---------- 车次细节 ----------
async fn detail_page(
    State(state): State<Arc<AppState>>,
    Path(train_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let trains = state.trains.read().unwrap();
    let train = trains.iter().find(|t| t.id == train_id).cloned();
    if let Some(train) = train {
        let from = params.get("from").cloned().unwrap_or_default();
        let to = params.get("to").cloned().unwrap_or_default();

        // 构造站点详情列表
        let mut station_details = Vec::new();
        for i in 0..train.stations.len() {
            station_details.push(serde_json::json!({
                "station": train.stations[i],
                "platform": train.platforms.get(i).unwrap_or(&"".to_string()),
                "arrival": train.arrival_times[i].as_ref().unwrap_or(&"始发".to_string()),
                "departure": train.departure_times[i].as_ref().unwrap_or(&"终到".to_string()),
            }));
        }

        let mut ctx = Context::new();
        ctx.insert("page_title", &format!("{} 详情", train.name));
        ctx.insert("train", &train);
        ctx.insert("from_highlight", &from);
        ctx.insert("to_highlight", &to);
        ctx.insert("station_details", &station_details);
        let html = state.tera.render("detail.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "车次不存在").into_response()
    }
}

// ---------- 运行图 ----------
async fn diagram_page(State(state): State<Arc<AppState>>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "列车运行图 - 至冬国铁路");
    let html = state.tera.render("diagram.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 管理端点 ----------
async fn admin_reload(State(state): State<Arc<AppState>>) -> Response {
    reload_all(&state);
    (StatusCode::OK, "重载成功").into_response()
}

// 显示查询界面
async fn my_orders_page(State(state): State<Arc<AppState>>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "我的订单 - 至冬国铁路");
    ctx.insert("name", ""); // 添加空字符串默认值
    let html = state.tera.render("my_orders.html", &ctx).unwrap();
    Html(html).into_response()
}

// 查询处理
async fn my_orders_query(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let name = params.get("name").cloned().unwrap_or_default();
    let mut ctx = Context::new();
    ctx.insert("page_title", "我的订单 - 至冬国铁路");
    if !name.is_empty() {
        let index = state.order_index.read().unwrap();
        let order_ids = index.get(&name).cloned().unwrap_or_default();
        ctx.insert("orders", &order_ids);
        ctx.insert("name", &name);
    }
    let html = state.tera.render("my_orders.html", &ctx).unwrap();
    Html(html).into_response()
}

async fn transfer_confirm(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let group_id = params.get("group_id").cloned().unwrap_or_default();
    if group_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 group_id").into_response();
    }

    let orders = state.orders.read().unwrap();
    let group_orders: Vec<&Order> = orders
        .values()
        .filter(|o| o.group_id.as_ref() == Some(&group_id))
        .collect();
    if group_orders.is_empty() {
        return (StatusCode::NOT_FOUND, "未找到该换乘组订单").into_response();
    }

    let passenger_name = group_orders[0].passenger_name.clone();
    let mut ctx = Context::new();
    ctx.insert("page_title", "换乘订单支付 - 至冬国铁路");
    ctx.insert("group_id", &group_id);
    ctx.insert("orders", &group_orders);
    ctx.insert("passenger_name", &passenger_name);
    let html = state.tera.render("transfer_confirm.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 主函数 ----------
#[tokio::main]
async fn main() {
    let _guard = init_logging();

    let tera = match Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            error!("Tera 解析模板目录失败: {}", e);
            return;
        }
    };

    let sold_counts: HashMap<(String, usize), HashSet<u32>> = HashMap::new();
    let static_trains = load_trains_static("trains.csv");
    let now_date = Local::now().date_naive();
    let mut trains = Vec::new();
    for mut train in static_trains {
        train.seats = compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        update_train_status_with_date(&mut train, &now_date);
        trains.push(train);
    }

    let notifications = load_notifications("notifications.txt");
    // 加载订单索引（如果存在）
    let order_index = if let Ok(data) = fs::read_to_string("order_index.json") {
        serde_json::from_str(&data).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    };
    // 加载订单详细信息
    let orders = load_orders();

    let state = Arc::new(AppState {
        tera,
        trains: RwLock::new(trains),
        orders: RwLock::new(orders),          // 使用加载的订单
        sold_counts: RwLock::new(sold_counts),
        notifications: RwLock::new(notifications),
        train_mtime: RwLock::new(get_mtime("trains.csv")),
        notif_mtime: RwLock::new(get_mtime("notifications.txt")),
        order_index: RwLock::new(order_index), // 使用加载的索引
    });

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
                info!("检测到文件变更，执行热重载...");
                reload_all(&state_clone);
            }
        }
    });

    // 构建限流配置
    let _governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(10) // 每秒允许10个请求
            .burst_size(30) // 突发峰值30个
            .finish()
            .unwrap(),
    );

    let app = Router::new()
        .route("/", get(index))
        .route("/search", get(search))
        .route("/order/create", post(create_order))
        .route("/order/{train_id}", get(order_form))
        .route("/order/transfer", post(order_transfer))
        .route("/order/transfer_form", get(order_transfer_form))
        .route("/orders", get(orders_query))
        .route("/detail/{train_id}", get(detail_page))
        .route("/payment/{order_id}/pay", get(pay_order))
        .route("/payment/{order_id}", get(payment_page))
        .route("/notifications", get(notifications_page))
        .route("/exchange", get(exchange_page))
        .route("/exchange/query", get(exchange_query))
        .route("/exchange/refund", post(exchange_refund))
        .route("/exchange/change", post(exchange_change))
        .route("/transfer_confirm", get(transfer_confirm))
        .route("/my_orders", get(my_orders_page))
        .route("/my_orders/query", get(my_orders_query))
        .route("/tickets/{order_id}", get(tickets_page))
        .route("/ticket/{order_id}/{seat_index}", get(ticket_image))
        .route("/exchange/available", get(exchange_available))
        .route("/diagram", get(diagram_page))
        .route("/admin/reload", get(admin_reload))
        .nest_service("/static", ServeDir::new("static"))
        .layer(TraceLayer::new_for_http())
        // .layer(GovernorLayer::new(governor_conf))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
