// src/db/order_store.rs
use crate::models::{Order, OrderStatus, Train};
use chrono::NaiveDate;
use std::collections::HashMap;
use std::fs;
use std::io;
use tracing::{info, error, warn};

/// 已售座位账本：
/// (车次实例 id, 区间下标, 乘车日期) -> (座位号 -> 占用次数)
///
/// 使用占用次数而不是 HashSet，避免同一座位被重复分配时 set.len() 掩盖超卖。
pub type SoldCounts = HashMap<(String, usize, String), HashMap<u32, u32>>;

pub fn date_key(date: &NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

pub fn normalize_date(value: &str) -> String {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|d| date_key(&d))
        .unwrap_or_else(|_| value.to_string())
}

pub fn add_seats(
    sold_counts: &mut SoldCounts,
    train_id: &str,
    interval: usize,
    travel_date: &str,
    seats: &[u32],
) {
    let key = (
        train_id.to_string(),
        interval,
        normalize_date(travel_date),
    );
    let entry = sold_counts.entry(key).or_default();
    for &seat in seats {
        *entry.entry(seat).or_insert(0) += 1;
    }
}

pub fn remove_seats(
    sold_counts: &mut SoldCounts,
    train_id: &str,
    interval: usize,
    travel_date: &str,
    seats: &[u32],
) {
    let key = (
        train_id.to_string(),
        interval,
        normalize_date(travel_date),
    );
    if let Some(entry) = sold_counts.get_mut(&key) {
        for &seat in seats {
            let remove = match entry.get_mut(&seat) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if remove {
                entry.remove(&seat);
            }
        }
        if entry.is_empty() {
            sold_counts.remove(&key);
        }
    }
}

pub fn sold_count_for(
    sold_counts: &SoldCounts,
    train_id: &str,
    interval: usize,
    travel_date: &str,
) -> u32 {
    sold_counts
        .get(&(
            train_id.to_string(),
            interval,
            normalize_date(travel_date),
        ))
        .map(|entry| entry.values().sum())
        .unwrap_or(0)
}

pub fn seat_is_sold(
    sold_counts: &SoldCounts,
    train_id: &str,
    interval: usize,
    travel_date: &str,
    seat: u32,
) -> bool {
    sold_counts
        .get(&(
            train_id.to_string(),
            interval,
            normalize_date(travel_date),
        ))
        .and_then(|entry| entry.get(&seat))
        .copied()
        .unwrap_or(0)
        > 0
}

pub fn load_orders() -> HashMap<String, Order> {
    let path = &crate::config::get().paths.orders_json;
    if let Ok(data) = fs::read_to_string(path) {
        match serde_json::from_str::<HashMap<String, Order>>(&data) {
            Ok(orders) => {
                info!("从 {} 加载了 {} 个订单", path, orders.len());
                orders
            }
            Err(e) => {
                error!("解析 {} 失败: {}", path, e);
                HashMap::new()
            }
        }
    } else {
        HashMap::new()
    }
}

pub fn save_orders(orders: &HashMap<String, Order>) -> io::Result<()> {
    let path = &crate::config::get().paths.orders_json;
    let data = serde_json::to_string_pretty(orders)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let tmp_path = format!("{}.tmp", path);
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, path)?;
    info!("已保存 {} 个订单到 {}", orders.len(), path);
    Ok(())
}

pub fn save_order_index(index: &HashMap<String, Vec<String>>) -> io::Result<()> {
    let path = &crate::config::get().paths.order_index_json;
    let data = serde_json::to_string_pretty(index)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let tmp_path = format!("{}.tmp", path);
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, path)?;
    info!("已保存订单索引，包含 {} 个用户", index.len());
    Ok(())
}

pub fn save_sold_counts(sold_counts: &SoldCounts) -> io::Result<()> {
    let path = &crate::config::get().paths.sold_counts_json;
    let mut serializable: HashMap<String, Vec<u32>> = HashMap::new();
    for ((train_id, idx, date), entry) in sold_counts.iter() {
        let key = format!("{}|{}|{}", train_id, idx, date);
        let mut v: Vec<u32> = Vec::new();
        for (&seat, &count) in entry {
            for _ in 0..count {
                v.push(seat);
            }
        }
        v.sort_unstable();
        serializable.insert(key, v);
    }
    let data = serde_json::to_string_pretty(&serializable)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let tmp_path = format!("{}.tmp", path);
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, path)?;
    info!("已保存 {} 个区间/日期的已售座位", serializable.len());
    Ok(())
}

pub fn load_sold_counts() -> SoldCounts {
    let path = &crate::config::get().paths.sold_counts_json;
    let mut result = SoldCounts::new();
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(serializable) = serde_json::from_str::<HashMap<String, Vec<u32>>>(&data) {
            let mut ignored_legacy = 0usize;
            for (key, v) in serializable {
                let mut parts = key.split('|');
                let train_id = match parts.next() {
                    Some(v) if !v.is_empty() => v,
                    _ => continue,
                };
                let idx = match parts.next().and_then(|s| s.parse::<usize>().ok()) {
                    Some(v) => v,
                    None => continue,
                };
                let date = match parts.next() {
                    Some(v) if NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() => v.to_string(),
                    _ => {
                        // 旧格式没有日期，无法安全分配到具体乘车日；
                        // 启动时会根据 orders.json 中的有效订单重建账本。
                        ignored_legacy += 1;
                        continue;
                    }
                };
                let entry = result
                    .entry((train_id.to_string(), idx, date))
                    .or_default();
                for seat in v {
                    *entry.entry(seat).or_insert(0) += 1;
                }
            }
            if ignored_legacy > 0 {
                warn!(
                    "{} 中有 {} 条旧格式（缺少乘车日期）记录，已根据订单重建座位账本",
                    path, ignored_legacy
                );
            }
            info!("从 {} 加载了 {} 个区间/日期的已售座位", path, result.len());
            return result;
        }
    }
    result
}

/// 以订单为准重建已售座位账本。启动时用于修复旧数据/异常退出后的不一致。
pub fn rebuild_sold_counts_from_orders(
    trains: &[Train],
    orders: &HashMap<String, Order>,
) -> SoldCounts {
    let mut result = SoldCounts::new();
    for order in orders.values() {
        if !matches!(order.status, OrderStatus::Pending | OrderStatus::Paid)
            || order.seat_numbers.is_empty()
        {
            continue;
        }
        let Some(train) = trains.iter().find(|t| t.id == order.train_id) else {
            continue;
        };
        let Some((fi, ti)) = crate::utils::helpers::find_interval(
            train,
            &order.from_station,
            &order.to_station,
        ) else {
            continue;
        };
        for i in fi..ti {
            add_seats(
                &mut result,
                &order.train_id,
                i,
                &order.travel_date,
                &order.seat_numbers,
            );
        }
    }
    result
}

/// 合并两份账本时按“同一座位取最大占用次数”处理，避免重复累加。
pub fn merge_sold_counts(target: &mut SoldCounts, other: SoldCounts) {
    for (key, entry) in other {
        let target_entry = target.entry(key).or_default();
        for (seat, count) in entry {
            target_entry
                .entry(seat)
                .and_modify(|existing| *existing = (*existing).max(count))
                .or_insert(count);
        }
    }
}
