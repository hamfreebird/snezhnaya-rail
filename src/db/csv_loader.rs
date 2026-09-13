// src/db/csv_loader.rs
use crate::models::{Train, Notification};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveTime};
use csv::ReaderBuilder;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use tracing::info;

use crate::utils::helpers::get_mtime;

pub fn load_trains_static() -> Vec<Train> {
    let path = &crate::config::get().paths.trains_csv;
    let business = &crate::config::get().business;

    let file = File::open(path).expect("无法打开 trains.csv 文件");
    let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(file);
    let mut trains = Vec::new();

    for result in rdr.records() {
        let record = result.expect("CSV 记录解析失败");
        let base_id = record[0].to_string();
        let model = record[1].to_string();
        let formation = record[2].to_string();
        let stations: Vec<String> = record[3].split('-').map(|s| s.to_string()).collect();
        let times: Vec<String> = record[4].split('-').map(|s| s.to_string()).collect();
        let prices: Vec<u32> = record[5].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let weights: Vec<u32> = record[6].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let _total_seats_csv = record[7].parse().unwrap_or(0);
        let run_days_str = record[8].to_string();
        let platforms: Vec<String> = record[9].split('-').map(|s| s.to_string()).collect();
        let stop_mins: Vec<u32> = record[10].split('-').map(|s| s.parse().unwrap_or(0)).collect();
        let name = record[11].to_string();

        let carriage_count = formation
            .chars()
            .take_while(|c| c.is_digit(10))
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(1);

        let seats_per_carriage = if model.contains("直特") {
            business.seats_per_carriage_zhite
        } else {
            business.seats_per_carriage_default
        };
        let total_seats_actual = carriage_count * seats_per_carriage;
        let all_seats: Vec<u32> = (1..=total_seats_actual).collect();

        let sum_w: u32 = weights.iter().sum();
        let mut allocated = vec![Vec::new(); weights.len()];
        if sum_w == 0 {
            let base = total_seats_actual / (weights.len() as u32);
            let rem = total_seats_actual % (weights.len() as u32);
            let mut start = 0;
            for i in 0..weights.len() {
                let count = if i < rem as usize { base + 1 } else { base };
                allocated[i] = all_seats[start..start + count as usize].to_vec();
                start += count as usize;
            }
        } else {
            let mut floors = Vec::new();
            let mut fractions = Vec::new();
            for w in &weights {
                let share = (total_seats_actual as f64 * (*w as f64)) / sum_w as f64;
                let floor = share.floor() as u32;
                floors.push(floor);
                fractions.push(share - floor as f64);
            }
            let mut allocated_counts: Vec<u32> = floors.iter().cloned().collect();
            let mut remaining = total_seats_actual - allocated_counts.iter().sum::<u32>();
            if remaining > 0 {
                let mut indices: Vec<usize> = (0..weights.len()).collect();
                indices.sort_by(|&a, &b| fractions[b].partial_cmp(&fractions[a]).unwrap());
                for &i in &indices {
                    if remaining == 0 {
                        break;
                    }
                    allocated_counts[i] += 1;
                    remaining -= 1;
                }
            }
            let mut start = 0;
            for i in 0..weights.len() {
                let count = allocated_counts[i] as usize;
                allocated[i] = all_seats[start..start + count].to_vec();
                start += count;
            }
        }

        let run_days: Vec<u8> = run_days_str
            .split('-')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        for &day in &run_days {
            let id = format!("{}-{}", base_id, day);
            let mut arrival_times = Vec::new();
            let mut departure_times = Vec::new();
            for i in 0..stations.len() {
                if i == 0 {
                    arrival_times.push(None);
                    departure_times.push(Some(times[i].clone()));
                } else if i == stations.len() - 1 {
                    arrival_times.push(Some(times[i].clone()));
                    departure_times.push(None);
                } else {
                    arrival_times.push(Some(times[i].clone()));
                    let dep_time = if let Ok(t) = NaiveTime::parse_from_str(&times[i], "%H:%M") {
                        let stop = stop_mins.get(i).cloned().unwrap_or(0);
                        let dep = t + Duration::minutes(stop as i64);
                        Some(dep.format("%H:%M").to_string())
                    } else {
                        Some(times[i].clone())
                    };
                    departure_times.push(dep_time);
                }
            }

            if stations.len() - 1 != prices.len() {
                panic!(
                    "车次 {} 站点数 {} 票价数 {} 不匹配",
                    base_id,
                    stations.len(),
                    prices.len()
                );
            }
            if stations.len() - 1 != weights.len() {
                panic!(
                    "车次 {} 站点数 {} 权重数 {} 不匹配",
                    base_id,
                    stations.len(),
                    weights.len()
                );
            }
            if stations.len() != platforms.len() {
                panic!(
                    "车次 {} 站点数 {} 站台数 {} 不匹配",
                    base_id,
                    stations.len(),
                    platforms.len()
                );
            }
            if stations.len() != stop_mins.len() {
                panic!(
                    "车次 {} 站点数 {} 停靠时间数 {} 不匹配",
                    base_id,
                    stations.len(),
                    stop_mins.len()
                );
            }

            trains.push(Train {
                id,
                base_train_id: base_id.clone(),
                name: name.clone(),
                model: model.clone(),
                formation: formation.clone(),
                stations: stations.clone(),
                platforms: platforms.clone(),
                arrival_times,
                departure_times,
                prices: prices.clone(),
                weights: weights.clone(),
                total_seats: total_seats_actual,
                carriage_count,
                seats_per_carriage,
                available_seats: allocated.clone(),
                seats: vec![],
                is_departed: false,
                is_selling: false,
                run_day: day,
            });
        }
    }
    info!("从 CSV 加载了 {} 个车次实例", trains.len());
    trains
}

pub fn load_notifications() -> Vec<Notification> {
    let path = &crate::config::get().paths.notifications_txt;
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "".to_string());
    let lines: Vec<&str> = content.lines().collect();
    let mut notifs = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        let title = lines[i].trim().to_string();
        let content_line = lines[i + 1].trim().to_string();
        if !title.is_empty() || !content_line.is_empty() {
            notifs.push(Notification {
                title,
                content: content_line,
            });
        }
        i += 2;
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    info!("从 {} 加载了 {} 条通知", path, notifs.len());
    notifs
}

pub fn compute_interval_seats_with_date(
    train: &Train,
    sold_counts: &HashMap<(String, usize), HashSet<u32>>,
    base_date: &NaiveDate,
) -> Vec<u32> {
    let business = &crate::config::get().business;
    let timeouts = &crate::config::get().timeouts;
    let df = &business.discount_factors;

    let total = train.total_seats;
    let weights = &train.weights;
    let sum_w: u32 = weights.iter().sum();

    let mut allocated = vec![0u32; weights.len()];
    if sum_w == 0 {
        let base = total / (weights.len() as u32);
        let rem = total % (weights.len() as u32);
        for i in 0..weights.len() {
            allocated[i] = base;
            if i < rem as usize {
                allocated[i] += 1;
            }
        }
    } else {
        let mut floors = Vec::with_capacity(weights.len());
        let mut fractions = Vec::with_capacity(weights.len());
        for &w in weights {
            let share = (total as f64 * w as f64) / sum_w as f64;
            let floor = share.floor() as u32;
            floors.push(floor);
            fractions.push(share - floor as f64);
        }
        let allocated_sum: u32 = floors.iter().sum();
        for i in 0..weights.len() {
            allocated[i] = floors[i];
        }
        let mut remaining = total - allocated_sum;
        if remaining > 0 {
            let mut indices: Vec<usize> = (0..weights.len()).collect();
            indices.sort_by(|&a, &b| fractions[b].partial_cmp(&fractions[a]).unwrap());
            for &i in &indices {
                if remaining == 0 {
                    break;
                }
                allocated[i] += 1;
                remaining -= 1;
            }
        }
    }

    let now = Local::now().naive_local();
    let days_diff = (*base_date - now.date()).num_days();
    let first_dep_str = train
        .departure_times
        .first()
        .and_then(|o| o.as_deref())
        .unwrap_or("00:00");

    let factor = if days_diff < 0 {
        0.0
    } else if days_diff == 0 {
        let mut factor = df.today_default;
        if let Ok(first_time) = NaiveTime::parse_from_str(first_dep_str, "%H:%M") {
            let dep_datetime = base_date.and_time(first_time);
            let mins_left = (dep_datetime - now).num_minutes();
            if mins_left <= 0 {
                factor = 0.0;
            } else if mins_left < timeouts.near_departure_window_minutes {
                factor = df.today_default
                    * (mins_left as f64 / timeouts.near_departure_window_minutes as f64);
            }
        }
        factor
    } else if days_diff <= business.presale_days {
        match days_diff {
            1 => df.day_1,
            2 => df.day_2,
            3 => df.day_3,
            4 => df.day_4,
            5 => df.day_5,
            6 => df.day_6,
            _ => df.day_7,
        }
    } else {
        df.future
    };

    let mut seats = Vec::new();
    for i in 0..weights.len() {
        let key = (train.id.clone(), i);
        let sold_count = sold_counts.get(&key).map(|set| set.len()).unwrap_or(0) as u32;
        let max_avail = (allocated[i] as f64 * factor) as u32;
        let avail = if max_avail >= sold_count {
            max_avail - sold_count
        } else {
            0
        };
        seats.push(avail);
    }
    seats
}

pub fn update_train_status_with_date(train: &mut Train, base_date: &NaiveDate) {
    let business = &crate::config::get().business;
    let timeouts = &crate::config::get().timeouts;

    let now = Local::now().naive_local();
    let today = now.date();
    let days_diff = (*base_date - today).num_days();

    if days_diff < 0 {
        train.is_departed = true;
        train.is_selling = false;
        return;
    }
    if days_diff > business.presale_days {
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    let base_weekday = base_date.weekday().number_from_monday() as u8;
    if base_weekday != train.run_day {
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    if days_diff == 0 {
        if let Some(Some(first_time_str)) = train.departure_times.first() {
            if let Ok(first_time) = NaiveTime::parse_from_str(first_time_str, "%H:%M") {
                if now.time() >= first_time {
                    train.is_departed = true;
                    train.is_selling = false;
                    return;
                }
                let diff_secs = (first_time - now.time()).num_seconds();
                if diff_secs < timeouts.stop_selling_before_departure_minutes * 60 {
                    train.is_selling = false;
                    train.is_departed = false;
                    return;
                }
                train.is_selling = true;
                train.is_departed = false;
                return;
            }
        }
        train.is_selling = false;
        train.is_departed = false;
        return;
    }

    train.is_selling = true;
    train.is_departed = false;
}

pub async fn refresh_trains_with_date(state: &crate::state::SharedState, base_date: &NaiveDate) {
    let sold_counts = state.sold_counts.read().await.clone();
    let mut trains = state.trains.write().await;
    for train in trains.iter_mut() {
        train.seats = compute_interval_seats_with_date(train, &sold_counts, base_date);
        update_train_status_with_date(train, base_date);
    }
}

pub async fn reload_all(state: &crate::state::SharedState) {
    let cfg = crate::config::get();
    let sold_counts = state.sold_counts.read().await.clone();

    let new_trains = load_trains_static();
    let new_notifs = load_notifications();
    let now_date = Local::now().date_naive();

    let mut updated_trains = Vec::new();
    for mut train in new_trains {
        for i in 0..train.available_seats.len() {
            let key = (train.id.clone(), i);
            if let Some(sold_set) = sold_counts.get(&key) {
                train.available_seats[i].retain(|s| !sold_set.contains(s));
            }
        }
        train.seats = compute_interval_seats_with_date(&train, &sold_counts, &now_date);
        update_train_status_with_date(&mut train, &now_date);
        updated_trains.push(train);
    }

    *state.trains.write().await = updated_trains;
    *state.notifications.write().await = new_notifs;
    *state.train_mtime.write().await = get_mtime(&cfg.paths.trains_csv);
    *state.notif_mtime.write().await = get_mtime(&cfg.paths.notifications_txt);
    info!("热重载完成，当前车次数: {}", state.trains.read().await.len());
}