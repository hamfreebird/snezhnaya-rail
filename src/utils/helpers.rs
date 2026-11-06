use crate::models::{Order, Train};
use rand::Rng;
use std::collections::HashMap;
use std::fs;
use std::time::SystemTime;
use tracing::debug;

pub fn find_interval(train: &Train, from: &str, to: &str) -> Option<(usize, usize)> {
    // 环线车次可能多次停靠同一站名。这里在所有“先经过 from、后到达 to”
    // 的组合中选停站区间最短的一段，避免把整圈绕行当成唯一直达方案。
    let mut best: Option<(usize, usize)> = None;
    for (fi, station) in train.stations.iter().enumerate() {
        if station != from {
            continue;
        }
        for (ti, target) in train.stations.iter().enumerate().skip(fi + 1) {
            if target != to {
                continue;
            }
            if best.map_or(true, |(bf, bt)| ti - fi < bt - bf) {
                best = Some((fi, ti));
            }
        }
    }
    best
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