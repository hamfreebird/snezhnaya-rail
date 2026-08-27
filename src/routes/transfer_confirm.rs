use axum::{
    extract::{State, Query},
    response::{Html, IntoResponse, Response},
    http::StatusCode,
};
use tera::Context;
use std::collections::HashMap;

use crate::state::SharedState;

pub async fn transfer_confirm(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let group_id = params.get("group_id").cloned().unwrap_or_default();
    if group_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 group_id").into_response();
    }

    let orders = state.orders.read().await;
    let group_orders: Vec<&crate::models::Order> = orders
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