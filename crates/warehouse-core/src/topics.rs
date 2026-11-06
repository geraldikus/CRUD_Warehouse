//! MQTT topic names shared by the API publisher and the desktop subscriber.
//!
//! The broker does not check these strings. This module is the contract.

pub const CREATED: &str = "warehouse/items/created";
pub const UPDATED: &str = "warehouse/items/updated";
pub const DELETED: &str = "warehouse/items/deleted";
pub const EXPIRING: &str = "warehouse/items/expiring";
pub const EXPIRED: &str = "warehouse/items/expired";

/// Wildcard the desktop client and `mqtt_listener` subscribe to.
pub const ALL: &str = "warehouse/items/#";
