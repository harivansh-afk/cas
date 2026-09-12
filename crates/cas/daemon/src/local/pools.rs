use super::*;

pub(super) const IMAGE_REQUESTS: usize = 128;
pub(super) const IMAGE_CONTROL: usize = 8;

/// Construct once per host; every image Share retains these same budgets.
pub(super) struct HostPools {
    requests: Arc<Budget>,
    append: Arc<Budget>,
    pub read: Arc<Budget>,
    control: Arc<Budget>,
}

impl HostPools {
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({ "requests": self.requests.usage(), "append": self.append.usage(),
            "read": self.read.usage(), "control": self.control.usage() })
    }

    pub fn new() -> Self {
        let budget = |bytes, requests| Budget::new(Amount { bytes, requests });
        Self {
            requests: budget(0, 1024),
            append: budget(64 * MAX_REQUEST_BYTES, 0),
            read: budget(64 * MAX_REQUEST_BYTES, 0),
            control: budget(256 * 1024, 32),
        }
    }

    pub fn image(&self) -> Pools {
        let share =
            |host, bytes, requests| Share::new(Arc::clone(host), Amount { bytes, requests });
        Pools {
            requests: share(&self.requests, 0, IMAGE_REQUESTS),
            append: share(&self.append, 8 * MAX_REQUEST_BYTES, 0),
            read: share(&self.read, 8 * MAX_REQUEST_BYTES, 0),
            control: share(&self.control, 64 * 1024, IMAGE_CONTROL),
        }
    }
}

pub(super) struct Pools {
    pub requests: Share,
    pub append: Share,
    pub read: Share,
    pub control: Share,
}

impl Pools {
    pub fn new() -> Self {
        HostPools::new().image()
    }
}
