use crate::models::Order;
use std::collections::HashMap;
use std::fs;
use tracing::{info, error};

pub fn load_orders() -> HashMap<String, Order> {
    let path = "orders.json";
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

pub fn save_orders(orders: &HashMap<String, Order>) {
    let data = serde_json::to_string_pretty(orders).expect("序列化订单失败");
    fs::write("orders.json", data).expect("写入订单文件失败");
    info!("已保存 {} 个订单到 orders.json", orders.len());
}

pub fn save_order_index(index: &HashMap<String, Vec<String>>) {
    let data = serde_json::to_string_pretty(index).expect("序列化索引失败");
    fs::write("order_index.json", data).expect("写入索引文件失败");
    info!("已保存订单索引，包含 {} 个用户", index.len());
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
        index.entry(name.to_string()).or_insert_with(Vec::new).push(order_id.to_string());
    }
    let index_clone = index.clone();
    drop(index);
    tokio::task::spawn_blocking(move || {
        save_order_index(&index_clone);
    });
}