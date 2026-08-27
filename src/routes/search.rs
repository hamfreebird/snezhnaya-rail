use axum::{extract::{State, Query}, response::{Html, IntoResponse, Response}};
use tera::Context;
use serde_json::json;
use std::collections::HashSet;
use chrono::{Local, NaiveDate};

use crate::state::SharedState;
use crate::db::csv_loader::refresh_trains_with_date;
use crate::transfer::find_transfers;
use crate::utils::helpers::find_interval;

#[derive(Debug, serde::Deserialize)]
pub struct SearchParams {
    pub from: String,
    pub to: String,
    pub date: Option<String>,
    pub mode: Option<String>,
}

pub async fn search(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Response {
    let date_str = params
        .date
        .clone()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let base_date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap_or(Local::now().date_naive());
    refresh_trains_with_date(&state, &base_date).await;

    let mode = params.mode.clone().unwrap_or_else(|| "direct".to_string());
    let trains = state.trains.read().await;
    let from = params.from.trim();
    let to = params.to.trim();

    if mode == "transfer" {
        let plans = find_transfers(&trains, from, to, &base_date, 8);
        let mut ctx = Context::new();
        ctx.insert("page_title", "中转换乘方案 - 至冬国铁路");
        ctx.insert("plans", &plans);
        ctx.insert("from", from);
        ctx.insert("to", to);
        ctx.insert("selected_date", &date_str);
        let mut all_stations = HashSet::new();
        for train in trains.iter() {
            for station in &train.stations {
                all_stations.insert(station.clone());
            }
        }
        let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
        stations_vec.sort();
        ctx.insert("stations", &stations_vec);
        ctx.insert("mode", "transfer");
        let html = state.tera.render("search_results.html", &ctx).unwrap();
        return Html(html).into_response();
    }

    // 直达
    let mut results = Vec::new();
    for train in trains.iter() {
        if !train.is_selling && !train.is_departed {
            continue;
        }
        if let Some((fi, ti)) = find_interval(train, from, to) {
            let total_price: u32 = train.prices[fi..ti].iter().sum();
            let min_seats = train.seats[fi..ti].iter().min().cloned().unwrap_or(0);
            let departure = &train.departure_times[fi].as_ref();
            let arrival = &train.arrival_times[ti].as_ref();
            results.push(json!({
                "id": train.id,
                "base_train_id": train.base_train_id,
                "model": train.model,
                "from": from,
                "to": to,
                "departure": departure,
                "arrival": arrival,
                "price": total_price,
                "seats": min_seats,
                "is_selling": train.is_selling,
                "is_departed": train.is_departed,
            }));
        }
    }

    results.sort_by(|a, b| {
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

    let mut ctx = Context::new();
    ctx.insert("page_title", "搜索结果 - 至冬国铁路");
    ctx.insert("results", &results);
    ctx.insert("from", from);
    ctx.insert("to", to);
    ctx.insert("selected_date", &date_str);
    ctx.insert("mode", "direct");

    let mut all_stations = HashSet::new();
    for train in trains.iter() {
        for station in &train.stations {
            all_stations.insert(station.clone());
        }
    }
    let mut stations_vec: Vec<String> = all_stations.into_iter().collect();
    stations_vec.sort();
    ctx.insert("stations", &stations_vec);

    let html = state.tera.render("search_results.html", &ctx).unwrap();
    Html(html).into_response()
}