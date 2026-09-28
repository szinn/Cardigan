//! A clock the tests move forward so failure backoffs come due.

use std::sync::{Arc, Mutex};

use cg_core::service::Clock;
use chrono::{DateTime, TimeDelta, Utc};

pub(crate) struct SettableClock(Mutex<DateTime<Utc>>);

impl SettableClock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Utc::now())))
    }

    pub(crate) fn advance(&self, by: TimeDelta) {
        *self.0.lock().expect("clock lock") += by;
    }
}

impl Clock for SettableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("clock lock")
    }
}
