use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
}

/// Parse an SSE data line into a CommandOutput-like event.
pub fn parse_sse_event(data: &str) -> Option<StreamEvent> {
    serde_json::from_str(data).ok()
}
