// src/cleanup.rs
//! 订单超时清理任务
//! 定期扫描 Pending 且已过期的订单，取消订单并释放座位。

use crate::state::SharedState;
use crate::db::order_store::{save_orders, save_order_index, save_sold_counts};
use crate::seats::release_seats;
use crate::models::OrderStatus;
use chrono::{Local, Utc};
use tracing::{info, error};

pub async fn cleanup_expired_orders(state: &SharedState) {
    let now_ts = Utc::now().timestamp();

    let mut trains = state.trains.write().await;
    let mut sold_counts = state.sold_counts.write().await;
    let mut orders = state.orders.write().await;
    let mut index = state.order_index.write().await;

    let mut cancelled = Vec::new();
    for (_, order) in orders.iter_mut() {
        if order.status == OrderStatus::Pending && order.expire_at < now_ts {
            order.status = OrderStatus::Cancelled;
            cancelled.push(order.clone());
        }
    }

    if cancelled.is_empty() {
        return;
    }

    for order in &cancelled {
        release_seats(
            &order.train_id,
            &order.travel_date,
            &order.from_station,
            &order.to_station,
            &order.seat_numbers,
            &trains,
            &mut sold_counts,
        );
    }

    for order in &cancelled {
        if let Some(list) = index.get_mut(&order.passenger_name) {
            list.retain(|id| id != &order.id);
            if list.is_empty() {
                index.remove(&order.passenger_name);
            }
        }
    }

    let now_date = Local::now().date_naive();
    for train in trains.iter_mut() {
        train.seats = crate::db::csv_loader::compute_interval_seats_with_date(
            train,
            &sold_counts,
            &now_date,
        );
    }

    let orders_clone = orders.clone();
    let index_clone = index.clone();
    let sold_clone = sold_counts.clone();

    drop(trains);
    drop(sold_counts);
    drop(orders);
    drop(index);

    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = save_orders(&orders_clone) {
            error!("清理任务：保存订单失败: {}", e);
        }
        if let Err(e) = save_order_index(&index_clone) {
            error!("清理任务：保存订单索引失败: {}", e);
        }
        if let Err(e) = save_sold_counts(&sold_clone) {
            error!("清理任务：保存已售座位失败: {}", e);
        }
    })
    .await;

    info!("已清理 {} 个过期订单", cancelled.len());
}