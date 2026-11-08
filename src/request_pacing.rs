use std::sync::Mutex;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};

pub(crate) struct RequestGate {
    interval: Duration,
    next_allowed: Mutex<Instant>,
}

impl RequestGate {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_allowed: Mutex::new(Instant::now()),
        }
    }

    pub(crate) async fn wait(&self) {
        loop {
            let target = {
                let mut next = self.next_allowed.lock().unwrap();
                let now = Instant::now();

                if now >= *next {
                    *next = now + self.interval;
                    return;
                }

                *next
            };

            sleep_until(target).await;
        }
    }

    pub(crate) fn pause(&self, duration: Duration) {
        let mut next = self.next_allowed.lock().unwrap();
        *next = (*next).max(Instant::now() + duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test(start_paused = true)]
    async fn spaces_concurrent_requests() {
        let gate = RequestGate::new(Duration::from_secs(2));
        let start = Instant::now();

        tokio::join!(gate.wait(), gate.wait(), gate.wait());

        assert_eq!(start.elapsed(), Duration::from_secs(4));
    }

    #[tokio::test(start_paused = true)]
    async fn pause_delays_later_requests() {
        let gate = RequestGate::new(Duration::from_secs(2));
        let start = Instant::now();

        gate.wait().await;
        gate.pause(Duration::from_secs(30));
        gate.wait().await;

        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }
}
