//! Request lifetime tracking and the drain gate used for graceful handover.
//!
//! TCP keep-alive connections are deliberately not counted as work: a handover
//! must wait for a streamed response, but must not hang forever because a
//! finished client retained an idle socket.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Default)]
pub struct Drain {
    accepting: Arc<AtomicBool>,
    changed: Arc<Notify>,
}

impl Drain {
    pub fn new() -> Self {
        Self {
            accepting: Arc::new(AtomicBool::new(true)),
            changed: Arc::new(Notify::new()),
        }
    }

    /// Stop admitting new work. Existing requests retain their response body
    /// guards and are allowed to complete normally.
    pub fn begin(&self) {
        self.accepting.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub async fn wait_until_draining(&self) {
        while self.accepting() {
            let notified = self.changed.notified();
            if !self.accepting() {
                return;
            }
            notified.await;
        }
    }

    pub fn accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct Admission {
    permits: Arc<Semaphore>,
}

impl Admission {
    pub fn new(limit: usize) -> Self {
        Self { permits: Arc::new(Semaphore::new(limit)) }
    }

    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }
}

#[derive(Clone)]
pub struct Activity {
    started: Instant,
    /// Seconds since `started` at the last request boundary.
    last: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
    quiet: Arc<Notify>,
}

impl Activity {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            quiet: Arc::new(Notify::new()),
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
        self.in_flight.load(Ordering::Acquire) > 0
    }

    /// Wait for requests already admitted to finish. Unlike idle shutdown this
    /// does not wait for HTTP keep-alive clients to close their TCP sockets.
    pub async fn wait_until_quiet(&self) {
        loop {
            if !self.busy() {
                return;
            }
            let notified = self.quiet.notified();
            if !self.busy() {
                return;
            }
            notified.await;
        }
    }

    /// Wait until nothing has happened for `timeout`, then return. Long
    /// streaming responses count as activity for their whole duration.
    pub async fn wait_until_idle(self, timeout: Duration) {
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
pub async fn track(
    activity: Activity,
    drain: Drain,
    admission: Admission,
    request: Request,
    next: Next,
) -> Response {
    if !drain.accepting() {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(permit) = admission.try_acquire() else {
        return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    activity.in_flight.fetch_add(1, Ordering::AcqRel);
    activity.touch();
    let response = next.run(request).await;
    let guard = Guard { activity, _permit: permit };
    response.map(|body| axum::body::Body::new(GuardedBody { body, _guard: guard }))
}

struct Guard {
    activity: Activity,
    _permit: OwnedSemaphorePermit,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.activity.touch();
        if self.activity.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.activity.quiet.notify_waiters();
        }
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
    fn admission_rejects_work_over_its_limit() {
        let admission = Admission::new(1);
        let first = admission.try_acquire().unwrap();
        assert!(admission.try_acquire().is_none());
        drop(first);
        assert!(admission.try_acquire().is_some());
    }

    #[tokio::test]
    async fn quiet_waits_for_the_last_request() {
        let activity = Activity::new();
        activity.in_flight.fetch_add(1, Ordering::Relaxed);
        let wait = activity.clone();
        let task = tokio::spawn(async move { wait.wait_until_quiet().await });
        tokio::task::yield_now().await;
        let permit = Admission::new(1).try_acquire().unwrap();
        drop(Guard { activity, _permit: permit });
        task.await.unwrap();
    }

    #[test]
    fn drain_stops_new_admissions() {
        let drain = Drain::new();
        assert!(drain.accepting());
        drain.begin();
        assert!(!drain.accepting());
    }
}
