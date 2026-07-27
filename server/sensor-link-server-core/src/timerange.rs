use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, IntoParams, ToSchema)]
pub struct TimeRange {
    /// Start date-time of the time range in UTC
    pub from: DateTime<Utc>,
    /// End date-time of the time range in UTC
    pub until: DateTime<Utc>,
}

impl TimeRange {
    pub fn duration(&self) -> Duration {
        let diff = self.until.timestamp_millis() - self.from.timestamp_millis();
        Duration::from_millis(diff.unsigned_abs())
    }

    pub fn iter_hours<'a>(&'a self) -> impl Iterator<Item = TimeRange> + 'a {
        let mut from = self.from;
        std::iter::from_fn(move || {
            if from < self.until {
                let until = from + chrono::Duration::hours(1);
                let range = TimeRange {
                    from,
                    until: if until <= self.until {
                        until
                    } else {
                        self.until
                    },
                };
                from = until;
                Some(range)
            } else {
                None
            }
        })
    }
}
