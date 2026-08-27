use axum::{
    extract::{State, Query},
    response::{Html, IntoResponse, Response},
};
use tera::Context;
use std::collections::HashMap;

use crate::state::SharedState;

pub async fn my_orders_page(State(state): State<SharedState>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "我的订单 - 至冬国铁路");
    ctx.insert("name", "");
    let html = state.tera.render("my_orders.html", &ctx).unwrap();
    Html(html).into_response()
}

pub async fn my_orders_query(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let name = params.get("name").cloned().unwrap_or_default();
    let mut ctx = Context::new();
    ctx.insert("page_title", "我的订单 - 至冬国铁路");
    if !name.is_empty() {
        let index = state.order_index.read().await;
        let order_ids = index.get(&name).cloned().unwrap_or_default();
        ctx.insert("orders", &order_ids);
        ctx.insert("name", &name);
    }
    let html = state.tera.render("my_orders.html", &ctx).unwrap();
    Html(html).into_response()
}