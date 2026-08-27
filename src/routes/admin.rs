use axum::{
    extract::State,
    response::{IntoResponse, Response},
    http::StatusCode,
};
use tracing::info;

use crate::state::SharedState;
use crate::db::csv_loader::reload_all;

pub async fn admin_reload(State(state): State<SharedState>) -> Response {
    reload_all(&state).await;
    info!("管理员手动触发热重载");
    (StatusCode::OK, "重载成功").into_response()
}