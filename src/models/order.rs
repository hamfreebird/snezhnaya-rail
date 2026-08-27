use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OrderStatus {
    Pending,
    Paid,
    Cancelled,
    Refunded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub train_id: String,
    pub from_station: String,
    pub to_station: String,
    pub from_platform: String,
    pub passenger_name: String,
    pub seat_count: u32,
    pub price: u32,
    pub status: OrderStatus,
    pub expire_at: i64,
    pub created_at: i64,
    pub pay_time: Option<i64>,
    pub travel_date: String,
    pub group_id: Option<String>,
    #[serde(default)]
    pub seat_numbers: Vec<u32>,
    #[serde(default = "default_category")]
    pub seat_category: String,
    #[serde(default)]
    pub carriage_index: u32,
}

pub fn default_category() -> String {
    "corridor".to_string()
}