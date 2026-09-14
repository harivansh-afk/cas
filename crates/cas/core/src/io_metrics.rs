//! Optional, fixed-size operation timings for one background worker turn.
//! Elapsed time includes descheduling; requested bytes are not device traffic.
use std::{cell::RefCell, marker::PhantomData, rc::Rc, time::Instant};

#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Operation {
    pub calls: u64,
    pub requested_bytes: u64,
    pub elapsed_ns: u64,
}

#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Counters {
    pub read: Operation,
    pub write: Operation,
    pub sync: Operation,
    pub allocate: Operation,
    pub punch: Operation,
    pub unlink: Operation,
    pub buffer_allocate: Operation,
    pub buffer_zero: Operation,
    pub scheduler_wait: Operation,
    pub hash: Operation,
    pub chunk_cache_wait: Operation,
    pub chunk_cache_hold: Operation,
    pub page_cache_wait: Operation,
    pub page_cache_hold: Operation,
    pub fetch_registry_wait: Operation,
}

impl Counters {
    pub fn add(&mut self, other: Self) {
        for (value, delta) in [
            (&mut self.read, other.read),
            (&mut self.write, other.write),
            (&mut self.sync, other.sync),
            (&mut self.allocate, other.allocate),
            (&mut self.punch, other.punch),
            (&mut self.unlink, other.unlink),
            (&mut self.buffer_allocate, other.buffer_allocate),
            (&mut self.buffer_zero, other.buffer_zero),
            (&mut self.scheduler_wait, other.scheduler_wait),
            (&mut self.hash, other.hash),
            (&mut self.chunk_cache_wait, other.chunk_cache_wait),
            (&mut self.chunk_cache_hold, other.chunk_cache_hold),
            (&mut self.page_cache_wait, other.page_cache_wait),
            (&mut self.page_cache_hold, other.page_cache_hold),
            (&mut self.fetch_registry_wait, other.fetch_registry_wait),
        ] {
            value.calls += delta.calls;
            value.requested_bytes += delta.requested_bytes;
            value.elapsed_ns += delta.elapsed_ns;
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Counters>> = const { RefCell::new(None) };
}

/// Never crosses threads and owns no heap allocation. No clock reads outside a scope.
pub struct Scope(PhantomData<Rc<()>>);

impl Scope {
    pub fn enter() -> Self {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            assert!(current.is_none(), "nested IO metrics scope");
            *current = Some(Counters::default());
        });
        Self(PhantomData)
    }

    pub fn finish(self) -> Counters {
        CURRENT.with(|current| {
            current
                .borrow_mut()
                .take()
                .expect("active IO metrics scope")
        })
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = None);
    }
}

pub(crate) struct Measurement {
    started: Option<Instant>,
    bytes: u64,
    field: fn(&mut Counters) -> &mut Operation,
    _thread: PhantomData<Rc<()>>,
}

pub(crate) fn measure(bytes: u64, field: fn(&mut Counters) -> &mut Operation) -> Measurement {
    Measurement {
        started: CURRENT.with(|current| current.borrow().is_some().then(Instant::now)),
        bytes,
        field,
        _thread: PhantomData,
    }
}

impl Drop for Measurement {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            CURRENT.with(|current| {
                if let Some(counters) = current.borrow_mut().as_mut() {
                    let value = (self.field)(counters);
                    value.calls += 1;
                    value.requested_bytes += self.bytes;
                    value.elapsed_ns += elapsed;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_counts_early_returns_isolates_threads_and_clears_on_drop() {
        fn operation() -> Result<(), ()> {
            let _measurement = measure(4096, |c| &mut c.write);
            Err(())
        }
        let scope = Scope::enter();
        assert!(operation().is_err());
        std::thread::spawn(|| {
            assert!(operation().is_err()); // No scope in this thread.
            let other = Scope::enter();
            assert!(operation().is_err());
            assert_eq!(other.finish().write.calls, 1);
        })
        .join()
        .unwrap();
        let counts = scope.finish();
        assert_eq!(
            (counts.write.calls, counts.write.requested_bytes),
            (1, 4096)
        );
        assert_eq!(counts.read.calls, 0);
        assert!(operation().is_err());
        let next = Scope::enter();
        assert_eq!(next.finish().write.calls, 0);
        drop(Scope::enter());
        assert_eq!(Scope::enter().finish().write.calls, 0);
    }
}
