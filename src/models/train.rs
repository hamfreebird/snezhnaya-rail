use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Train {
    pub id: String,
    pub base_train_id: String,
    pub name: String,
    pub model: String,
    pub formation: String,
    pub stations: Vec<String>,
    pub platforms: Vec<String>,
    pub arrival_times: Vec<Option<String>>,
    pub departure_times: Vec<Option<String>>,
    pub prices: Vec<u32>,
    pub weights: Vec<u32>,
    pub total_seats: u32,
    pub carriage_count: u32,
    pub seats_per_carriage: u32,
    pub available_seats: Vec<Vec<u32>>,
    pub seats: Vec<u32>,
    pub is_departed: bool,
    pub is_selling: bool,
    pub run_day: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArrivalInfo {
    pub train_id: String,
    pub base_train_id: String,
    pub from_station: String,
    pub to_station: String,
    pub departure_time: String,
    pub arrival_time: String,
    pub price: u32,
    pub min_seats: u32,
    pub fi: usize,
    pub ti: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DepartureInfo {
    pub train_id: String,
    pub base_train_id: String,
    pub from_station: String,
    pub to_station: String,
    pub departure_time: String,
    pub arrival_time: String,
    pub price: u32,
    pub min_seats: u32,
    pub fi: usize,
    pub ti: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferPlan {
    pub first_train: ArrivalInfo,
    pub second_train: DepartureInfo,
    pub transfer_station: String,
    pub transfer_wait: i64,
    pub total_duration: i64,
    pub total_price: u32,
    pub min_seats: u32,
}