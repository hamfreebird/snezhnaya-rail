// src/routes/order.rs
use axum::{
    extract::{State, Path, Query},
    response::{Html, IntoResponse, Redirect, Response, Json},
    http::StatusCode,
    Form,
};
use tera::Context;
use chrono::{Local, NaiveDate, Duration, Utc};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use tracing::{info, error};
use serde_json::json;

use crate::state::SharedState;
use crate::db::csv_loader::refresh_trains_with_date;
use crate::db::order_store::{save_orders, save_sold_counts};
use crate::seats::allocate_seats_for_interval;
use crate::utils::helpers::{find_interval, generate_unique_order_id};
use crate::models::{Order, OrderStatus};

#[derive(Debug, serde::Deserialize)]
pub struct CreateOrderForm {
    pub train_id: String,
    pub from: String,
    pub from_platform: String,
    pub to: String,
    pub total_price: u32,
    pub passenger_name: String,
    pub seat_count: u32,
    pub date: String,
    pub carriage_index: u32,
    pub seat_category: String,
    pub _ajax: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TransferOrderForm {
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
    pub _ajax: Option<String>,
}

fn err_response(is_ajax: bool, msg: &str, code: StatusCode) -> Response {
    let body = msg.to_string();
    if is_ajax {
        (code, Json(json!({ "success": false, "message": body }))).into_response()
    } else {
        (code, body).into_response()
    }
}

pub async fn order_form(
    State(state): State<SharedState>,
    Path(train_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let from = params.get("from").cloned().unwrap_or_default();
    let to = params.get("to").cloned().unwrap_or_default();
    let date_str = params
        .get("date")
        .cloned()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let base_date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
        .unwrap_or(Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date).await;

    if from.is_empty() || to.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少出发站或到达站").into_response();
    }

    let trains = state.trains.read().await;
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

pub async fn create_order(
    State(state): State<SharedState>,
    Form(form): Form<CreateOrderForm>,
) -> Response {
    let is_ajax = form._ajax.as_deref() == Some("1");
    let base_date = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date).await;

    let mut trains = state.trains.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut orders = state.orders.write().await;

    let train_idx = match trains.iter().position(|t| t.id == form.train_id) {
        Some(i) => i,
        None => return err_response(is_ajax, "车次不存在", StatusCode::BAD_REQUEST),
    };
    if !trains[train_idx].is_selling {
        return err_response(is_ajax, "该车次已停止售票", StatusCode::BAD_REQUEST);
    }

    let (fi, ti) = match find_interval(&trains[train_idx], &form.from, &form.to) {
        Some((f, t)) => (f, t),
        None => return err_response(is_ajax, "无效的站点区间", StatusCode::BAD_REQUEST),
    };

    let assigned = match allocate_seats_for_interval(
        &mut trains[train_idx],
        fi,
        ti,
        form.carriage_index,
        &form.seat_category,
        form.seat_count,
    ) {
        Some(seats) => seats,
        None => return err_response(is_ajax, "该类坐席余票不足", StatusCode::BAD_REQUEST),
    };

    for i in fi..ti {
        let key = (form.train_id.clone(), i);
        let set = sold_counts.entry(key).or_insert_with(HashSet::new);
        for &s in &assigned {
            set.insert(s);
        }
    }

    let order_id = generate_unique_order_id(&orders);
    let expire_minutes = crate::config::get().timeouts.order_expire_minutes;
    let expire_at = Utc::now() + Duration::minutes(expire_minutes);

    let order = Order {
        id: order_id.clone(),
        train_id: form.train_id.clone(),
        from_station: form.from.clone(),
        to_station: form.to.clone(),
        from_platform: form.from_platform.clone(),
        passenger_name: form.passenger_name.clone(),
        seat_count: form.seat_count,
        price: form.total_price * form.seat_count,
        status: OrderStatus::Pending,
        expire_at: expire_at.timestamp(),
        created_at: Utc::now().timestamp(),
        pay_time: None,
        travel_date: form.date.clone(),
        group_id: None,
        seat_numbers: assigned.clone(),
        seat_category: form.seat_category.clone(),
        carriage_index: form.carriage_index,
    };

    orders.insert(order_id.clone(), order);

    let save_ok = match save_orders(&orders) {
        Ok(_) => match save_sold_counts(&sold_counts) {
            Ok(_) => true,
            Err(e) => {
                error!("保存已售座位失败: {}", e);
                false
            }
        },
        Err(e) => {
            error!("保存订单失败: {}", e);
            false
        }
    };

    if !save_ok {
        orders.remove(&order_id);
        for i in fi..ti {
            let key = (form.train_id.clone(), i);
            if let Some(set) = sold_counts.get_mut(&key) {
                for &s in &assigned {
                    set.remove(&s);
                }
            }
            for &s in &assigned {
                trains[train_idx].available_seats[i].push(s);
            }
            trains[train_idx].available_seats[i].sort();
            trains[train_idx].available_seats[i].dedup();
        }
        trains[train_idx].seats = crate::db::csv_loader::compute_interval_seats_with_date(
            &trains[train_idx],
            &sold_counts,
            &base_date,
        );
        return err_response(
            is_ajax,
            "保存订单失败，请重试",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    info!("订单创建成功: {}，用户: {}", order_id, form.passenger_name);

    if is_ajax {
        let redirect_url = format!(
            "/order_success?order_id={}&expire_at={}",
            order_id,
            expire_at.timestamp()
        );
        (StatusCode::OK, Json(json!({ "success": true, "redirect": redirect_url })))
            .into_response()
    } else {
        let mut ctx = Context::new();
        ctx.insert("page_title", "占座成功 - 至冬国铁路");
        ctx.insert("order_id", &order_id);
        ctx.insert("expire_at", &expire_at.timestamp());
        let html = state.tera.render("order_success.html", &ctx).unwrap();
        Html(html).into_response()
    }
}

pub async fn order_transfer(
    State(state): State<SharedState>,
    Form(form): Form<TransferOrderForm>,
) -> Response {
    let is_ajax = form._ajax.as_deref() == Some("1");
    let base_date = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date).await;

    info!("进入 order_transfer，is_ajax={}", is_ajax);

    let mut trains = state.trains.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut orders = state.orders.write().await;

    let train1_idx = match trains.iter().position(|t| t.id == form.first_train_id) {
        Some(i) => i,
        None => return err_response(is_ajax, "第一段车次不存在", StatusCode::BAD_REQUEST),
    };
    let train2_idx = match trains.iter().position(|t| t.id == form.second_train_id) {
        Some(i) => i,
        None => return err_response(is_ajax, "第二段车次不存在", StatusCode::BAD_REQUEST),
    };

    if !trains[train1_idx].is_selling {
        return err_response(is_ajax, "第一段车次已停止售票", StatusCode::BAD_REQUEST);
    }
    let (fi1, ti1) = match find_interval(&trains[train1_idx], &form.first_from, &form.first_to) {
        Some(x) => x,
        None => return err_response(is_ajax, "第一段无效区间", StatusCode::BAD_REQUEST),
    };

    let assigned1 = match allocate_seats_for_interval(
        &mut trains[train1_idx],
        fi1,
        ti1,
        form.carriage_index,
        &form.seat_category,
        form.seat_count,
    ) {
        Some(s) => s,
        None => return err_response(is_ajax, "第一段该类坐席余票不足", StatusCode::BAD_REQUEST),
    };
    for i in fi1..ti1 {
        let key = (form.first_train_id.clone(), i);
        let set = sold_counts.entry(key).or_insert_with(HashSet::new);
        for &s in &assigned1 {
            set.insert(s);
        }
    }
    let price1: u32 = trains[train1_idx].prices[fi1..ti1].iter().sum();

    if !trains[train2_idx].is_selling {
        rollback_seats(
            &mut trains[train1_idx],
            &mut sold_counts,
            &form.first_train_id,
            fi1,
            ti1,
            &assigned1,
        );
        return err_response(is_ajax, "第二段车次已停止售票", StatusCode::BAD_REQUEST);
    }
    let (fi2, ti2) = match find_interval(&trains[train2_idx], &form.second_from, &form.second_to) {
        Some(x) => x,
        None => {
            rollback_seats(
                &mut trains[train1_idx],
                &mut sold_counts,
                &form.first_train_id,
                fi1,
                ti1,
                &assigned1,
            );
            return err_response(is_ajax, "第二段无效区间", StatusCode::BAD_REQUEST);
        }
    };

    let assigned2 = match allocate_seats_for_interval(
        &mut trains[train2_idx],
        fi2,
        ti2,
        form.carriage_index,
        &form.seat_category,
        form.seat_count,
    ) {
        Some(s) => s,
        None => {
            rollback_seats(
                &mut trains[train1_idx],
                &mut sold_counts,
                &form.first_train_id,
                fi1,
                ti1,
                &assigned1,
            );
            return err_response(is_ajax, "第二段该类坐席余票不足", StatusCode::BAD_REQUEST);
        }
    };
    for i in fi2..ti2 {
        let key = (form.second_train_id.clone(), i);
        let set = sold_counts.entry(key).or_insert_with(HashSet::new);
        for &s in &assigned2 {
            set.insert(s);
        }
    }
    let price2: u32 = trains[train2_idx].prices[fi2..ti2].iter().sum();

    let group_id = Uuid::new_v4().to_string();
    let expire_minutes = crate::config::get().timeouts.order_expire_minutes;
    let expire_at = Utc::now() + Duration::minutes(expire_minutes);
    let order1_id = generate_unique_order_id(&orders);
    let order2_id = generate_unique_order_id(&orders);

    let order1 = Order {
        id: order1_id.clone(),
        train_id: form.first_train_id.clone(),
        from_station: form.first_from.clone(),
        to_station: form.first_to.clone(),
        from_platform: trains[train1_idx].platforms[fi1].clone(),
        passenger_name: form.passenger_name.clone(),
        seat_count: form.seat_count,
        price: price1 * form.seat_count,
        status: OrderStatus::Pending,
        expire_at: expire_at.timestamp(),
        created_at: Utc::now().timestamp(),
        pay_time: None,
        travel_date: form.date.clone(),
        group_id: Some(group_id.clone()),
        seat_numbers: assigned1.clone(),
        seat_category: form.seat_category.clone(),
        carriage_index: form.carriage_index,
    };
    let order2 = Order {
        id: order2_id.clone(),
        train_id: form.second_train_id.clone(),
        from_station: form.second_from.clone(),
        to_station: form.second_to.clone(),
        from_platform: trains[train2_idx].platforms[fi2].clone(),
        passenger_name: form.passenger_name.clone(),
        seat_count: form.seat_count,
        price: price2 * form.seat_count,
        status: OrderStatus::Pending,
        expire_at: expire_at.timestamp(),
        created_at: Utc::now().timestamp(),
        pay_time: None,
        travel_date: form.date.clone(),
        group_id: Some(group_id.clone()),
        seat_numbers: assigned2.clone(),
        seat_category: form.seat_category.clone(),
        carriage_index: form.carriage_index,
    };

    orders.insert(order1_id.clone(), order1);
    orders.insert(order2_id.clone(), order2);

    let save_ok = match save_orders(&orders) {
        Ok(_) => match save_sold_counts(&sold_counts) {
            Ok(_) => true,
            Err(e) => {
                error!("保存已售座位失败: {}", e);
                false
            }
        },
        Err(e) => {
            error!("保存订单失败: {}", e);
            false
        }
    };

    if !save_ok {
        orders.remove(&order1_id);
        orders.remove(&order2_id);
        rollback_seats(
            &mut trains[train1_idx],
            &mut sold_counts,
            &form.first_train_id,
            fi1,
            ti1,
            &assigned1,
        );
        rollback_seats(
            &mut trains[train2_idx],
            &mut sold_counts,
            &form.second_train_id,
            fi2,
            ti2,
            &assigned2,
        );
        return err_response(
            is_ajax,
            "保存订单失败，请重试",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    info!("换乘订单创建成功，group_id: {}", group_id);

    if is_ajax {
        let redirect_url = format!("/transfer_confirm?group_id={}", group_id);
        (StatusCode::OK, Json(json!({ "success": true, "redirect": redirect_url })))
            .into_response()
    } else {
        Redirect::to(&format!("/transfer_confirm?group_id={}", group_id)).into_response()
    }
}

fn rollback_seats(
    train: &mut crate::models::Train,
    sold_counts: &mut HashMap<(String, usize), HashSet<u32>>,
    train_id: &str,
    fi: usize,
    ti: usize,
    assigned: &[u32],
) {
    for i in fi..ti {
        let key = (train_id.to_string(), i);
        if let Some(set) = sold_counts.get_mut(&key) {
            for &s in assigned {
                set.remove(&s);
            }
        }
        for &s in assigned {
            train.available_seats[i].push(s);
        }
        train.available_seats[i].sort();
        train.available_seats[i].dedup();
    }
}

pub async fn order_transfer_form(
    State(state): State<SharedState>,
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

    let trains = state.trains.read().await;
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
    ctx.insert("carriage_count", &carriage_count);
    let html = state.tera.render("order_transfer_form.html", &ctx).unwrap();
    Html(html).into_response()
}

pub async fn order_success_page(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let order_id = params.get("order_id").cloned().unwrap_or_default();
    let expire_at = params
        .get("expire_at")
        .cloned()
        .unwrap_or_else(|| "0".to_string());
    if order_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少订单号").into_response();
    }
    let mut ctx = Context::new();
    ctx.insert("page_title", "占座成功 - 至冬国铁路");
    ctx.insert("order_id", &order_id);
    ctx.insert("expire_at", &expire_at);
    let html = state.tera.render("order_success.html", &ctx).unwrap();
    Html(html).into_response()
}