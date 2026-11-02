use std::env;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use uuid::Uuid;

use crate::AppState;
use crate::models::Item;

/// How far ahead we warn, and how often the loop wakes up.
///
/// Defaults are a real warehouse: 24 hours, 1 hour, check once a minute.
/// `EXPIRY_DAY_SECS` / `EXPIRY_HOUR_SECS` / `EXPIRY_TICK_SECS` shrink that
/// so a demo does not have to wait a day.
#[derive(Clone, Copy)]
pub struct Thresholds {
    pub day: Duration,
    pub hour: Duration,
    pub tick: Duration,
}

/// The next reminder we have not sent yet.
///
/// `Day` and `Hour` are still in the future. `Expired` means the date has passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryStage {
    Day,
    Hour,
    Expired,
}

impl ExpiryStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Expired => "expired",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Day => 1,
            Self::Hour => 2,
            Self::Expired => 3,
        }
    }

    /// Labels in the MQTT body. Not a live countdown.
    fn hours_left(self) -> Option<i64> {
        match self {
            Self::Day => Some(24),
            Self::Hour => Some(1),
            Self::Expired => None,
        }
    }
}

impl Thresholds {
    pub fn from_env() -> Self {
        Self {
            day: secs_from_env("EXPIRY_DAY_SECS", 24 * 60 * 60),
            hour: secs_from_env("EXPIRY_HOUR_SECS", 60 * 60),
            tick: secs_from_env("EXPIRY_TICK_SECS", 60),
        }
    }
}

fn secs_from_env(name: &str, default: u64) -> Duration {
    let secs = env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default);
    Duration::from_secs(secs)
}

fn rank_notice(current: &str) -> u8 {
    match current {
        "day" => 1,
        "hour" => 2,
        "expired" => 3,
        _ => 0,
    }
}

fn add_window(now: DateTime<Utc>, window: Duration) -> Option<DateTime<Utc>> {
    ChronoDuration::from_std(window)
        .ok()
        .map(|window| now + window)
}

/// Which reminder to send now, if any.
///
/// `current` is the last stage already published (`none`, `day`, `hour`, `expired`).
/// A tighter window wins: an item created 30 minutes before expiry skips "day"
/// and goes straight to "hour".
pub fn next_notice(
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    current: &str,
    day: Duration,
    hour: Duration,
) -> Option<ExpiryStage> {
    let target = if expires_at <= now {
        ExpiryStage::Expired
    } else if add_window(now, hour).is_some_and(|limit| expires_at <= limit) {
        ExpiryStage::Hour
    } else if add_window(now, day).is_some_and(|limit| expires_at <= limit) {
        ExpiryStage::Day
    } else {
        return None;
    };

    if target.rank() > rank_notice(current) {
        Some(target)
    } else {
        None
    }
}

struct WatchedItem {
    id: Uuid,
    name: String,
    quantity: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    expiry_notice: String,
}

impl WatchedItem {
    fn as_item(&self) -> Option<Item> {
        Some(Item {
            id: self.id,
            name: self.name.clone(),
            quantity: self.quantity,
            created_at: self.created_at,
            updated_at: self.updated_at,
            expires_at: Some(self.expires_at?),
        })
    }
}

/// Background task: sleep, look at dates, publish, repeat.
///
/// `tokio::spawn` runs this beside the HTTP server. `sleep` yields the thread,
/// so Add requests are still served while we wait for the next tick.
pub async fn run_loop(state: AppState) {
    let thresholds = Thresholds::from_env();
    tracing::info!(
        day_secs = thresholds.day.as_secs(),
        hour_secs = thresholds.hour.as_secs(),
        tick_secs = thresholds.tick.as_secs(),
        "expiry checker started"
    );

    loop {
        if let Err(err) = check_once(&state, Utc::now(), thresholds).await {
            tracing::warn!(error = %err, "expiry check failed");
        }
        tokio::time::sleep(thresholds.tick).await;
    }
}

async fn check_once(
    state: &AppState,
    now: DateTime<Utc>,
    thresholds: Thresholds,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query_as!(
        WatchedItem,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at, expiry_notice
        FROM items
        WHERE expires_at IS NOT NULL
        "#
    )
    .fetch_all(&state.pool)
    .await?;

    for row in rows {
        let Some(expires_at) = row.expires_at else {
            continue;
        };
        let Some(stage) = next_notice(
            now,
            expires_at,
            &row.expiry_notice,
            thresholds.day,
            thresholds.hour,
        ) else {
            continue;
        };
        let Some(item) = row.as_item() else {
            continue;
        };

        // Only the task that flips the stored stage publishes.
        // A second checker would see the new value and stay quiet.
        let updated = sqlx::query!(
            r#"
            UPDATE items
            SET expiry_notice = $1
            WHERE id = $2 AND expiry_notice = $3
            "#,
            stage.as_str(),
            row.id,
            row.expiry_notice
        )
        .execute(&state.pool)
        .await?;

        if updated.rows_affected() == 0 {
            continue;
        }

        match stage.hours_left() {
            Some(hours_left) => state.mqtt.publish_expiring(&item, hours_left).await,
            None => state.mqtt.publish_expired(&item).await,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 2, hour, minute, 0).unwrap()
    }

    #[test]
    fn ignores_items_that_are_not_close() {
        let now = at(12, 0);
        let expires = now + ChronoDuration::days(3);
        assert_eq!(
            next_notice(
                now,
                expires,
                "none",
                Duration::from_secs(86_400),
                Duration::from_secs(3600)
            ),
            None
        );
    }

    #[test]
    fn warns_one_day_ahead_once() {
        let now = at(12, 0);
        let expires = now + ChronoDuration::hours(20);
        let day = Duration::from_secs(86_400);
        let hour = Duration::from_secs(3600);
        assert_eq!(
            next_notice(now, expires, "none", day, hour),
            Some(ExpiryStage::Day)
        );
        assert_eq!(next_notice(now, expires, "day", day, hour), None);
    }

    #[test]
    fn steps_from_day_to_hour() {
        let now = at(12, 0);
        let expires = now + ChronoDuration::minutes(30);
        assert_eq!(
            next_notice(
                now,
                expires,
                "day",
                Duration::from_secs(86_400),
                Duration::from_secs(3600)
            ),
            Some(ExpiryStage::Hour)
        );
    }

    #[test]
    fn skips_day_when_already_inside_the_hour_window() {
        let now = at(12, 0);
        let expires = now + ChronoDuration::minutes(30);
        assert_eq!(
            next_notice(
                now,
                expires,
                "none",
                Duration::from_secs(86_400),
                Duration::from_secs(3600)
            ),
            Some(ExpiryStage::Hour)
        );
    }

    #[test]
    fn reports_expired_once() {
        let now = at(12, 0);
        let expires = now - ChronoDuration::minutes(1);
        let day = Duration::from_secs(86_400);
        let hour = Duration::from_secs(3600);
        assert_eq!(
            next_notice(now, expires, "hour", day, hour),
            Some(ExpiryStage::Expired)
        );
        assert_eq!(next_notice(now, expires, "expired", day, hour), None);
        assert_eq!(
            next_notice(now, expires, "none", day, hour),
            Some(ExpiryStage::Expired)
        );
    }
}
