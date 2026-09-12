//! Credits precede allocations and stay owned until the resource is released.
use std::sync::{Arc, Mutex};

mod allocator;
pub use allocator::BudgetAllocator;

mod shared;
pub use shared::BudgetArc;

pub mod channel;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Amount {
    pub bytes: usize,
    pub requests: usize,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub current: Amount,
    pub peak: Amount,
    pub admitted: u64,
    pub released: u64,
    pub rejected: u64,
}

#[derive(Debug)]
pub struct Budget {
    limit: Amount,
    usage: Mutex<Usage>,
}

impl Budget {
    pub fn new(limit: Amount) -> Arc<Self> {
        Arc::new(Self {
            limit,
            usage: Mutex::new(Usage::default()),
        })
    }

    pub fn usage(&self) -> Usage {
        *self.usage.lock().expect("budget mutex poisoned")
    }

    pub fn reserve(self: &Arc<Self>, amount: Amount) -> Option<Lease> {
        self.claim(amount).then(|| Lease {
            budget: Arc::clone(self),
            amount,
        })
    }

    fn claim(&self, amount: Amount) -> bool {
        let mut usage = self.usage.lock().expect("budget mutex poisoned");
        if amount.bytes > self.limit.bytes - usage.current.bytes
            || amount.requests > self.limit.requests - usage.current.requests
        {
            usage.rejected = usage.rejected.saturating_add(1);
            return false;
        }
        usage.current.bytes += amount.bytes;
        usage.current.requests += amount.requests;
        usage.peak.bytes = usage.peak.bytes.max(usage.current.bytes);
        usage.peak.requests = usage.peak.requests.max(usage.current.requests);
        usage.admitted = usage.admitted.saturating_add(1);
        true
    }

    fn release(&self, amount: Amount) {
        let mut usage = self.usage.lock().expect("budget mutex poisoned");
        usage.current.bytes -= amount.bytes;
        usage.current.requests -= amount.requests;
        usage.released = usage.released.saturating_add(1);
    }
}

/// Move-only credit: cloning ownership must not manufacture capacity.
#[derive(Debug)]
pub struct Lease {
    budget: Arc<Budget>,
    amount: Amount,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.release(self.amount);
    }
}

/// One image's share of a host-wide pool. A failed image reservation releases
/// its host credit immediately. No path holds two mutexes simultaneously.
#[derive(Debug)]
pub struct Share {
    host: Arc<Budget>,
    image: Arc<Budget>,
}

impl Share {
    pub fn new(host: Arc<Budget>, image_limit: Amount) -> Self {
        Self {
            host,
            image: Budget::new(image_limit),
        }
    }

    pub fn reserve(&self, amount: Amount) -> Option<Credits> {
        Some(Credits {
            _host: self.host.reserve(amount)?,
            _image: self.image.reserve(amount)?,
        })
    }

    pub fn usage(&self) -> Usage {
        self.image.usage()
    }
    pub fn host_usage(&self) -> Usage {
        self.host.usage()
    }
}

#[derive(Debug)]
pub struct Credits {
    _host: Lease,
    _image: Lease,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_exhaustion_rolls_back_host_credit_and_drop_releases_exactly_once() {
        let host = Budget::new(Amount {
            bytes: 128,
            requests: 8,
        });
        let image = Share::new(
            Arc::clone(&host),
            Amount {
                bytes: 64,
                requests: 2,
            },
        );
        let first = image
            .reserve(Amount {
                bytes: 40,
                requests: 1,
            })
            .unwrap();
        assert!(
            image
                .reserve(Amount {
                    bytes: 25,
                    requests: 1
                })
                .is_none()
        );
        assert_eq!(
            host.usage().current,
            Amount {
                bytes: 40,
                requests: 1
            }
        );
        let second = image
            .reserve(Amount {
                bytes: 24,
                requests: 1,
            })
            .unwrap();
        assert!(
            image
                .reserve(Amount {
                    bytes: 0,
                    requests: 1
                })
                .is_none()
        );
        drop(first);
        assert_eq!(
            image.usage().current,
            Amount {
                bytes: 24,
                requests: 1
            }
        );
        drop(second);
        assert_eq!(image.usage().current, Amount::default());
        assert_eq!(host.usage().current, Amount::default());
        assert_eq!(host.usage().admitted, host.usage().released);
    }

    #[test]
    fn concurrent_images_share_one_hard_host_bound() {
        let host = Budget::new(Amount {
            bytes: 4096,
            requests: 8,
        });
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let image = Share::new(
                    Arc::clone(&host),
                    Amount {
                        bytes: 1024,
                        requests: 2,
                    },
                );
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if let Some(credit) = image.reserve(Amount {
                            bytes: 512,
                            requests: 1,
                        }) {
                            std::thread::yield_now();
                            assert!(image.host_usage().current.bytes <= 4096);
                            assert!(image.host_usage().current.requests <= 8);
                            drop(credit);
                        }
                    }
                });
            }
        });
        assert_eq!(host.usage().current, Amount::default());
        assert_eq!(host.usage().admitted, host.usage().released);
    }
}
