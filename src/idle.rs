//! Idle shutdown. With socket activation the daemon does not need to sit
//! resident: systemd holds the port, starts it on the first connection, and
//! it can exit once nothing has used it for a while. A turn can stream for
//! many minutes, so in-flight requests hold it open regardless of the timer.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

#[derive(Clone)]
pub struct Activity {
    started: Instant,
    /// Seconds since `started` at the last request boundary.
    last: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
}

impl Activity {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn touch(&self) {
        self.last.store(self.started.elapsed().as_secs(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        let last = self.last.load(Ordering::Relaxed);
        self.started.elapsed().saturating_sub(Duration::from_secs(last))
    }

    pub fn busy(&self) -> bool {
        self.in_flight.load(Ordering::Relaxed) > 0
    }

    /// Wait until nothing has happened for `timeout`, then return. Long
    /// streaming responses count as activity for their whole duration.
    pub async fn wait_until_idle(self, timeout: Duration) {
        // Check often enough to exit promptly without waking constantly.
        let tick = (timeout / 10).clamp(Duration::from_secs(1), Duration::from_secs(30));
        loop {
            tokio::time::sleep(tick).await;
            if !self.busy() && self.idle_for() >= timeout {
                tracing::info!(?timeout, "idle; shutting down");
                return;
            }
        }
    }
}

impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}

/// Counts a request from the moment it arrives until its response body has
/// been dropped, so the clock only runs when the daemon is truly unused.
pub async fn track(activity: Activity, request: Request, next: Next) -> Response {
    activity.in_flight.fetch_add(1, Ordering::Relaxed);
    activity.touch();
    let response = next.run(request).await;
    let guard = Guard { activity };
    // The body may still be streaming, so the guard rides along with it and
    // releases only when the body is finished or dropped.
    response.map(|body| axum::body::Body::new(GuardedBody { body, _guard: guard }))
}

struct Guard {
    activity: Activity,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.activity.touch();
        self.activity.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

pin_project_lite::pin_project! {
    struct GuardedBody<B> {
        #[pin]
        body: B,
        _guard: Guard,
    }
}

impl<B> http_body::Body for GuardedBody<B>
where
    B: http_body::Body,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.project().body.poll_frame(cx)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_grows_from_the_last_touch() {
        let activity = Activity::new();
        assert!(!activity.busy());
        activity.touch();
        assert!(activity.idle_for() < Duration::from_secs(1));
    }

    #[test]
    fn in_flight_requests_keep_it_busy() {
        let activity = Activity::new();
        activity.in_flight.fetch_add(1, Ordering::Relaxed);
        assert!(activity.busy());
        let guard = Guard { activity: activity.clone() };
        drop(guard);
        assert!(!activity.busy(), "dropping the guard releases the request");
    }
}
