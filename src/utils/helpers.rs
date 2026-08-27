use crate::models::{Order, Train};
use rand::Rng;
use std::collections::HashMap;
use std::fs;
use std::time::SystemTime;
use tracing::debug;

pub fn find_interval(train: &Train, from: &str, to: &str) -> Option<(usize, usize)> {
    let from_indices: Vec<usize> = train
        .stations
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == from)
        .map(|(i, _)| i)
        .collect();
    let to_indices: Vec<usize> = train
        .stations
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == to)
        .map(|(i, _)| i)
        .collect();
    for &fi in &from_indices {
        for &ti in &to_indices {
            if fi < ti {
                return Some((fi, ti));
            }
        }
    }
    None
}

pub fn generate_unique_order_id(orders: &HashMap<String, Order>) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let id = format!("{:08}", rng.gen_range(0..100000000));
        if !orders.contains_key(&id) {
            debug!("生成订单号: {}", id);
            return id;
        }
    }
}

pub fn get_mtime(path: &str) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}