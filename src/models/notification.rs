use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub title: String,
    pub content: String,
}