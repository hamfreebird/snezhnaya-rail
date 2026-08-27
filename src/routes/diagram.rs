use axum::{
    extract::State,
    response::{Html, IntoResponse, Response},
};
use tera::Context;

use crate::state::SharedState;

pub async fn diagram_page(State(state): State<SharedState>) -> Response {
    let mut ctx = Context::new();
    ctx.insert("page_title", "列车运行图 - 至冬国铁路");
    let html = state.tera.render("diagram.html", &ctx).unwrap();
    Html(html).into_response()
}