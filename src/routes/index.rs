use axum::{extract::State, response::{Html, IntoResponse, Response}};
use tera::Context;
use serde_json::json;
use std::collections::HashSet;
use chrono::Local;

use crate::state::SharedState;
use crate::db::csv_loader::refresh_trains_with_date;

pub async fn index(State(state): State<SharedState>) -> Response {
    let today = Local::now().date_naive();
    refresh_trains_with_date(&state, &today).await;

    let mut ctx = Context::new();
    ctx.insert("page_title", "至冬国铁路 - 首页");
    ctx.insert("mode", "direct");

    let trains = state.trains.read().await;
    let mut all_stations = HashSet::new();
    for train in trains.iter() {
        for station in &train.stations {
            all_stations.insert(station.clone());
        }
    }
    let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
    stations_vec.sort();
    ctx.insert("stations", &stations_vec);

    let today_str = today.format("%Y-%m-%d").to_string();
    ctx.insert("selected_date", &today_str);

    let display_trains: Vec<serde_json::Value> = trains
        .iter()
        .filter(|t| t.is_selling || !t.is_departed)
        .map(|t| {
            let first = t.stations.first().unwrap();
            let last = t.stations.last().unwrap();
            let dep = t.departure_times.first().and_then(|o| o.as_ref());
            let arr = t.arrival_times.last().and_then(|o| o.as_ref());
            let min_seats = t.seats.iter().min().cloned().unwrap_or(0);
            json!({
                "id": t.id,
                "base_train_id": t.base_train_id,
                "model": t.model,
                "from": first,
                "to": last,
                "departure": dep,
                "arrival": arr,
                "seats": min_seats,
                "is_selling": t.is_selling,
                "is_departed": t.is_departed,
                "run_day": t.run_day,
            })
        })
        .collect();

    // 排序（与原来相同）
    let mut sorted = display_trains;
    sorted.sort_by(|a, b| {
        let a_sell = a["is_selling"].as_bool().unwrap_or(false);
        let b_sell = b["is_selling"].as_bool().unwrap_or(false);
        let a_dep = a["is_departed"].as_bool().unwrap_or(false);
        let b_dep = b["is_departed"].as_bool().unwrap_or(false);
        if a_sell && !b_sell {
            return std::cmp::Ordering::Less;
        }
        if !a_sell && b_sell {
            return std::cmp::Ordering::Greater;
        }
        if a_dep && !b_dep {
            return std::cmp::Ordering::Greater;
        }
        if !a_dep && b_dep {
            return std::cmp::Ordering::Less;
        }
        a["id"].as_str().unwrap().cmp(b["id"].as_str().unwrap())
    });
    ctx.insert("display_trains", &sorted);

    let html = state.tera.render("index.html", &ctx).unwrap();
    Html(html).into_response()
}