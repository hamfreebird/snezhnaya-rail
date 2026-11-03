use axum::{
    extract::{State, Path},
    response::{Html, IntoResponse, Response},
    http::{StatusCode, header},
};
use tera::Context;

use crate::state::SharedState;
use crate::ticket::generate_ticket_image;
use crate::models::{OrderStatus, Train};

async fn load_order_train(
    state: &SharedState,
    order_id: &str,
) -> Result<(crate::models::Order, Train), Response> {
    // 先取订单并释放 orders 读锁，再取 trains 读锁，避免与写路径形成锁序反转。
    let order = {
        let orders = state.orders.read().await;
        orders.get(order_id).cloned()
    };
    let order = match order {
        Some(order) => order,
        None => return Err((StatusCode::NOT_FOUND, "订单不存在").into_response()),
    };
    if order.status != OrderStatus::Paid {
        return Err((StatusCode::BAD_REQUEST, "订单未支付").into_response());
    }
    let train = {
        let trains = state.trains.read().await;
        trains.iter().find(|t| t.id == order.train_id).cloned()
    };
    match train {
        Some(train) => Ok((order, train)),
        None => Err((StatusCode::NOT_FOUND, "订单对应的车次不存在").into_response()),
    }
}

fn seat_label(seat: u32, train: &Train) -> String {
    let seats_per_carriage = train.seats_per_carriage.max(1);
    let carriage = (seat.saturating_sub(1)) / seats_per_carriage + 1;
    let number = (seat.saturating_sub(1)) % seats_per_carriage + 1;
    format!("{}车{}号", carriage, number)
}

pub async fn tickets_page(
    State(state): State<SharedState>,
    Path(order_id): Path<String>,
) -> Response {
    let (order, train) = match load_order_train(&state, &order_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };

    let seat_numbers: Vec<String> = order
        .seat_numbers
        .iter()
        .map(|&seat| seat_label(seat, &train))
        .collect();

    let mut ctx = Context::new();
    ctx.insert("base_train_id", &train.base_train_id);
    ctx.insert("page_title", "车票打印 - 至冬国铁路");
    ctx.insert("order_id", &order_id);
    ctx.insert("seat_numbers", &seat_numbers);
    let html = state.tera.render("tickets.html", &ctx).unwrap();
    Html(html).into_response()
}

pub async fn ticket_image(
    State(state): State<SharedState>,
    Path((order_id, seat_index)): Path<(String, usize)>,
) -> Response {
    let (order, train) = match load_order_train(&state, &order_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };

    if seat_index >= order.seat_numbers.len() {
        return (StatusCode::NOT_FOUND, "座位索引无效").into_response();
    }

    let seat = order.seat_numbers[seat_index];
    let seat_str = seat_label(seat, &train);
    let img_bytes = generate_ticket_image(&order, &seat_str, &train);
    ([(header::CONTENT_TYPE, "image/png")], img_bytes).into_response()
}
