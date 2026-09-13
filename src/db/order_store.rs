// src/db/order_store.rs
use crate::models::Order;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use tracing::{info, error};

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

pub fn save_sold_counts(
    sold_counts: &HashMap<(String, usize), HashSet<u32>>,
) -> io::Result<()> {
    let path = &crate::config::get().paths.sold_counts_json;
    let mut serializable: HashMap<String, Vec<u32>> = HashMap::new();
    for ((train_id, idx), set) in sold_counts.iter() {
        let key = format!("{}|{}", train_id, idx);
        let mut v: Vec<u32> = set.iter().cloned().collect();
        v.sort();
        serializable.insert(key, v);
    }
    let data = serde_json::to_string_pretty(&serializable)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let tmp_path = format!("{}.tmp", path);
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, path)?;
    info!("已保存 {} 个区间的已售座位", serializable.len());
    Ok(())
}

pub fn load_sold_counts() -> HashMap<(String, usize), HashSet<u32>> {
    let path = &crate::config::get().paths.sold_counts_json;
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(serializable) = serde_json::from_str::<HashMap<String, Vec<u32>>>(&data) {
            let mut result = HashMap::new();
            for (key, v) in serializable {
                if let Some((train_id, idx_str)) = key.split_once('|') {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        result.insert((train_id.to_string(), idx), v.into_iter().collect());
                    }
                }
            }
            info!("从 {} 加载了 {} 个区间的已售座位", path, result.len());
            return result;
        }
    }
    HashMap::new()
}

pub async fn update_order_index_async(
    order_index: &tokio::sync::RwLock<HashMap<String, Vec<String>>>,
    name: &str,
    order_id: &str,
    remove: bool,
) {
    let mut index = order_index.write().await;
    if remove {
        if let Some(list) = index.get_mut(name) {
            list.retain(|id| id != order_id);
            if list.is_empty() {
                index.remove(name);
            }
        }
    } else {
        index
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(order_id.to_string());
    }
    let index_clone = index.clone();
    drop(index);
    tokio::task::spawn_blocking(move || {
        if let Err(e) = save_order_index(&index_clone) {
            error!("保存订单索引失败: {}", e);
        }
    });
}