//! Per-IP rate limiting on the write path.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use governor::clock::DefaultClock;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};

use crate::error::AppError;
use crate::state::AppState;

pub type IpLimiter = RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>;

pub fn build(rps: u32, burst: u32) -> Option<Arc<IpLimiter>> {
    let rps = NonZeroU32::new(rps)?;
    let burst = NonZeroU32::new(burst.max(rps.get())).unwrap();
    Some(Arc::new(RateLimiter::keyed(
        Quota::per_second(rps).allow_burst(burst),
    )))
}

/// Periodically discards idle keys.
///
/// The keyed limiter's map grows with every distinct source address, so without
/// this the rate limiter would itself become the memory leak it exists to
/// prevent.
pub fn spawn_janitor(limiter: Arc<IpLimiter>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            limiter.retain_recent();
            let len = limiter.len();
            if len > 100_000 {
                // Pathological key churn (spoofed XFF, botnet). Reset rather
                // than let the map grow without bound.
                tracing::warn!(keys = len, "rate limiter key set too large, clearing");
                limiter.shrink_to_fit();
            }
        }
    });
}

/// Resolves the client address.
///
/// `X-Forwarded-For` is honoured only when `LOGGER_TRUST_PROXY` is set: behind a
/// proxy the peer address is the proxy, but an untrusted XFF header makes the
/// limiter trivially bypassable by spoofing a fresh IP per request.
fn client_ip(state: &AppState, req: &Request) -> IpAddr {
    if state.cfg.trust_proxy {
        if let Some(xff) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(first) = xff.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

pub async fn limit(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let Some(limiter) = state.limiter.as_ref() else {
        return Ok(next.run(req).await);
    };

    let ip = client_ip(&state, &req);
    if limiter.check_key(&ip).is_err() {
        state
            .metrics
            .rate_limited
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(AppError::RateLimited);
    }
    Ok(next.run(req).await)
}
