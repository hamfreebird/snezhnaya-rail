// src/routes/admin.rs
use axum::{
    extract::{State, Query},
    response::{IntoResponse, Response},
    http::StatusCode,
};
use std::collections::HashMap;
use tracing::{info, warn};

use crate::state::SharedState;
use crate::db::csv_loader::reload_all;

pub async fn admin_reload(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let expected = &crate::config::get().security.admin_token;

    // 若配置中未设置 Token，禁用管理端点
    if expected.is_empty() {
        warn!("管理端点被访问，但未配置 admin_token，已拒绝");
        return (StatusCode::FORBIDDEN, "管理端点未启用").into_response();
    }

    let provided = params.get("token").map(|s| s.as_str()).unwrap_or("");
    if provided != expected {
        warn!("管理端点被访问，Token 不匹配");
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    reload_all(&state).await;
    info!("管理员手动触发热重载");
    (StatusCode::OK, "重载成功").into_response()
}