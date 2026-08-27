mod index;
mod search;
mod order;
mod payment;
mod exchange;
mod tickets;
mod detail;
mod diagram;
mod admin;
mod my_orders;
mod transfer_confirm;

pub use admin::admin_reload;
pub use detail::detail_page;
pub use diagram::diagram_page;
pub use exchange::{
    exchange_page, exchange_query, exchange_refund,
    exchange_change, exchange_available,
};
pub use index::index;
pub use my_orders::{my_orders_page, my_orders_query};
pub use order::{
    order_form, create_order, order_transfer,
    order_transfer_form, order_success_page
};
pub use payment::{payment_page, pay_order};
pub use search::search;
pub use tickets::{tickets_page, ticket_image};
pub use transfer_confirm::transfer_confirm;

use axum::{Router, extract::State, response::{Html, IntoResponse, Response}};
use std::collections::HashMap;
use tera::Context;
use crate::state::SharedState;

pub fn create_routes(state: SharedState) -> Router {
    Router::new()
        .route("/", axum::routing::get(index))
        .route("/search", axum::routing::get(search))
        .route("/order/create", axum::routing::post(create_order))
        .route("/order/{train_id}", axum::routing::get(order_form))
        .route("/order/transfer", axum::routing::post(order_transfer))
        .route("/order/transfer_form", axum::routing::get(order_transfer_form))
        .route("/order_success", axum::routing::get(order_success_page))
        .route("/orders", axum::routing::get(orders_query))
        .route("/detail/{train_id}", axum::routing::get(detail_page))
        .route("/payment/{order_id}/pay", axum::routing::get(pay_order))
        .route("/payment/{order_id}", axum::routing::get(payment_page))
        .route("/notifications", axum::routing::get(notifications_page))
        .route("/exchange", axum::routing::get(exchange_page))
        .route("/exchange/query", axum::routing::get(exchange_query))
        .route("/exchange/refund", axum::routing::post(exchange_refund))
        .route("/exchange/change", axum::routing::post(exchange_change))
        .route("/transfer_confirm", axum::routing::get(transfer_confirm))
        .route("/my_orders", axum::routing::get(my_orders_page))
        .route("/my_orders/query", axum::routing::get(my_orders_query))
        .route("/tickets/{order_id}", axum::routing::get(tickets_page))
        .route("/ticket/{order_id}/{seat_index}", axum::routing::get(ticket_image))
        .route("/exchange/available", axum::routing::get(exchange_available))
        .route("/diagram", axum::routing::get(diagram_page))
        .route("/admin/reload", axum::routing::get(admin_reload))
        .with_state(state)
}

async fn orders_query(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    if let Some(order_id) = params.get("order_id") {
        let orders = state.orders.read().await;
        let order = orders.get(order_id).cloned();
        let mut ctx = Context::new();
        ctx.insert("page_title", "订单查询结果 - 至冬国铁路");
        if let Some(order) = order {
            ctx.insert("order", &order);
        }
        let html = state.tera.render("order_result.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        let mut ctx = Context::new();
        ctx.insert("page_title", "订单查询 - 至冬国铁路");
        let html = state.tera.render("order_query.html", &ctx).unwrap();
        Html(html).into_response()
    }
}

async fn notifications_page(
    State(state): State<SharedState>,
) -> Response {
    let notifs = state.notifications.read().await;
    let mut ctx = Context::new();
    ctx.insert("page_title", "通知 - 至冬国铁路");
    ctx.insert("notifications", &*notifs);
    let html = state.tera.render("notifications.html", &ctx).unwrap();
    Html(html).into_response()
}