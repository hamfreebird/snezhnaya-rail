use crate::models::Train;
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// 在指定区间内分配座位，返回分配的座位号列表，若余票不足返回 None
pub fn allocate_seats_for_interval(
    train: &mut Train,
    fi: usize,
    ti: usize,
    carriage_index: u32,
    seat_category: &str,
    seat_count: u32,
) -> Option<Vec<u32>> {
    let seats_per_carriage = train.seats_per_carriage;
    let mut all_seats: Vec<u32> = Vec::new();
    for i in fi..ti {
        all_seats.extend(train.available_seats[i].iter().cloned());
    }
    let in_carriage: Vec<u32> = all_seats
        .into_iter()
        .filter(|&s| (s - 1) / seats_per_carriage + 1 == carriage_index)
        .collect();
    let filtered: Vec<u32> = if seat_category == "corridor" {
        in_carriage.into_iter().filter(|&s| s % 2 == 1).collect()
    } else {
        in_carriage.into_iter().filter(|&s| s % 2 == 0).collect()
    };
    if filtered.len() < seat_count as usize {
        debug!("坐席不足: 需要 {} 张，可用 {} 张", seat_count, filtered.len());
        return None;
    }
    let assigned: Vec<u32> = filtered.into_iter().take(seat_count as usize).collect();
    for i in fi..ti {
        train.available_seats[i].retain(|s| !assigned.contains(s));
    }
    debug!("分配座位 {:?} 给车次 {} 区间 {}-{}", assigned, train.id, fi, ti);
    Some(assigned)
}

/// 释放座位
pub fn release_seats(
    train_id: &str,
    from: &str,
    to: &str,
    seat_numbers: &[u32],
    trains: &mut [Train],
    sold_counts: &mut HashMap<(String, usize), HashSet<u32>>,
) {
    if let Some(train) = trains.iter_mut().find(|t| t.id == train_id) {
        if let Some((fi, ti)) = super::utils::helpers::find_interval(train, from, to) {
            for i in fi..ti {
                for &s in seat_numbers {
                    train.available_seats[i].push(s);
                }
                train.available_seats[i].sort();
                train.available_seats[i].dedup();
                let key = (train_id.to_string(), i);
                if let Some(set) = sold_counts.get_mut(&key) {
                    for &s in seat_numbers {
                        set.remove(&s);
                    }
                }
            }
            tracing::info!("释放座位 {:?} 从车次 {}", seat_numbers, train_id);
        }
    }
}