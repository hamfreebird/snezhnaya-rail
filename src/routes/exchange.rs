// src/routes/exchange.rs
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
use tracing::{info, error};

use crate::state::SharedState;
use crate::models::OrderStatus;
use crate::db::order_store::{
    add_seats, date_key, save_orders, save_order_index, save_sold_counts,
};
use crate::db::csv_loader::refresh_trains_with_date;
use crate::seats::{allocate_seats_for_interval, release_seats, remove_interval_seats};
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
    let order = {
        let orders = state.orders.read().await;
        orders.get(&order_id).cloned()
    };
    if let Some(order) = order {
        let mut ctx = Context::new();
        ctx.insert("page_title", "改签/退签 - 至冬国铁路");
        ctx.insert("order", &order);
        let now = Local::now().naive_local();
        let trains = state.trains.read().await;
        let train = trains.iter().find(|t| t.id == order.train_id);
        let can_operate = if let Some(t) = train {
            if let Some(first_time_str) = t.departure_times.first().and_then(|o| o.as_ref()) {
                if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
                        .unwrap_or(now.date());
                    let dep_datetime = travel_date.and_time(first_time);
                    now < dep_datetime && order.status == OrderStatus::Paid
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

    // 统一锁顺序：trains -> sold_counts -> orders -> index
    let mut trains = state.trains.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut orders = state.orders.write().await;
    let mut index = state.order_index.write().await;

    let order = match orders.get_mut(&order_id) {
        Some(o) => {
            if o.status != OrderStatus::Paid {
                return (StatusCode::BAD_REQUEST, "订单状态不可退签").into_response();
            }
            // 发车检查
            let travel_date = NaiveDate::parse_from_str(&o.travel_date, "%Y-%m-%d")
                .unwrap_or(Local::now().date_naive());
            if let Some(t) = trains.iter().find(|t| t.id == o.train_id) {
                if let Some(first_time_str) = t.departure_times.first().and_then(|x| x.as_ref()) {
                    if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                        let dep_datetime = travel_date.and_time(first_time);
                        if Local::now().naive_local() >= dep_datetime {
                            return (StatusCode::BAD_REQUEST, "已发车，不可退签").into_response();
                        }
                    }
                }
            }
            o.status = OrderStatus::Refunded;
            o.clone()
        }
        None => return (StatusCode::NOT_FOUND, "订单不存在").into_response(),
    };

    // 释放座位：只归还该订单实际经过的区间与乘车日期
    release_seats(
        &order.train_id,
        &order.travel_date,
        &order.from_station,
        &order.to_station,
        &order.seat_numbers,
        &trains,
        &mut sold_counts,
    );

    // 重新计算余票（使用订单的乘车日期，避免把其他日期的占用算进来）
    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    for t in trains.iter_mut() {
        t.seats =
            crate::db::csv_loader::compute_interval_seats_with_date(t, &sold_counts, &travel_date);
    }

    // 更新索引
    if let Some(list) = index.get_mut(&order.passenger_name) {
        list.retain(|id| id != &order_id);
        if list.is_empty() {
            index.remove(&order.passenger_name);
        }
    }

    // 持久化
    let orders_clone = orders.clone();
    let index_clone = index.clone();
    let sold_clone = sold_counts.clone();

    drop(trains);
    drop(sold_counts);
    drop(orders);
    drop(index);

    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = save_orders(&orders_clone) {
            error!("退签保存订单失败: {}", e);
        }
        if let Err(e) = save_order_index(&index_clone) {
            error!("退签保存索引失败: {}", e);
        }
        if let Err(e) = save_sold_counts(&sold_clone) {
            error!("退签保存已售座位失败: {}", e);
        }
    })
    .await;

    info!("订单 {} 退签成功，用户: {}", order_id, order.passenger_name);

    let mut ctx = Context::new();
    ctx.insert("page_title", "退签成功 - 至冬国铁路");
    ctx.insert("message", "退签成功，已释放座位");
    ctx.insert("order_id", &order_id);
    let html = state.tera.render("exchange_result.html", &ctx).unwrap();
    Html(html).into_response()
}

// ---------- 改签 ----------
/// 原子性修复：先分配新车次 → 创建新订单 → 同步保存 → 成功后释放原票并取消原订单
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

    let err = |msg: &str| -> Response {
        let body = msg.to_string();
        if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": body }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, body).into_response()
        }
    };

    if order_id.is_empty() || new_train_id.is_empty() || new_from.is_empty() || new_to.is_empty() {
        return err("参数不完整");
    }

    // 统一锁顺序
    let mut trains = state.trains.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut orders = state.orders.write().await;

    // 1. 取出原订单并校验
    let order = match orders.get(&order_id) {
        Some(o) => {
            if o.status != OrderStatus::Paid {
                return err("原订单状态不可改签");
            }
            o.clone()
        }
        None => return err("原订单不存在"),
    };

    // 检查原订单是否已发车
    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let travel_date_key = date_key(&travel_date);
    if new_seat_count == 0 {
        return err("改签张数必须大于 0");
    }
    if let Some(t) = trains.iter().find(|t| t.id == order.train_id) {
        if let Some(first_time_str) = t.departure_times.first().and_then(|x| x.as_ref()) {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                let dep_datetime = travel_date.and_time(first_time);
                if Local::now().naive_local() >= dep_datetime {
                    return err("已发车，不可改签");
                }
            }
        }
    }

    // 2. 查找新车次
    let new_train_idx = match trains.iter().position(|t| t.id == new_train_id) {
        Some(i) => i,
        None => return err("新车次不存在"),
    };

    // 刷新新车次余票与状态
    {
        let new_train = &mut trains[new_train_idx];
        new_train.seats = crate::db::csv_loader::compute_interval_seats_with_date(
            new_train,
            &sold_counts,
            &travel_date,
        );
        crate::db::csv_loader::update_train_status_with_date(new_train, &travel_date);
        if !new_train.is_selling {
            return err("新车次已停止售票或已发车");
        }
    }

    // 3. 预分配新车次座位（此时不修改原订单）
    let (new_from_platform, assigned_seats, new_total_price, new_fi, new_ti) = {
        let new_train = &trains[new_train_idx];
        let (fi, ti) = match find_interval(new_train, &new_from, &new_to) {
            Some(x) => x,
            None => return err("新车次无效区间"),
        };
        let assigned = match allocate_seats_for_interval(
            new_train,
            &sold_counts,
            &travel_date,
            fi,
            ti,
            new_carriage,
            &new_category,
            new_seat_count,
        ) {
            Some(s) => s,
            None => return err("该坐席类型余票不足，请选择其他车厢或类别"),
        };
        for i in fi..ti {
            add_seats(
                &mut sold_counts,
                &new_train_id,
                i,
                &travel_date_key,
                &assigned,
            );
        }
        let interval_price: u32 = new_train.prices[fi..ti].iter().sum();
        // 改签后的订单金额就是新车次的实际总价：更贵补差、更便宜降价。
        let new_total = interval_price * new_seat_count;
        (
            new_train.platforms[fi].clone(),
            assigned,
            new_total,
            fi,
            ti,
        )
    };

    // 4. 创建新订单
    let new_order_id = generate_unique_order_id(&orders);
    let new_order = Order {
        id: new_order_id.clone(),
        train_id: new_train_id.clone(),
        from_station: new_from.clone(),
        to_station: new_to.clone(),
        from_platform: new_from_platform,
        passenger_name: order.passenger_name.clone(),
        seat_count: new_seat_count,
        price: new_total_price,
        status: OrderStatus::Paid,
        expire_at: Utc::now().timestamp()
            + crate::config::get().timeouts.order_expire_minutes * 60,
        created_at: Utc::now().timestamp(),
        pay_time: Some(Utc::now().timestamp()),
        travel_date: travel_date_key.clone(),
        group_id: None,
        seat_numbers: assigned_seats.clone(),
        seat_category: new_category.clone(),
        carriage_index: new_carriage,
    };
    orders.insert(new_order_id.clone(), new_order);

    // 5. 同步持久化（新订单 + 新座位状态）
    let save_ok = match save_orders(&orders) {
        Ok(_) => match save_sold_counts(&sold_counts) {
            Ok(_) => true,
            Err(e) => {
                error!("改签保存已售座位失败: {}", e);
                false
            }
        },
        Err(e) => {
            error!("改签保存订单失败: {}", e);
            false
        }
    };

    if !save_ok {
        // 回滚：移除新订单，释放新车次座位
        orders.remove(&new_order_id);
        remove_interval_seats(
            &new_train_id,
            &travel_date_key,
            new_fi,
            new_ti,
            &assigned_seats,
            &mut sold_counts,
        );
        return if is_ajax {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": "保存失败，请重试" }))).into_response()
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, "保存失败，请重试").into_response()
        };
    }

    // 6. 持久化成功后：释放原票座位
    release_seats(
        &order.train_id,
        &travel_date_key,
        &order.from_station,
        &order.to_station,
        &order.seat_numbers,
        &trains,
        &mut sold_counts,
    );

    // 7. 取消原订单
    if let Some(o) = orders.get_mut(&order_id) {
        o.status = OrderStatus::Cancelled;
    }

    // 8. 更新索引
    {
        let mut index = state.order_index.write().await;
        let name = order.passenger_name.clone();
        if let Some(list) = index.get_mut(&name) {
            list.retain(|id| id != &order_id);
        }
        index
            .entry(name)
            .or_insert_with(Vec::new)
            .push(new_order_id.clone());
        let index_clone = index.clone();
        drop(index);
        tokio::task::spawn_blocking(move || {
            if let Err(e) = save_order_index(&index_clone) {
                error!("改签保存索引失败: {}", e);
            }
        });
    }

    // 9. 再次同步持久化（原订单状态、原票释放后的座位）
    let orders_clone = orders.clone();
    let sold_clone = sold_counts.clone();
    drop(orders);
    drop(sold_counts);
    drop(trains);

    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = save_orders(&orders_clone) {
            error!("改签最终保存订单失败: {}", e);
        }
        if let Err(e) = save_sold_counts(&sold_clone) {
            error!("改签最终保存已售座位失败: {}", e);
        }
    })
    .await;

    // 重新计算所有车次余票
    refresh_trains_with_date(&state, &travel_date).await;

    info!("订单 {} 改签为 {}，新订单: {}", order_id, new_train_id, new_order_id);

    if is_ajax {
        (StatusCode::OK, Json(json!({ "success": true, "order_id": new_order_id }))).into_response()
    } else {
        let mut ctx = Context::new();
        ctx.insert("page_title", "改签成功 - 至冬国铁路");
        ctx.insert("message", "改签成功，请查看新订单");
        ctx.insert("order_id", &new_order_id);
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

    let order = {
        let orders = state.orders.read().await;
        match orders.get(&order_id) {
            Some(o) => o.clone(),
            None => return (StatusCode::NOT_FOUND, "订单不存在").into_response(),
        }
    };

    let travel_date = NaiveDate::parse_from_str(&order.travel_date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let weekday = travel_date.weekday().number_from_monday() as u8;

    let from = &order.from_station;
    let to = &order.to_station;

    refresh_trains_with_date(&state, &travel_date).await;
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
            let unit_price: u32 = train.prices[fi..ti].iter().sum();
            let total_price = unit_price.saturating_mul(order.seat_count);
            let departure = train.departure_times[fi].as_deref().unwrap_or("");
            let arrival = train.arrival_times[ti].as_deref().unwrap_or("");

            available.push(json!({
                "id": train.id,
                "base_train_id": train.base_train_id,
                "name": train.name,
                "departure": departure,
                "arrival": arrival,
                // price 与订单总价同口径，unit_price 供需要按人展示的前端使用
                "price": total_price,
                "unit_price": unit_price,
                "seats": min_seats,
                "carriage_count": train.carriage_count,
            }));
        }
    }

    available.sort_by(|a, b| {
        a["departure"].as_str().unwrap().cmp(b["departure"].as_str().unwrap())
    });

    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&available).unwrap(),
    )
        .into_response()
}