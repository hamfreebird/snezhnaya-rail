use ab_glyph::{FontArc, PxScale};
use axum::{
    Form, Router,
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
use serde_json;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration as StdDuration, SystemTime};
use tera::{Context, Tera};
use tokio::time;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};
use tower_http::services::ServeDir;
// use uuid::Uuid;

// ---------- 数据结构 ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Train {
    // 唯一ID：格式为 "车次-运行日"，如 "1001-3"
    id: String,
    // 原始车次号（显示用）
    base_train_id: String,
    // 列车名称
    name: String,
    model: String,
    formation: String,
    // 站点列表（顺序）
    stations: Vec<String>,
    // 站台列表（与站点一一对应）
    platforms: Vec<String>,
    // 到达时间列表（首站为None）
    arrival_times: Vec<Option<String>>,
    // 出发时间列表（终点站为None）
    departure_times: Vec<Option<String>>,
    // 区间票价（长度 = stations.len()-1）
    prices: Vec<u32>,
    // 权重
    weights: Vec<u32>,
    // 总座位数
    total_seats: u32,
    // 实时余票
    seats: Vec<u32>,
    // 状态
    is_departed: bool,
    is_selling: bool,
    // 运行日（该实例对应的星期几，1=周一）
    run_day: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    train_id: String, // 指向 Train.id（唯一ID）
    from_station: String,
    to_station: String,
    from_platform: String, // 新增：出发站台
    passenger_name: String,
    seat_count: u32,
    price: u32,
    status: OrderStatus,
    expire_at: i64,
    created_at: i64,
    pay_time: Option<i64>,
    travel_date: String,
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
    order_index: RwLock<HashMap<String, Vec<String>>>,
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
    from_platform: String,
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

fn load_order_index() -> HashMap<String, Vec<String>> {
    let path = "order_index.json";
    fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
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
    sold_counts: &HashMap<(String, usize), u32>,
    base_date: &NaiveDate,
) -> Vec<u32> {
    let total = train.total_seats;
    let weights = &train.weights;
    let sum_w: u32 = weights.iter().sum();

    // 最大余数法分配席位
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
                if remaining == 0 {
                    break;
                }
                allocated[i] += 1;
                remaining -= 1;
            }
        }
    }

    let now = Local::now().naive_local();
    let days_diff = (*base_date - now.date()).num_days();

    // 获取首站出发时间（从 departure_times 第一个非None）
    let first_dep_str = train
        .departure_times
        .first()
        .and_then(|o| o.as_deref())
        .unwrap_or("00:00");
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
    for (i, &alloc) in allocated.iter().enumerate() {
        let key = (train.id.clone(), i);
        let sold = sold_counts.get(&key).cloned().unwrap_or(0);
        let max_avail = (alloc as f64 * factor) as u32;
        let avail = if max_avail >= sold {
            max_avail - sold
        } else {
            0
        };
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

    // 检查运行日是否匹配（由于我们按日拆分实例，train.run_day 应等于 base_date 的星期）
    // 但 base_date 可能不是今天，我们用 base_date 的星期与 train.run_day 比较
    let base_weekday = base_date.weekday().number_from_monday() as u8; // 1=周一
    if base_weekday != train.run_day {
        // 如果不匹配，则该实例今天不可售（但显示时可能隐藏）
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
        let prices: Vec<u32> = record[5]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let weights: Vec<u32> = record[6]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let total_seats: u32 = record[7].parse().unwrap_or(0);

        // 新增列
        let run_days_str = record[8].to_string();
        let platforms: Vec<String> = record[9].split('-').map(|s| s.to_string()).collect();
        let stop_mins: Vec<u32> = record[10]
            .split('-')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let name = record[11].to_string();

        // 解析运行日
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
                total_seats,
                seats: vec![], // 稍后计算
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
    println!("热重载完成");
}

// ---------- 车票生成辅助函数 ----------
fn load_font() -> FontArc {
    let font_paths = [
        // Windows 常用中文字体
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/yahei.ttf",
        // Linux (需安装 Noto 或文泉驿)
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        // 项目内置（请将字体文件放入此目录）
        "static/fonts/SimHei.ttf",
        "static/fonts/NotoSansCJK-Regular.ttc",
        "static/fonts/DejaVuSans.ttf",
        "static/fonts/msyh.ttf",
    ];
    for path in font_paths.iter() {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(data) {
                println!("✅ 成功加载字体: {}", path);
                return font;
            }
        }
    }
    // 如果所有字体加载失败，给出明确错误并退出
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

fn generate_ticket_image(order: &Order, seat_number: &str, _train: &Train) -> Vec<u8> {
    let width = 400;
    let height = 200;
    let mut img = RgbImage::new(width, height);

    // 1. 创建蓝白渐变背景（从上到下）
    let top_color = Rgb([50, 90, 170]); // 深蓝
    let bottom_color = Rgb([220, 235, 255]); // 淡蓝白
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
    let dark_blue = Rgb([0, 0, 128]); // 深蓝色文字
    let scale = PxScale { x: 16.0, y: 16.0 };

    // 2. 绘制文字（深蓝色）
    draw_text_mut(&mut img, dark_blue, 10, 10, scale, &font, "+ 至冬国铁路");
    draw_text_mut(
        &mut img,
        dark_blue,
        10,
        35,
        scale,
        &font,
        &format!(
            "{}  {}→{}",
            order.train_id, order.from_station, order.to_station
        ),
    );
    draw_text_mut(
        &mut img,
        dark_blue,
        10,
        55,
        scale,
        &font,
        &format!("站台: {}", order.from_platform),
    );
    draw_text_mut(
        &mut img,
        dark_blue,
        10,
        75,
        scale,
        &font,
        &format!("座号: {}", seat_number),
    );
    draw_text_mut(
        &mut img,
        dark_blue,
        10,
        95,
        scale,
        &font,
        &format!("票价: {} 摩拉", order.price / order.seat_count),
    );
    draw_text_mut(
        &mut img,
        dark_blue,
        10,
        115,
        scale,
        &font,
        &format!("订单号: {}", &order.id[..8]),
    );
    let bottom_text = "唯我皇安娜丝塔夏将带来无垢的黎明！";
    draw_text_mut(&mut img, dark_blue, 10, 140, scale, &font, bottom_text);

    // 3. 二维码（右下角，保持黑色）
    let qr = QrCode::new(order.id.as_bytes()).unwrap();
    let qr_image = qr.render::<image::Luma<u8>>().build();
    let qr_resized =
        image::imageops::resize(&qr_image, 60, 60, image::imageops::FilterType::Nearest);
    let qr_rgb = image::DynamicImage::ImageLuma8(qr_resized).into_rgb8();
    image::imageops::overlay(&mut img, &qr_rgb, (width - 70) as i64, (height - 70) as i64);

    // 4. 编码为PNG
    let mut buffer = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
        .unwrap();
    buffer
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
    let base_date =
        NaiveDate::parse_from_str(&form.date, "%Y-%m-%d").unwrap_or(Local::now().date_naive());
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

    let order_id = format!("{:08}", rand::thread_rng().gen_range(0..100000000));
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
        travel_date: form.date.clone(),
    };

    let mut orders = state.orders.write().unwrap();
    orders.insert(order_id.clone(), order);

    // 保存订单到文件
    let orders_clone = orders.clone();
    drop(orders); // 释放写锁
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
        // 立即同步保存索引文件
        save_order_index(&index);
    } // 释放索引锁

    // 3. 保存订单详细信息（同步写入，避免异步导致的延迟）
    {
        let orders = state.orders.read().unwrap();
        save_orders(&orders); // 同步写入
    }

    // 4. 跳转到车票打印页
    Redirect::to(&format!("/tickets/{}", order_id)).into_response()
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

    // 1. 获取订单并更新状态（在 orders 锁内）
    let (passenger_name, train_id, from, to, seat_count) = {
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
                order.seat_count,
            );
            // 保存订单（在锁内克隆整个 orders）
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

    // 2. 更新索引（单独获取 order_index 锁）
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

    // 3. 释放余票（获取 sold_counts 和 trains 锁）
    {
        let mut sold_counts = state.sold_counts.write().unwrap();
        let trains = state.trains.read().unwrap();
        if let Some(train) = trains.iter().find(|t| t.id == train_id) {
            if let Some((fi, ti)) = find_interval(train, &from, &to) {
                for i in fi..ti {
                    let key = (train_id.clone(), i);
                    if let Some(sold) = sold_counts.get_mut(&key) {
                        if *sold >= seat_count {
                            *sold -= seat_count;
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
    }

    // 4. 渲染结果
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
        if let Some(first_time_str) = train.departure_times.first().and_then(|o| o.as_ref()) {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                let now = Local::now().naive_local();
                let travel_date =
                    NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d").unwrap_or(now.date());
                let dep_datetime = travel_date.and_time(first_time);
                if now >= dep_datetime {
                    return (StatusCode::BAD_REQUEST, "已发车，不可改签").into_response();
                }
            }
        }
    }

    // 2. 释放原票（更新 sold_counts）
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

    // 3. 检查新车次是否有效且余票充足
    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let new_train = trains.iter_mut().find(|t| t.id == new_train_id);
    let new_train = match new_train {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "新车次不存在").into_response(),
    };
    // 刷新新车次状态（基于乘车日期）
    new_train.seats = compute_interval_seats_with_date(new_train, &sold_counts, &travel_date);
    update_train_status_with_date(new_train, &travel_date);
    if !new_train.is_selling {
        return (StatusCode::BAD_REQUEST, "新车次已停止售票或已发车").into_response();
    }

    // 在 if let 块内处理，并提取所需变量
    let (new_price_total, from_platform, _interval_price) = {
        if let Some((fi, ti)) = find_interval(new_train, &new_from, &new_to) {
            // 检查余票
            for i in fi..ti {
                if new_train.seats[i] < new_seat_count {
                    return (StatusCode::BAD_REQUEST, "新车次余票不足").into_response();
                }
            }
            // 扣减新车次余票
            for i in fi..ti {
                new_train.seats[i] -= new_seat_count;
            }
            // 更新 sold_counts
            for i in fi..ti {
                let key = (new_train_id.clone(), i);
                *sold_counts.entry(key).or_insert(0) += new_seat_count;
            }
            // 计算区间总价
            let interval_price: u32 = new_train.prices[fi..ti].iter().sum();
            let new_total = interval_price * new_seat_count;
            let final_price = if new_total > order.price { new_total } else { order.price };
            let from_platform = new_train.platforms[fi].clone();
            (final_price, from_platform, interval_price)
        } else {
            return (StatusCode::BAD_REQUEST, "新车次无效区间").into_response();
        }
    };

    // 4. 创建新订单
    let new_order_id = format!("{:08}", rand::thread_rng().gen_range(0..100000000));
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
        expire_at: Utc::now().timestamp() + 1800, // 30分钟
        created_at: Utc::now().timestamp(),
        pay_time: Some(Utc::now().timestamp()),
        travel_date: order.travel_date.clone(),
    };

    // 原订单标记为取消
    let old_order = orders.get_mut(&order_id).unwrap();
    old_order.status = OrderStatus::Cancelled;

    // 插入新订单
    orders.insert(new_order_id.clone(), new_order);

    // 更新索引（删除原订单号，添加新订单号）
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

    // 在更新索引后
    let orders_clone = orders.clone();
    drop(orders);
    tokio::task::spawn_blocking(move || {
        save_orders(&orders_clone);
    });

    // 5. 手动更新所有列车数据（避免死锁）
    for t in trains.iter_mut() {
        t.seats = compute_interval_seats_with_date(t, &sold_counts, &travel_date);
        update_train_status_with_date(t, &travel_date);
    }

    let mut ctx = Context::new();
    ctx.insert("page_title", "改签成功 - 至冬国铁路");
    ctx.insert("message", "改签成功，请查看新订单");
    ctx.insert("order_id", &order_id);
    let html = state.tera.render("exchange_result.html", &ctx).unwrap();
    Html(html).into_response()
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
        let trains = state.trains.read().unwrap();
        let train = trains.iter().find(|t| t.id == order.train_id);
        if let Some(train) = train {
            let seat_numbers = generate_seat_numbers(train, order.seat_count);
            let mut ctx = Context::new();
            let base_train_id = train.base_train_id.clone();
            ctx.insert("base_train_id", &base_train_id);
            ctx.insert("page_title", "车票打印 - 至冬国铁路");
            ctx.insert("order_id", &order_id);
            ctx.insert("seat_numbers", &seat_numbers);
            let html = state.tera.render("tickets.html", &ctx).unwrap();
            Html(html).into_response()
        } else {
            (StatusCode::NOT_FOUND, "列车信息不存在").into_response()
        }
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
        let trains = state.trains.read().unwrap();
        let train = trains.iter().find(|t| t.id == order.train_id);
        if let Some(train) = train {
            let seat_numbers = generate_seat_numbers(train, order.seat_count);
            if let Some(seat) = seat_numbers.get(seat_index) {
                let img_bytes = generate_ticket_image(&order, seat, train);
                ([(header::CONTENT_TYPE, "image/png")], img_bytes).into_response()
            } else {
                (StatusCode::NOT_FOUND, "座位索引无效").into_response()
            }
        } else {
            (StatusCode::NOT_FOUND, "列车不存在").into_response()
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
                println!("检测到文件变更，执行热重载...");
                reload_all(&state_clone);
            }
        }
    });

    // 构建限流配置
    let governor_conf = Arc::new(
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
        .route("/orders", get(orders_query))
        .route("/detail/{train_id}", get(detail_page))
        .route("/payment/{order_id}/pay", get(pay_order))
        .route("/payment/{order_id}", get(payment_page))
        .route("/notifications", get(notifications_page))
        .route("/exchange", get(exchange_page))
        .route("/exchange/query", get(exchange_query))
        .route("/exchange/refund", post(exchange_refund))
        .route("/exchange/change", post(exchange_change))
        .route("/my_orders", get(my_orders_page))
        .route("/my_orders/query", get(my_orders_query))
        .route("/tickets/{order_id}", get(tickets_page))
        .route("/ticket/{order_id}/{seat_index}", get(ticket_image))
        .route("/exchange/available", get(exchange_available))
        .route("/diagram", get(diagram_page))
        .route("/admin/reload", get(admin_reload))
        .nest_service("/static", ServeDir::new("static"))
        // .layer(GovernorLayer::new(governor_conf))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("至冬国铁路已启动：http://localhost:3000");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
