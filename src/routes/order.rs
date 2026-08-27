use axum::{
    extract::{State, Path, Query},
    response::{Html, IntoResponse, Redirect, Response, Json},
    http::StatusCode,
    Form,
};
use tera::Context;
use chrono::{Local, NaiveDate, Duration, Utc};
use std::collections::HashMap;
use uuid::Uuid;
use tracing::info;
use serde_json::json;

use crate::state::SharedState;
use crate::db::csv_loader::refresh_trains_with_date;
use crate::db::order_store::save_orders;
use crate::seats::allocate_seats_for_interval;
use crate::utils::helpers::{find_interval, generate_unique_order_id};
use crate::models::{Order, OrderStatus};

// ---------- 表单结构体 ----------
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

// ---------- 路由函数 ----------
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
    let base_date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap_or(Local::now().date_naive());
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
    let train = trains.iter_mut().find(|t| t.id == form.train_id);
    let train = match train {
        Some(t) => t,
        None => {
            return if is_ajax {
                (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "车次不存在" }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, "车次不存在").into_response()
            };
        }
    };
    if !train.is_selling {
        return if is_ajax {
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "该车次已停止售票" }))).into_response()
        } else {
            (StatusCode::BAD_REQUEST, "该车次已停止售票").into_response()
        };
    }

    let (fi, ti) = match find_interval(train, &form.from, &form.to) {
        Some((f, t)) => (f, t),
        None => {
            return if is_ajax {
                (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "无效的站点区间" }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, "无效的站点区间").into_response()
            };
        }
    };

    let assigned = match allocate_seats_for_interval(
        train,
        fi,
        ti,
        form.carriage_index,
        &form.seat_category,
        form.seat_count,
    ) {
        Some(seats) => seats,
        None => {
            return if is_ajax {
                (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "该类坐席余票不足" }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, "该类坐席余票不足").into_response()
            };
        }
    };

    let mut sold_counts = state.sold_counts.write().await;
    for i in fi..ti {
        let key = (form.train_id.clone(), i);
        let set = sold_counts.entry(key).or_insert_with(std::collections::HashSet::new);
        for &s in &assigned {
            set.insert(s);
        }
    }
    drop(sold_counts);

    let orders_map = state.orders.read().await;
    let order_id = generate_unique_order_id(&orders_map);
    drop(orders_map);

    let passenger_name_clone = form.passenger_name.clone();
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

    let mut orders = state.orders.write().await;
    orders.insert(order_id.clone(), order);
    let orders_clone = orders.clone();
    drop(orders);
    tokio::task::spawn_blocking(move || {
        save_orders(&orders_clone);
    });

    info!("订单创建成功: {}，用户: {}", order_id, passenger_name_clone);

    if is_ajax {
        let redirect_url = format!("/order_success?order_id={}&expire_at={}", order_id, expire_at.timestamp());
        (StatusCode::OK, Json(json!({ "success": true, "redirect": redirect_url }))).into_response()
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

    // 第一阶段：分配座位（在锁内）
    let (group_id, order1_id, order2_id) = {
        let mut trains = state.trains.write().await;
        let mut sold_counts = state.sold_counts.write().await;
        let mut orders = state.orders.write().await;

        info!("已获取 trains, sold_counts, orders 写锁");

        let train1_idx = match trains.iter().position(|t| t.id == form.first_train_id) {
            Some(idx) => idx,
            None => {
                return if is_ajax {
                    (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第一段车次不存在" }))).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "第一段车次不存在").into_response()
                };
            }
        };
        let train2_idx = match trains.iter().position(|t| t.id == form.second_train_id) {
            Some(idx) => idx,
            None => {
                return if is_ajax {
                    (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第二段车次不存在" }))).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "第二段车次不存在").into_response()
                };
            }
        };

        info!("找到两个车次索引");

        // 第一段
        let (fi1, _ti1, price1, assigned1) = {
            let train1 = &mut trains[train1_idx];
            if !train1.is_selling {
                return if is_ajax {
                    (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第一段车次已停止售票" }))).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "第一段车次已停止售票").into_response()
                };
            }
            let (fi, ti) = match find_interval(train1, &form.first_from, &form.first_to) {
                Some((f, t)) => (f, t),
                None => {
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第一段无效区间" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "第一段无效区间").into_response()
                    };
                }
            };
            let assigned = match allocate_seats_for_interval(
                train1,
                fi,
                ti,
                form.carriage_index,
                &form.seat_category,
                form.seat_count,
            ) {
                Some(seats) => seats,
                None => {
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第一段该类坐席余票不足" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "第一段该类坐席余票不足").into_response()
                    };
                }
            };
            for i in fi..ti {
                let key = (form.first_train_id.clone(), i);
                let set = sold_counts.entry(key).or_insert_with(std::collections::HashSet::new);
                for &s in &assigned {
                    set.insert(s);
                }
            }
            let price: u32 = train1.prices[fi..ti].iter().sum();
            info!("第一段分配完成，座位: {:?}", assigned);
            (fi, ti, price, assigned)
        };

        // 第二段
        let (fi2, _ti2, price2, assigned2) = {
            let train2 = &mut trains[train2_idx];
            if !train2.is_selling {
                return if is_ajax {
                    (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第二段车次已停止售票" }))).into_response()
                } else {
                    (StatusCode::BAD_REQUEST, "第二段车次已停止售票").into_response()
                };
            }
            let (fi, ti) = match find_interval(train2, &form.second_from, &form.second_to) {
                Some((f, t)) => (f, t),
                None => {
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第二段无效区间" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "第二段无效区间").into_response()
                    };
                }
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
                    let mut trains_ref = trains;
                    crate::seats::release_seats(
                        &form.first_train_id,
                        &form.first_from,
                        &form.first_to,
                        &assigned1,
                        &mut trains_ref,
                        &mut sold_counts,
                    );
                    return if is_ajax {
                        (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "message": "第二段该类坐席余票不足" }))).into_response()
                    } else {
                        (StatusCode::BAD_REQUEST, "第二段该类坐席余票不足").into_response()
                    };
                }
            };
            for i in fi..ti {
                let key = (form.second_train_id.clone(), i);
                let set = sold_counts.entry(key).or_insert_with(std::collections::HashSet::new);
                for &s in &assigned {
                    set.insert(s);
                }
            }
            let price: u32 = train2.prices[fi..ti].iter().sum();
            info!("第二段分配完成，座位: {:?}", assigned);
            (fi, ti, price, assigned)
        };

        let total_price1 = price1 * form.seat_count;
        let total_price2 = price2 * form.seat_count;

        let group_id = Uuid::new_v4().to_string();
        let expire_at = Utc::now() + Duration::minutes(30);

        info!("生成订单号...");
        let order1_id = generate_unique_order_id(&orders);
        let order2_id = generate_unique_order_id(&orders);
        info!("订单号生成完成: {}, {}", order1_id, order2_id);

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

        info!("插入订单...");
        orders.insert(order1_id.clone(), order1);
        orders.insert(order2_id.clone(), order2);
        info!("订单插入完成");

        // 克隆 orders 并释放锁
        let orders_clone = orders.clone();
        drop(orders);
        drop(sold_counts);
        drop(trains);
        info!("锁已释放");

        // 异步保存订单
        tokio::task::spawn_blocking(move || {
            save_orders(&orders_clone);
            info!("订单已异步保存");
        });

        info!("换乘订单创建成功，group_id: {}", group_id);
        (group_id, order1_id, order2_id)
    };

    if is_ajax {
        let redirect_url = format!("/transfer_confirm?group_id={}", group_id);
        info!("返回 AJAX 响应，重定向到: {}", redirect_url);
        (StatusCode::OK, Json(json!({ "success": true, "redirect": redirect_url }))).into_response()
    } else {
        info!("返回重定向到 transfer_confirm");
        Redirect::to(&format!("/transfer_confirm?group_id={}", group_id)).into_response()
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
    let expire_at = params.get("expire_at").cloned().unwrap_or_else(|| "0".to_string());
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