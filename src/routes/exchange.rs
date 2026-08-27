use axum::{
    extract::{State, Query},
    response::{Html, IntoResponse, Response, Json},
    http::StatusCode,
    Form,
};
use tera::Context;
use chrono::{Local, NaiveDate, NaiveTime, Utc, Datelike};
use serde_json::json;
use std::collections::HashMap;
use tracing::info;

use crate::state::SharedState;
use crate::models::{OrderStatus};
use crate::db::order_store::{save_orders, save_order_index};
use crate::db::csv_loader::refresh_trains_with_date;
use crate::seats::{allocate_seats_for_interval, release_seats};
use crate::utils::helpers::{find_interval, generate_unique_order_id};
use crate::models::Order;

// ---------- 改签/退签页面 ----------
pub async fn exchange_page(State(state): State<SharedState>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "改签/退签 - 至冬国铁路");
    let html = state.tera.render("exchange.html", &ctx).unwrap();
    Html(html).into_response()
}

pub async fn exchange_query(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }
    let orders = state.orders.read().await;
    if let Some(order) = orders.get(&order_id) {
        let mut ctx = Context::new();
        ctx.insert("page_title", "改签/退签 - 至冬国铁路");
        ctx.insert("order", order);
        let now = Local::now().naive_local();
        let trains = state.trains.read().await;
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

// ---------- 退签 ----------
pub async fn exchange_refund(
    State(state): State<SharedState>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }

    let (passenger_name, train_id, from, to, seat_numbers) = {
        let mut orders = state.orders.write().await;
        if let Some(order) = orders.get_mut(&order_id) {
            if order.status != OrderStatus::Paid {
                return (StatusCode::BAD_REQUEST, "订单状态不可退签").into_response();
            }
            let now = Local::now().naive_local();
            let trains = state.trains.read().await;
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
            order.status = OrderStatus::Refunded;
            let data = (
                order.passenger_name.clone(),
                order.train_id.clone(),
                order.from_station.clone(),
                order.to_station.clone(),
                order.seat_numbers.clone(),
            );
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

    // 释放座位
    {
        let mut sold_counts = state.sold_counts.write().await;
        let mut trains = state.trains.write().await;
        release_seats(&train_id, &from, &to, &seat_numbers, &mut trains, &mut sold_counts);
    }

    // 更新索引
    {
        let mut index = state.order_index.write().await;
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

    let today = Local::now().date_naive();
    refresh_trains_with_date(&state, &today).await;

    info!("订单 {} 退签成功，用户: {}", order_id, passenger_name);

    let mut ctx = Context::new();
    ctx.insert("page_title", "退签成功 - 至冬国铁路");
    ctx.insert("message", "退签成功，已释放座位");
    ctx.insert("order_id", &order_id);
    let html = state.tera.render("exchange_result.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 改签 ----------
pub async fn exchange_change(
    State(state): State<SharedState>,
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

    if order_id.is_empty() || new_train_id.is_empty() || new_from.is_empty() || new_to.is_empty() {
        return if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "参数不完整" }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, "参数不完整").into_response()
        };
    }

    let mut orders = state.orders.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut trains = state.trains.write().await;

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

    // 释放原票座位
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

    // 检查新车次并分配座位
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
    new_train.seats = crate::db::csv_loader::compute_interval_seats_with_date(new_train, &sold_counts, &travel_date);
    crate::db::csv_loader::update_train_status_with_date(new_train, &travel_date);
    if !new_train.is_selling {
        return if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "新车次已停止售票或已发车" }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, "新车次已停止售票或已发车").into_response()
        };
    }

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
                let set = sold_counts.entry(key).or_insert_with(std::collections::HashSet::new);
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

    let old_order = orders.get_mut(&order_id).unwrap();
    old_order.status = OrderStatus::Cancelled;

    orders.insert(new_order_id.clone(), new_order);

    // 更新索引
    let mut index = state.order_index.write().await;
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

    let orders_clone = orders.clone();
    drop(orders);
    tokio::task::spawn_blocking(move || {
        save_orders(&orders_clone);
    });

    // 手动更新所有列车数据
    for t in trains.iter_mut() {
        t.seats = crate::db::csv_loader::compute_interval_seats_with_date(t, &sold_counts, &travel_date);
        crate::db::csv_loader::update_train_status_with_date(t, &travel_date);
    }

    info!("订单 {} 改签为 {}，新订单: {}", order_id, new_train_id, new_order_id);

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

// ---------- 可用车次查询 ----------
pub async fn exchange_available(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }

    let orders = state.orders.read().await;
    let order = orders.get(&order_id).cloned();
    let order = match order {
        Some(o) => o,
        None => return (StatusCode::NOT_FOUND, "订单不存在").into_response(),
    };

    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let weekday = travel_date.weekday().number_from_monday() as u8;

    let from = &order.from_station;
    let to = &order.to_station;

    let trains = state.trains.read().await;
    let now = Local::now().naive_local();
    let mut available = Vec::new();

    for train in trains.iter() {
        if train.run_day != weekday {
            continue;
        }
        if let Some((fi, ti)) = find_interval(train, from, to) {
            if let Some(Some(first_dep_str)) = train.departure_times.first() {
                if let Ok(first_dep) = NaiveTime::parse_from_str(first_dep_str, "%H:%M") {
                    let dep_datetime = travel_date.and_time(first_dep);
                    if now >= dep_datetime {
                        continue;
                    }
                }
            }
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            if min_seats == 0 {
                continue;
            }
            let price: u32 = train.prices[fi..ti].iter().sum();
            let departure = train.departure_times[fi].as_deref().unwrap_or("");
            let arrival = train.arrival_times[ti].as_deref().unwrap_or("");

            available.push(json!({
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

    available.sort_by(|a, b| {
        a["departure"].as_str().unwrap().cmp(b["departure"].as_str().unwrap())
    });

    ([(axum::http::header::CONTENT_TYPE, "application/json")], serde_json::to_string(&available).unwrap()).into_response()
}