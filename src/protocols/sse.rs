//! SSE framing shared by client protocol adapters.
use serde_json::Value;
pub fn encode(event: &str, value: &Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}
