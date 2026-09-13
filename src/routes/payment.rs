// src/routes/payment.rs
use axum::{
    extract::{State, Path},
    response::{Html, IntoResponse, Redirect, Response},
    http::StatusCode,
};
use tera::Context;
use chrono::Utc;
use tracing::{info, error};

use crate::state::SharedState;
use crate::db::order_store::{save_orders, save_order_index};

pub async fn payment_page(
    State(state): State<SharedState>,
    Path(order_id): Path<String>,
) -> Response {
    let orders = state.orders.read().await;
    if let Some(order) = orders.get(&order_id) {
        if order.status != crate::models::OrderStatus::Pending {
            return (StatusCode::BAD_REQUEST, "订单状态异常").into_response();
        }
        if order.expire_at < Utc::now().timestamp() {
            return (StatusCode::BAD_REQUEST, "订单已过期").into_response();
        }
        let trains = state.trains.read().await;
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

pub async fn pay_order(
    State(state): State<SharedState>,
    Path(order_id): Path<String>,
) -> Response {
    let now_ts = Utc::now().timestamp();

    // 支付前检查订单状态与过期时间（过期订单交由清理任务统一处理）
    {
        let orders = state.orders.read().await;
        match orders.get(&order_id) {
            Some(order) => {
                if order.status != crate::models::OrderStatus::Pending {
                    return (StatusCode::BAD_REQUEST, "订单已处理").into_response();
                }
                if order.expire_at < now_ts {
                    return (StatusCode::BAD_REQUEST, "订单已过期").into_response();
                }
            }
            None => return (StatusCode::NOT_FOUND, "订单不存在").into_response(),
        }
    }

    // 更新订单状态
    let (passenger_name, order_data) = {
        let mut orders = state.orders.write().await;
        if let Some(order) = orders.get_mut(&order_id) {
            if order.status != crate::models::OrderStatus::Pending {
                return (StatusCode::BAD_REQUEST, "订单已处理").into_response();
            }
            if order.expire_at < now_ts {
                return (StatusCode::BAD_REQUEST, "订单已过期").into_response();
            }
            order.status = crate::models::OrderStatus::Paid;
            order.pay_time = Some(now_ts);
            let name = order.passenger_name.clone();
            let data = order.clone();
            (name, data)
        } else {
            return (StatusCode::NOT_FOUND, "订单不存在").into_response();
        }
    };

    // 更新索引
    {
        let mut index = state.order_index.write().await;
        index
            .entry(passenger_name.clone())
            .or_insert_with(Vec::new)
            .push(order_id.clone());
        let index_clone = index.clone();
        drop(index);
        tokio::task::spawn_blocking(move || {
            if let Err(e) = save_order_index(&index_clone) {
                error!("支付保存索引失败: {}", e);
            }
        });
    }

    // 持久化订单
    {
        let orders = state.orders.read().await;
        let orders_clone = orders.clone();
        drop(orders);
        tokio::task::spawn_blocking(move || {
            if let Err(e) = save_orders(&orders_clone) {
                error!("支付保存订单失败: {}", e);
            }
        });
    }

    info!("订单 {} 支付成功，用户: {}", order_id, passenger_name);

    if let Some(group_id) = &order_data.group_id {
        Redirect::to(&format!("/transfer_confirm?group_id={}", group_id)).into_response()
    } else {
        Redirect::to(&format!("/tickets/{}", order_id)).into_response()
    }
}