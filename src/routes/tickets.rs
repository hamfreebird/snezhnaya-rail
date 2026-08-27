use axum::{
    extract::{State, Path},
    response::{Html, IntoResponse, Response},
    http::{StatusCode, header},
};
use tera::Context;

use crate::state::SharedState;
use crate::ticket::generate_ticket_image;
use crate::models::OrderStatus;

pub async fn tickets_page(
    State(state): State<SharedState>,
    Path(order_id): Path<String>,
) -> Response {
    let orders = state.orders.read().await;
    let order = orders.get(&order_id).cloned();
    if let Some(order) = order {
        if order.status != OrderStatus::Paid {
            return (StatusCode::BAD_REQUEST, "订单未支付，无法打印车票").into_response();
        }
        let trains = state.trains.read().await;
        let seat_numbers: Vec<String> = order
            .seat_numbers
            .iter()
            .map(|&s| {
                let train = trains.iter().find(|t| t.id == order.train_id).unwrap();
                let carriage = (s - 1) / train.seats_per_carriage + 1;
                let seat = (s - 1) % train.seats_per_carriage + 1;
                format!("{}车{}号", carriage, seat)
            })
            .collect();
        let mut ctx = Context::new();
        let base_train_id = state.trains
            .read()
            .await
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

pub async fn ticket_image(
    State(state): State<SharedState>,
    Path((order_id, seat_index)): Path<(String, usize)>,
) -> Response {
    let orders = state.orders.read().await;
    let order = orders.get(&order_id).cloned();
    if let Some(order) = order {
        if order.status != OrderStatus::Paid {
            return (StatusCode::BAD_REQUEST, "订单未支付").into_response();
        }
        if seat_index < order.seat_numbers.len() {
            let seat = order.seat_numbers[seat_index];
            let trains = state.trains.read().await;
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