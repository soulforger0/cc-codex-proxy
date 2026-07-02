use crate::error::{ProxyError, Result};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::{pin::Pin, time::Duration};
use tokio::time::Instant;
use tracing::warn;

pub type ProxyByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

#[derive(Debug, Clone, Copy)]
pub struct HttpClientTuning {
    pub connect_timeout_ms: u64,
    pub pool_idle_timeout_ms: u64,
    pub pool_max_idle_per_host: usize,
    pub tcp_keepalive_ms: u64,
}

pub fn build_client(tuning: HttpClientTuning) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(duration_from_millis(tuning.connect_timeout_ms))
        .pool_max_idle_per_host(tuning.pool_max_idle_per_host);

    builder = if tuning.pool_idle_timeout_ms == 0 {
        builder.pool_idle_timeout(None)
    } else {
        builder.pool_idle_timeout(duration_from_millis(tuning.pool_idle_timeout_ms))
    };

    builder = if tuning.tcp_keepalive_ms == 0 {
        builder.tcp_keepalive(None)
    } else {
        builder.tcp_keepalive(duration_from_millis(tuning.tcp_keepalive_ms))
    };

    Ok(builder.build()?)
}

pub fn duration_from_millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

pub fn optional_duration_from_millis(ms: u64) -> Option<Duration> {
    (ms > 0).then(|| Duration::from_millis(ms))
}

pub fn monitor_idle_stream<S>(
    stream: S,
    label: impl Into<String>,
    request_id: Option<String>,
    warn_after: Duration,
    timeout_after: Option<Duration>,
) -> ProxyByteStream
where
    S: Stream<Item = Result<Bytes>> + Send + 'static,
{
    let label = label.into();
    Box::pin(async_stream::stream! {
        futures_util::pin_mut!(stream);
        let warn_enabled = !warn_after.is_zero();
        let mut idle_started = Instant::now();
        let mut next_warn_at = warn_enabled.then(|| idle_started + warn_after);
        let mut timeout_at = timeout_after.map(|timeout| idle_started + timeout);

        loop {
            let Some(deadline) = next_idle_deadline(next_warn_at, timeout_at) else {
                match stream.next().await {
                    Some(item) => yield item,
                    None => break,
                }
                continue;
            };
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            tokio::select! {
                item = stream.next() => {
                    match item {
                        Some(item) => {
                            idle_started = Instant::now();
                            next_warn_at = warn_enabled.then(|| idle_started + warn_after);
                            timeout_at = timeout_after.map(|timeout| idle_started + timeout);
                            yield item;
                        }
                        None => break,
                    }
                }
                _ = &mut sleep => {
                    let now = Instant::now();
                    if timeout_at.is_some_and(|deadline| now >= deadline) {
                        yield Err(ProxyError::Transport(format!(
                            "{label} upstream stream idle timeout after {} ms",
                            timeout_after.unwrap_or_default().as_millis()
                        )));
                        break;
                    }
                    if next_warn_at.is_some_and(|deadline| now >= deadline) {
                        warn!(
                            request_id = request_id.as_deref().unwrap_or("untracked"),
                            label = %label,
                            idle_ms = now.saturating_duration_since(idle_started).as_millis(),
                            "upstream stream idle while waiting for next chunk"
                        );
                        next_warn_at = warn_enabled.then(|| now + warn_after);
                    }
                }
            }
        }
    })
}

fn next_idle_deadline(warn_at: Option<Instant>, timeout_at: Option<Instant>) -> Option<Instant> {
    match (warn_at, timeout_at) {
        (Some(warn_at), Some(timeout_at)) => Some(warn_at.min(timeout_at)),
        (Some(warn_at), None) => Some(warn_at),
        (None, Some(timeout_at)) => Some(timeout_at),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, StreamExt};

    #[tokio::test]
    async fn idle_monitor_does_not_timeout_immediately() {
        let mut stream = monitor_idle_stream(
            stream::pending(),
            "test",
            None,
            Duration::from_millis(50),
            Some(Duration::from_millis(100)),
        );

        let result = tokio::time::timeout(Duration::from_millis(10), stream.next()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn idle_monitor_chunks_reset_timeout() {
        let source =
            stream::once(async { Ok(Bytes::from_static(b"chunk")) }).chain(stream::pending());
        let mut stream = monitor_idle_stream(
            source,
            "test",
            None,
            Duration::from_millis(50),
            Some(Duration::from_millis(80)),
        );

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk, Bytes::from_static(b"chunk"));

        let result = tokio::time::timeout(Duration::from_millis(20), stream.next()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn idle_monitor_yields_transport_error_after_timeout() {
        let mut stream = monitor_idle_stream(
            stream::pending(),
            "test",
            Some("req_test".into()),
            Duration::from_millis(5),
            Some(Duration::from_millis(20)),
        );

        let err = tokio::time::timeout(Duration::from_millis(80), stream.next())
            .await
            .expect("stream should produce timeout")
            .expect("timeout item")
            .unwrap_err();
        assert!(err.to_string().contains("idle timeout"));
    }
}
