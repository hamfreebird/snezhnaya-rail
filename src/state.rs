use crate::db::order_store::SoldCounts;
use crate::models::{Train, Order, Notification};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::SystemTime;

pub struct AppState {
    pub tera: tera::Tera,
    pub trains: RwLock<Vec<Train>>,
    pub orders: RwLock<HashMap<String, Order>>,
    pub sold_counts: RwLock<SoldCounts>,
    pub notifications: RwLock<Vec<Notification>>,
    pub train_mtime: RwLock<SystemTime>,
    pub notif_mtime: RwLock<SystemTime>,
    pub order_index: RwLock<HashMap<String, Vec<String>>>,
}

pub type SharedState = Arc<AppState>;