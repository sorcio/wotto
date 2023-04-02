use std::time::Duration;

use futures::future::join_all;
use leaky_bucket::RateLimiter;

/// Layer multiple `leaky_bucket::RateLimiter`s
pub(crate) struct Throttler {
    limiters: Vec<RateLimiter>,
}

impl Throttler {
    fn new(limiters: Vec<RateLimiter>) -> Self {
        Self { limiters }
    }

    pub(crate) fn make() -> ThrottlerBuilder {
        ThrottlerBuilder::new()
    }

    pub(crate) async fn acquire_one(&self) {
        let futures = self.limiters.iter().map(RateLimiter::acquire_one);
        join_all(futures).await;
    }
}

pub(crate) struct ThrottlerBuilder {
    limiters: Vec<RateLimiter>,
}

impl ThrottlerBuilder {
    pub(crate) fn new() -> Self {
        Self {
            limiters: Vec::with_capacity(3),
        }
    }

    pub(crate) fn layer(mut self, max: usize, refill_every_millis: u64) -> Self {
        let limiter = leaky_bucket::Builder::default()
            .fair(true)
            .initial(0)
            .max(max)
            .refill(1)
            .interval(Duration::from_millis(refill_every_millis))
            .build();
        self.limiters.push(limiter);
        self
    }

    pub(crate) fn build(self) -> Throttler {
        assert!(
            !self.limiters.is_empty(),
            "throttler needs at least one limiter"
        );
        Throttler::new(self.limiters)
    }
}
