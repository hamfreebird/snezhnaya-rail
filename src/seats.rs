use crate::db::order_store::{date_key, remove_seats, seat_is_sold, SoldCounts};
use crate::models::Train;
use chrono::NaiveDate;
use serde::Serialize;
use std::collections::HashSet;
use tracing::debug;

/// 某车厢在靠走廊/靠窗两类坐席下的可售数量。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CarriageAvailability {
    pub index: u32,
    pub corridor: u32,
    pub window: u32,
}

fn free_seat_intersection(
    train: &Train,
    sold_counts: &SoldCounts,
    travel_date: &str,
    fi: usize,
    ti: usize,
) -> HashSet<u32> {
    let mut candidates: Option<HashSet<u32>> = None;

    for i in fi..ti {
        let free: HashSet<u32> = train.available_seats[i]
            .iter()
            .copied()
            .filter(|&seat| !seat_is_sold(sold_counts, &train.id, i, travel_date, seat))
            .collect();
        candidates = Some(match candidates {
            Some(current) => current.intersection(&free).copied().collect(),
            None => free,
        });
    }

    candidates.unwrap_or_default()
}

/// 计算下单页面各车厢/坐席类别的可售数量。
///
/// 结果同时受两个条件约束：
/// 1. 所有途经区间的物理空闲座位交集；
/// 2. 各区间按权重和日期折扣计算出的剩余容量。
pub fn carriage_availability(
    train: &Train,
    sold_counts: &SoldCounts,
    base_date: &NaiveDate,
    fi: usize,
    ti: usize,
) -> Vec<CarriageAvailability> {
    let mut result: Vec<CarriageAvailability> = (1..=train.carriage_count)
        .map(|index| CarriageAvailability {
            index,
            corridor: 0,
            window: 0,
        })
        .collect();

    if fi >= ti || ti > train.available_seats.len() {
        return result;
    }

    let available =
        crate::db::csv_loader::compute_interval_seats_with_date(train, sold_counts, base_date);
    let capacity = available
        .get(fi..ti)
        .and_then(|slice| slice.iter().min().copied())
        .unwrap_or(0);
    if capacity == 0 {
        return result;
    }

    let travel_date = date_key(base_date);
    let seats_per_carriage = train.seats_per_carriage.max(1);
    for seat in free_seat_intersection(train, sold_counts, &travel_date, fi, ti) {
        let carriage = seat.saturating_sub(1) / seats_per_carriage + 1;
        if let Some(entry) = result.get_mut(carriage.saturating_sub(1) as usize) {
            if seat % 2 == 1 {
                entry.corridor += 1;
            } else {
                entry.window += 1;
            }
        }
    }

    for entry in &mut result {
        entry.corridor = entry.corridor.min(capacity);
        entry.window = entry.window.min(capacity);
    }
    result
}

/// 在指定区间内分配座位，返回分配的座位号列表，若余票不足返回 None。
///
/// 座位号是全局物理座位号：跨区间行程只在所有区间都空闲的座位（交集）中取号。
/// 每个区间的可售数量仍受权重容量与日期折扣系数约束，与页面展示的余票使用同一口径。
pub fn allocate_seats_for_interval(
    train: &Train,
    sold_counts: &SoldCounts,
    base_date: &NaiveDate,
    fi: usize,
    ti: usize,
    carriage_index: u32,
    seat_category: &str,
    seat_count: u32,
) -> Option<Vec<u32>> {
    if seat_count == 0 || fi >= ti || ti > train.available_seats.len() {
        debug!("非法座位分配请求: count={}, 区间={}-{}", seat_count, fi, ti);
        return None;
    }

    // 使用与展示完全相同的容量/日期口径，避免“看得到余票却买不到”或反向超卖。
    let available = crate::db::csv_loader::compute_interval_seats_with_date(
        train,
        sold_counts,
        base_date,
    );
    if available
        .get(fi..ti)
        .map(|slice| slice.iter().any(|&n| n < seat_count))
        .unwrap_or(true)
    {
        debug!(
            "坐席不足: 车次 {} 区间 {}-{} 需要 {} 张，余票 {:?}",
            train.id,
            fi,
            ti,
            seat_count,
            available.get(fi..ti)
        );
        return None;
    }

    let travel_date = date_key(base_date);
    let seats_per_carriage = train.seats_per_carriage;
    let mut filtered: Vec<u32> = free_seat_intersection(train, sold_counts, &travel_date, fi, ti)
        .into_iter()
        .filter(|&s| (s - 1) / seats_per_carriage + 1 == carriage_index)
        .filter(|&s| {
            if seat_category == "corridor" {
                s % 2 == 1
            } else {
                s % 2 == 0
            }
        })
        .collect();
    filtered.sort_unstable();

    if filtered.len() < seat_count as usize {
        debug!(
            "坐席不足: 车次 {} 区间 {}-{} 车厢 {} 需要 {} 张，可用 {} 张",
            train.id,
            fi,
            ti,
            carriage_index,
            seat_count,
            filtered.len()
        );
        return None;
    }

    let assigned: Vec<u32> = filtered.into_iter().take(seat_count as usize).collect();
    debug!(
        "分配座位 {:?} 给车次 {} 区间 {}-{} 日期 {}",
        assigned, train.id, fi, ti, travel_date
    );
    Some(assigned)
}

/// 释放订单占用的座位。只归还该订单实际经过的区间，绝不向其他区间泄漏座位号。
pub fn release_seats(
    train_id: &str,
    travel_date: &str,
    from: &str,
    to: &str,
    seat_numbers: &[u32],
    trains: &[Train],
    sold_counts: &mut SoldCounts,
) {
    if seat_numbers.is_empty() {
        return;
    }
    if let Some(train) = trains.iter().find(|t| t.id == train_id) {
        if let Some((fi, ti)) = super::utils::helpers::find_interval(train, from, to) {
            for i in fi..ti {
                remove_seats(sold_counts, train_id, i, travel_date, seat_numbers);
            }
            tracing::info!(
                "释放座位 {:?} 从车次 {} 日期 {} 区间 {}-{}",
                seat_numbers,
                train_id,
                travel_date,
                fi,
                ti
            );
            return;
        }
    }
    tracing::warn!(
        "释放座位失败：找不到车次 {} 的区间 {} -> {}",
        train_id,
        from,
        to
    );
}

/// 删除旧账本中某个区间的座位记录（用于回滚“尚未写入订单”的临时分配）。
pub fn remove_interval_seats(
    train_id: &str,
    travel_date: &str,
    fi: usize,
    ti: usize,
    seat_numbers: &[u32],
    sold_counts: &mut SoldCounts,
) {
    for i in fi..ti {
        remove_seats(sold_counts, train_id, i, travel_date, seat_numbers);
    }
}
