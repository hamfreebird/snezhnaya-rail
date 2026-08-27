use crate::models::{Train, ArrivalInfo, DepartureInfo, TransferPlan};
use chrono::{Local, NaiveDate, NaiveTime, Datelike};
use std::collections::HashMap;
use tracing::debug;

fn parse_time_handle_24h(s: &str) -> NaiveTime {
    let s = if s.starts_with("24:") {
        format!("00:{}", &s[3..])
    } else {
        s.to_string()
    };
    NaiveTime::parse_from_str(&s, "%H:%M").unwrap()
}

pub fn find_transfers(
    trains: &[Train],
    from: &str,
    to: &str,
    travel_date: &NaiveDate,
    min_transfer: i64,
) -> Vec<TransferPlan> {
    let weekday = travel_date.weekday().number_from_monday() as u8;
    let now = Local::now().naive_local();

    let available: Vec<&Train> = trains
        .iter()
        .filter(|t| t.run_day == weekday && t.is_selling && !t.is_departed)
        .collect();

    let mut arrival_map: HashMap<String, Vec<ArrivalInfo>> = HashMap::new();
    for train in &available {
        if let Some(fi) = train.stations.iter().position(|s| s == from) {
            for i in (fi + 1)..train.stations.len() {
                let x = &train.stations[i];
                let arrival_time = train.arrival_times[i].as_ref().unwrap();
                let price: u32 = train.prices[fi..i].iter().sum();
                let min_seats = train.seats[fi..i].iter().min().cloned().unwrap_or(0);
                if let Ok(arr_dt) = NaiveTime::parse_from_str(arrival_time, "%H:%M") {
                    let dep_datetime = travel_date.and_time(arr_dt);
                    if now >= dep_datetime {
                        continue;
                    }
                }
                arrival_map.entry(x.clone()).or_default().push(ArrivalInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: from.to_string(),
                    to_station: x.clone(),
                    departure_time: train.departure_times[fi].as_ref().unwrap().clone(),
                    arrival_time: arrival_time.clone(),
                    price,
                    min_seats,
                    fi,
                    ti: i,
                });
            }
        }
    }

    let mut departure_map: HashMap<String, Vec<DepartureInfo>> = HashMap::new();
    for train in &available {
        if let Some(ti) = train.stations.iter().position(|s| s == to) {
            for i in 0..ti {
                let x = &train.stations[i];
                let dep_time = train.departure_times[i].as_ref().unwrap();
                let price: u32 = train.prices[i..ti].iter().sum();
                let min_seats = train.seats[i..ti].iter().min().cloned().unwrap_or(0);
                if let Ok(dep_dt) = NaiveTime::parse_from_str(dep_time, "%H:%M") {
                    let dep_datetime = travel_date.and_time(dep_dt);
                    if now >= dep_datetime {
                        continue;
                    }
                }
                departure_map.entry(x.clone()).or_default().push(DepartureInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: x.clone(),
                    to_station: to.to_string(),
                    departure_time: dep_time.clone(),
                    arrival_time: train.arrival_times[ti].as_ref().unwrap().clone(),
                    price,
                    min_seats,
                    fi: i,
                    ti,
                });
            }
        }
    }

    let mut plans = Vec::new();
    for (x, arrivals) in arrival_map {
        if let Some(departures) = departure_map.get(&x) {
            for a in arrivals {
                for d in departures {
                    if a.train_id == d.train_id {
                        continue;
                    }
                    let arr_time = parse_time_handle_24h(&a.arrival_time);
                    let dep_time = parse_time_handle_24h(&d.departure_time);
                    let wait_minutes = (dep_time - arr_time).num_minutes();
                    if wait_minutes < min_transfer {
                        continue;
                    }
                    let first_dep = parse_time_handle_24h(&a.departure_time);
                    let first_arr = parse_time_handle_24h(&a.arrival_time);
                    let first_dur = (first_arr - first_dep).num_minutes();
                    let second_dep = parse_time_handle_24h(&d.departure_time);
                    let second_arr = parse_time_handle_24h(&d.arrival_time);
                    let second_dur = (second_arr - second_dep).num_minutes();
                    let total_duration = first_dur + wait_minutes + second_dur;
                    let total_price = a.price + d.price;
                    let min_seats = a.min_seats.min(d.min_seats);

                    plans.push(TransferPlan {
                        first_train: a.clone(),
                        second_train: d.clone(),
                        transfer_station: x.clone(),
                        transfer_wait: wait_minutes,
                        total_duration,
                        total_price,
                        min_seats,
                    });
                }
            }
        }
    }

    plans.sort_by_key(|p| p.total_duration);
    plans.retain(|p| p.min_seats > 0);
    plans.truncate(50);
    debug!("找到 {} 个换乘方案", plans.len());
    plans
}