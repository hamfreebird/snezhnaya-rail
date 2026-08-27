use axum::{
    extract::{State, Path, Query},
    response::{Html, IntoResponse, Response},
    http::StatusCode,
};
use tera::Context;
use serde_json::json;
use std::collections::HashMap;

use crate::state::SharedState;

pub async fn detail_page(
    State(state): State<SharedState>,
    Path(train_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let trains = state.trains.read().await;
    let train = trains.iter().find(|t| t.id == train_id).cloned();
    if let Some(train) = train {
        let from = params.get("from").cloned().unwrap_or_default();
        let to = params.get("to").cloned().unwrap_or_default();

        let mut station_details = Vec::new();
        for i in 0..train.stations.len() {
            station_details.push(json!({
                "station": train.stations[i],
                "platform": train.platforms.get(i).unwrap_or(&"".to_string()),
                "arrival": train.arrival_times[i].as_ref().unwrap_or(&"始发".to_string()),
                "departure": train.departure_times[i].as_ref().unwrap_or(&"终到".to_string()),
            }));
        }

        let mut ctx = Context::new();
        ctx.insert("page_title", &format!("{} 详情", train.name));
        ctx.insert("train", &train);
        ctx.insert("from_highlight", &from);
        ctx.insert("to_highlight", &to);
        ctx.insert("station_details", &station_details);
        let html = state.tera.render("detail.html", &ctx).unwrap();
        Html(html).into_response()
    } else {
        (StatusCode::NOT_FOUND, "车次不存在").into_response()
    }
}