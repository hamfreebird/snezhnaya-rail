use crate::models::{Train, ArrivalInfo, DepartureInfo, TransferPlan};
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use std::collections::HashMap;
use tracing::debug;

/// 把 "HH:MM"（允许 24:xx 表示次日）转换为“从乘车日 00:00 起的分钟数”。
fn parse_minutes(value: &str) -> Option<i64> {
    let mut parts = value.trim().split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    if !(0..=24).contains(&hour) || minute >= 60 {
        return None;
    }
    Some(hour * 60 + minute)
}

/// 计算一趟车各站的绝对分钟偏移。遇到时间回退（跨零点或 24:xx 后续站）自动加一天，
/// 从而保证同一趟车上的区间时长不会变成负数。
fn train_time_offsets(train: &Train) -> Vec<(Option<i64>, Option<i64>)> {
    let mut result = Vec::with_capacity(train.stations.len());
    let mut last = 0i64;

    for i in 0..train.stations.len() {
        let mut arrival = train.arrival_times[i]
            .as_deref()
            .and_then(parse_minutes);
        if let Some(value) = arrival.as_mut() {
            while *value < last {
                *value += 24 * 60;
            }
            last = *value;
        }

        let mut departure = train.departure_times[i]
            .as_deref()
            .and_then(parse_minutes);
        if let Some(value) = departure.as_mut() {
            while *value < last {
                *value += 24 * 60;
            }
            last = *value;
        }

        result.push((arrival, departure));
    }

    result
}

fn offset_datetime(date: &NaiveDate, offset: i64) -> NaiveDateTime {
    date.and_hms_opt(0, 0, 0)
        .expect("合法日期")
        + chrono::Duration::minutes(offset)
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

    let offsets: HashMap<String, Vec<(Option<i64>, Option<i64>)>> = available
        .iter()
        .map(|train| (train.id.clone(), train_time_offsets(train)))
        .collect();

    let mut arrival_map: HashMap<String, Vec<ArrivalInfo>> = HashMap::new();
    for train in &available {
        let time_offsets = &offsets[&train.id];
        // 环线车次可能多次经过同一站名，必须按站序中的所有出现位置枚举，
        // 否则只会取到第一次（对以该站为终点的环线而言就是 0 号位）。
        for (fi, station) in train.stations.iter().enumerate() {
            if station != from {
                continue;
            }
            let Some(departure_time) = train.departure_times[fi].as_ref() else {
                continue;
            };
            let Some(dep_offset) = time_offsets[fi].1 else {
                continue;
            };
            if now >= offset_datetime(travel_date, dep_offset) {
                continue;
            }

            for i in (fi + 1)..train.stations.len() {
                let x = &train.stations[i];
                // 同站上下车不构成出行，跳过环线绕回起点的情况。
                if x == from {
                    continue;
                }
                let Some(arrival_time) = train.arrival_times[i].as_ref() else {
                    continue;
                };
                if time_offsets[i].0.is_none() {
                    continue;
                }

                let price: u32 = train.prices[fi..i].iter().sum();
                let min_seats = train.seats[fi..i].iter().min().cloned().unwrap_or(0);
                arrival_map.entry(x.clone()).or_default().push(ArrivalInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: from.to_string(),
                    to_station: x.clone(),
                    departure_time: departure_time.clone(),
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
        let time_offsets = &offsets[&train.id];
        // 同理，终点站名可能在环线中出现多次，逐一作为下车位置处理。
        for (ti, station) in train.stations.iter().enumerate() {
            if station != to {
                continue;
            }
            let Some(arrival_time) = train.arrival_times[ti].as_ref() else {
                continue;
            };
            if time_offsets[ti].0.is_none() {
                continue;
            }

            for i in 0..ti {
                let x = &train.stations[i];
                // 同站上下车不构成出行，跳过环线绕回终点的情况。
                if x == to {
                    continue;
                }
                let Some(dep_time) = train.departure_times[i].as_ref() else {
                    continue;
                };
                let Some(dep_offset) = time_offsets[i].1 else {
                    continue;
                };
                if now >= offset_datetime(travel_date, dep_offset) {
                    continue;
                }

                let price: u32 = train.prices[i..ti].iter().sum();
                let min_seats = train.seats[i..ti].iter().min().cloned().unwrap_or(0);
                departure_map.entry(x.clone()).or_default().push(DepartureInfo {
                    train_id: train.id.clone(),
                    base_train_id: train.base_train_id.clone(),
                    from_station: x.clone(),
                    to_station: to.to_string(),
                    departure_time: dep_time.clone(),
                    arrival_time: arrival_time.clone(),
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

                    let a_offsets = &offsets[&a.train_id];
                    let d_offsets = &offsets[&d.train_id];
                    let (Some(first_dep), Some(first_arr)) =
                        (a_offsets[a.fi].1, a_offsets[a.ti].0)
                    else {
                        continue;
                    };
                    let (Some(second_dep), Some(second_arr)) =
                        (d_offsets[d.fi].1, d_offsets[d.ti].0)
                    else {
                        continue;
                    };

                    // 换乘发生在同一次出行中：若第二段发车早于第一段到达，
                    // 视为次日发车（必要时继续顺延），而不是丢弃该方案。
                    let mut wait_minutes = second_dep - first_arr;
                    while wait_minutes < 0 {
                        wait_minutes += 24 * 60;
                    }
                    if wait_minutes < min_transfer {
                        continue;
                    }

                    let first_dur = first_arr - first_dep;
                    let second_dur = second_arr - second_dep;
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
    let max_plans = crate::config::get().business.max_transfer_plans;
    plans.truncate(max_plans);
    debug!("找到 {} 个换乘方案", plans.len());
    plans
}
