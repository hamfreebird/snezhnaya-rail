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
use std::sync::atomic::{AtomicUsize, Ordering};
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

const DID_YOU_KNOW: &[&str] = &[
    "原神游戏中，至东国所有的铁路均为单向单线。",
    "曙光车站和海屑镇西边的支线原本可以通往焰羽谷，该支线已被废弃，现用作列车坟场。",
    "海屑镇附近的铁路由一群孩子维护，他们被封为铁路大臣。",
    "原神游戏中，在至东国铁路上运行的列车有两种编成，分别是一节机车挂两节客车和一节机车挂一节客车和一节货车。",
    "原神游戏中，离开旅行者的专列后，列车会自动移动到某个站的存车线上。",
    "原神游戏中，列车无法达速通过大枢纽。",
    "原神游戏中，从新基捷城召唤列车去贝洛沃迪港，列车会走一个巨长无比且相当复杂的交路。"
];

// 每次刷新通知页时向后轮换一条；服务重启后从第一条重新开始。
static DID_YOU_KNOW_INDEX: AtomicUsize = AtomicUsize::new(0);

async fn notifications_page(
    State(state): State<SharedState>,
) -> Response {
    let notifs = state.notifications.read().await;
    let fact_index = DID_YOU_KNOW_INDEX.fetch_add(1, Ordering::Relaxed)
        % DID_YOU_KNOW.len();
    let mut ctx = Context::new();
    ctx.insert("page_title", "通知 - 至冬国铁路");
    ctx.insert("notifications", &*notifs);
    ctx.insert("did_you_know", DID_YOU_KNOW[fact_index]);
    let html = state.tera.render("notifications.html", &ctx).unwrap();
    // 禁止缓存，确保每次刷新都能拿到下一条“你知道吗”。
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Html(html),
    )
        .into_response()
}
