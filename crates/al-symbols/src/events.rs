//! Event publisher/subscriber discovery.

/// Information about an event.
#[derive(Debug, Clone)]
pub struct EventInfo {
    pub name: String,
    pub object_name: String,
    pub event_type: EventType,
}

/// Event types in AL.
#[derive(Debug, Clone)]
pub enum EventType {
    Business,
    Integration,
    Internal,
}
