//! Retrying transient failures when fetching from a registry.
//!
//! The containers-image-proxy (skopeo) does not retry failed requests
//! itself; in podman the retry loop lives in the caller (`c/common`), and
//! the same holds for us.  Registries such as quay.io intermittently return
//! 5xx errors or drop connections, so without retries a single blip fails
//! an entire pull.
//!
//! Retries are done at the granularity of one proxy operation (opening the
//! image, fetching the manifest, or fetching and importing one blob), so a
//! failed layer does not force refetching the others.  Every blob attempt
//! starts from a fresh proxy request: data from a failed attempt is never
//! reused, and a layer is only registered once the proxy has verified the
//! size and digest of the complete blob.

use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use anyhow::{Context, Result};
use containers_image_proxy::{Error as ProxyError, GetBlobError};

use crate::progress::{ProgressEvent, ProgressReporter};

/// Default number of retries, matching podman's default in `containers.conf`.
const DEFAULT_MAX_RETRIES: u32 = 3;
/// Default delay before the first retry; it doubles for each further retry.
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_secs(1);
/// Default upper bound on the delay between two attempts.
const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(30);
/// Up to this fraction of each delay is randomly subtracted, so that
/// concurrent layer fetches failing together do not retry in lockstep.
const JITTER_FRACTION: f64 = 0.2;

/// Lowercased substrings of proxy error messages that indicate a transient
/// network or registry failure worth retrying.
///
/// Errors from the proxy's `GetBlob` (which, unlike `GetRawBlob`, verifies
/// the digest for us) and other methods only reach us as the text of a Go
/// error, so this is necessarily a heuristic.  It is modelled on
/// `IsErrorRetryable()` in containers/common `pkg/retry`.  Truncated or
/// corrupted blob transfers are matched separately by [`is_truncated_blob`].
const TRANSIENT_ERROR_PATTERNS: &[&str] = &[
    // HTTP status errors from containers/image (`UnexpectedHTTPStatusError`)
    "received unexpected http status: 500",
    "received unexpected http status: 502",
    "received unexpected http status: 503",
    "received unexpected http status: 504",
    // Rate limiting: HTTP 429 as reported by containers/image, which older
    // versions format as "StatusCode: 429, <body>", and the registry error
    // code for it
    "too many requests",
    "statuscode: 429",
    "toomanyrequests",
    // Network-level failures
    "connection reset by peer",
    "connection refused",
    "connection timed out",
    "i/o timeout",
    "tls handshake timeout",
    "client.timeout exceeded",
    "timeout awaiting response headers",
    "network is unreachable",
    "network is down",
    "no route to host",
    "host is down",
    "software caused connection abort",
    "temporary failure in name resolution",
    "server misbehaving",
    "http2: server sent goaway",
    "http2: client connection lost",
    "stream error: stream id",
    "server closed idle connection",
    "unexpected eof",
];

/// How to retry transient failures while fetching an image from a registry.
///
/// Only errors that look transient (network failures, HTTP 5xx and 429
/// responses, truncated or corrupted blob transfers) are retried; others
/// such as authentication failures or a missing image fail immediately.
///
/// The delay before retry `n` (counting from zero) is
/// `initial_delay * 2^n`, capped at `max_delay`, minus a random jitter of up
/// to 20%.
///
/// Start from [`RetryPolicy::default()`] or [`RetryPolicy::none()`] and
/// adjust the fields as needed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Maximum number of retries after the initial attempt; zero disables
    /// retrying.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub initial_delay: Duration,
    /// Upper bound on the delay between two attempts.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub const fn none() -> Self {
        Self {
            max_retries: 0,
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    /// The default policy with a different number of retries.
    pub fn with_max_retries(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Self::default()
        }
    }

    /// The delay before retry number `retry` (starting at zero), where
    /// `jitter` in `[0, 1)` selects how much of [`JITTER_FRACTION`] to take
    /// off.
    fn backoff_delay(&self, retry: u32, jitter: f64) -> Duration {
        let exponential = self
            .initial_delay
            .saturating_mul(2u32.saturating_pow(retry))
            .min(self.max_delay);
        // Scaling by at most JITTER_FRACTION cannot overflow, unlike scaling
        // a huge `max_delay` by a factor close to 1.
        exponential.saturating_sub(exponential.mul_f64(JITTER_FRACTION * jitter.clamp(0.0, 1.0)))
    }
}

/// A uniformly distributed value in `[0, 1)`.
///
/// Jitter only needs to decorrelate concurrent retries, not be
/// cryptographically random, and the std hasher's random keys provide that
/// without a dependency on `rand`.
fn random_unit() -> f64 {
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    // Use the top 53 bits, the precision of an f64 mantissa.
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// The proxy's `GetBlob` reports a short read as
/// `expected N bytes in blob, got M` and a digest mismatch as
/// `corrupted blob, expecting <digest>`.
fn is_truncated_blob(msg: &str) -> bool {
    (msg.contains("expected ") && msg.contains(" bytes in blob, got "))
        || msg.contains("corrupted blob, expecting ")
}

/// Whether a (Go) error message from the proxy describes a transient failure.
fn is_transient_message(msg: &str) -> bool {
    let msg = msg.to_lowercase();
    is_truncated_blob(&msg)
        || TRANSIENT_ERROR_PATTERNS.iter().any(|p| msg.contains(p))
        // A connection closed without a response, e.g. `Get "https://…": EOF`
        || msg.trim_end().ends_with(": eof")
}

/// A blob fetch where both the image proxy and the local import failed.
///
/// The proxy's error comes first: a broken transfer is usually what made
/// the import fail too, and it decides whether to retry.  It is not exposed
/// as [`std::error::Error::source()`], since the message already includes
/// it.
#[derive(Debug, thiserror::Error)]
#[error("{proxy}; the import also failed: {import:#}")]
pub(crate) struct ProxyAndImportError {
    pub(crate) proxy: ProxyError,
    pub(crate) import: anyhow::Error,
}

fn is_transient_proxy_error(err: &ProxyError) -> bool {
    match err {
        // The proxy classifies `GetRawBlob` errors itself; trust that.
        ProxyError::BlobError(GetBlobError::Retryable(_)) => true,
        ProxyError::RequestInitiationFailure { error, .. } => is_transient_message(error),
        ProxyError::RequestReturned(msg) => is_transient_message(msg),
        // Local failures, such as the proxy process having exited
        _ => false,
    }
}

/// Whether `err` looks like a transient failure that is worth retrying.
///
/// Only errors reported by the image proxy are considered; local failures,
/// such as a malformed layer that the proxy verified as matching its digest,
/// are never retried.
pub(crate) fn is_transient(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| {
            cause.downcast_ref::<ProxyError>().or_else(|| {
                cause
                    .downcast_ref::<ProxyAndImportError>()
                    .map(|e| &e.proxy)
            })
        })
        .any(is_transient_proxy_error)
}

/// Run `op` until it succeeds, fails with a non-transient error, or the
/// retries in `policy` are exhausted.
///
/// Each retry is reported as a [`ProgressEvent::Message`], which is how
/// callers such as `cfsctl` show it; it is only logged at debug level, so
/// it does not show up twice.
/// `what` describes the operation in those messages.
///
/// `op` must start from scratch every time it is called; nothing from a
/// failed attempt may leak into the next one.
pub(crate) async fn with_retry<T, F, Fut>(
    policy: &RetryPolicy,
    what: &str,
    reporter: &dyn ProgressReporter,
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut retry = 0;
    loop {
        let err = match op().await {
            Ok(v) => return Ok(v),
            Err(err) => err,
        };
        if !is_transient(&err) {
            return Err(err);
        }
        if retry >= policy.max_retries {
            return if retry > 0 {
                let retries = if retry == 1 { "retry" } else { "retries" };
                Err(err).with_context(|| format!("Giving up after {retry} {retries}"))
            } else {
                Err(err)
            };
        }
        let delay = policy.backoff_delay(retry, random_unit());
        retry += 1;
        let msg = format!(
            "{what}: transient error, retrying in {:.1}s ({retry}/{}): {err:#}",
            delay.as_secs_f64(),
            policy.max_retries
        );
        tracing::debug!("{msg}");
        reporter.report(ProgressEvent::Message(msg));
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::progress::NullReporter;

    /// A progress reporter that records messages.
    #[derive(Debug, Default)]
    struct MessageLog(Mutex<Vec<String>>);

    impl ProgressReporter for MessageLog {
        fn report(&self, event: ProgressEvent) {
            if let ProgressEvent::Message(m) = event {
                self.0.lock().unwrap().push(m);
            }
        }
    }

    const FAST_RETRIES: RetryPolicy = RetryPolicy {
        max_retries: 3,
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };

    /// An error as the proxy reports most failures, e.g. for `GetBlob` or
    /// `FinishPipe`.
    fn proxy_failure(msg: &str) -> anyhow::Error {
        ProxyError::RequestInitiationFailure {
            method: "GetBlob".into(),
            error: msg.into(),
        }
        .into()
    }

    #[test]
    fn test_is_transient() {
        let proxy = |e: ProxyError| anyhow::Error::from(e);
        let msg = proxy_failure;
        let cases = [
            // Messages as produced by skopeo's proxy for real-world failures
            (
                msg(
                    "reading blob sha256:abcd: fetching blob: received unexpected HTTP status: 503 Service Unavailable",
                ),
                true,
            ),
            (
                msg("received unexpected HTTP status: 502 Bad Gateway"),
                true,
            ),
            (
                msg(
                    "pinging container registry quay.io: Get \"https://quay.io/v2/\": dial tcp 1.2.3.4:443: i/o timeout",
                ),
                true,
            ),
            (
                msg("read tcp 1.2.3.4:5->6.7.8.9:443: read: connection reset by peer"),
                true,
            ),
            (msg("unexpected EOF"), true),
            (
                msg("pinging container registry quay.io: Get \"https://quay.io/v2/\": EOF"),
                true,
            ),
            (msg("http2: client connection lost"), true),
            (msg("expected 1234 bytes in blob, got 1000"), true),
            (msg("corrupted blob, expecting sha256:abcd"), true),
            (msg("toomanyrequests: rate limit exceeded"), true),
            (msg("reading manifest: too many requests to registry"), true),
            (
                proxy(ProxyError::RequestReturned(
                    "Get \"https://quay.io/v2/\": net/http: TLS handshake timeout".into(),
                )),
                true,
            ),
            (
                proxy(ProxyError::BlobError(GetBlobError::Retryable(
                    "something".into(),
                ))),
                true,
            ),
            // Transient cause wrapped in context
            (
                msg("connection refused").context("Failed to import layer sha256:abcd"),
                true,
            ),
            // Both the proxy and the import failed: the proxy decides
            (
                anyhow::Error::from(ProxyAndImportError {
                    proxy: ProxyError::RequestReturned("unexpected EOF".into()),
                    import: anyhow::anyhow!("unexpected EOF in tar stream"),
                })
                .context("Fetching layer sha256:abcd"),
                true,
            ),
            (
                ProxyAndImportError {
                    proxy: ProxyError::RequestReturned("manifest unknown".into()),
                    import: anyhow::anyhow!("unexpected EOF in tar stream"),
                }
                .into(),
                false,
            ),
            // Permanent failures
            (msg("reading blob: EOF is not a valid digest"), false),
            (msg("received unexpected HTTP status: 404 Not Found"), false),
            (
                msg(
                    "reading manifest latest in quay.io/foo/bar: unauthorized: access to the requested resource is not authorized",
                ),
                false,
            ),
            (msg("manifest unknown"), false),
            (msg("expected layer media type"), false),
            (
                proxy(ProxyError::BlobError(GetBlobError::Other(
                    "something".into(),
                ))),
                false,
            ),
            (
                proxy(ProxyError::Io(std::io::Error::other("skopeo exited"))),
                false,
            ),
            (
                proxy(ProxyError::Other(
                    "skopeo proxy unexpectedly exited: connection reset by peer".into(),
                )),
                false,
            ),
            // Local errors are never retried, whatever they say
            (anyhow::anyhow!("unexpected EOF in tar stream"), false),
            (
                anyhow::anyhow!("connection reset by peer").context("Importing layer"),
                false,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(is_transient(&err), expected, "{err:#}");
        }
    }

    #[test]
    fn test_backoff_delay() {
        let policy = RetryPolicy {
            max_retries: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        };
        // (retry, jitter, expected)
        let cases = [
            (0, 0.0, Duration::from_secs(1)),
            (1, 0.0, Duration::from_secs(2)),
            (2, 0.0, Duration::from_secs(4)),
            (4, 0.0, Duration::from_secs(16)),
            // Capped
            (5, 0.0, Duration::from_secs(30)),
            (u32::MAX, 0.0, Duration::from_secs(30)),
            // Jitter only ever shortens the delay, by at most 20%
            (0, 0.5, Duration::from_millis(900)),
            (5, 1.0, Duration::from_secs(24)),
        ];
        for (retry, jitter, expected) in cases {
            assert_eq!(
                policy.backoff_delay(retry, jitter),
                expected,
                "retry={retry} jitter={jitter}"
            );
        }
        assert_eq!(RetryPolicy::none().backoff_delay(3, 0.5), Duration::ZERO);
        // Must not overflow
        let huge = RetryPolicy {
            max_retries: 1,
            initial_delay: Duration::MAX,
            max_delay: Duration::MAX,
        };
        for jitter in [0.0, 0.5, 0.999] {
            assert!(huge.backoff_delay(u32::MAX, jitter) > Duration::from_secs(u64::MAX / 2));
        }
    }

    #[test]
    fn test_random_unit_range() {
        for _ in 0..1000 {
            let v = random_unit();
            assert!((0.0..1.0).contains(&v), "{v}");
        }
    }

    /// Drive [`with_retry`] with a fetcher that fails with the given errors
    /// in order and then succeeds.
    #[tokio::test]
    async fn test_with_retry() {
        const TRANSIENT: &str = "received unexpected HTTP status: 503 Service Unavailable";
        const PERMANENT: &str = "unauthorized: authentication required";
        const ONE_RETRY: RetryPolicy = RetryPolicy {
            max_retries: 1,
            ..FAST_RETRIES
        };
        // (policy, errors before success, expected attempts, expected
        // success, expected context of the final error)
        type Case<'a> = (RetryPolicy, &'a [&'a str], usize, bool, Option<&'a str>);
        let cases: &[Case] = &[
            (FAST_RETRIES, &[], 1, true, None),
            (FAST_RETRIES, &[TRANSIENT], 2, true, None),
            (FAST_RETRIES, &[TRANSIENT; 3], 4, true, None),
            // Out of retries
            (
                FAST_RETRIES,
                &[TRANSIENT; 4],
                4,
                false,
                Some("Giving up after 3 retries"),
            ),
            (
                ONE_RETRY,
                &[TRANSIENT; 2],
                2,
                false,
                Some("Giving up after 1 retry"),
            ),
            // Permanent errors are not retried, even after transient ones
            (FAST_RETRIES, &[PERMANENT], 1, false, None),
            (FAST_RETRIES, &[TRANSIENT, PERMANENT], 2, false, None),
            // Retrying disabled
            (RetryPolicy::none(), &[TRANSIENT], 1, false, None),
        ];
        for (policy, errors, expected_attempts, expected_ok, expected_context) in cases {
            let log = MessageLog::default();
            let mut attempts = 0;
            let r = with_retry(policy, "Fetching thing", &log, || {
                let result = match errors.get(attempts) {
                    Some(e) => Err(proxy_failure(e)),
                    None => Ok(attempts),
                };
                attempts += 1;
                std::future::ready(result)
            })
            .await;
            let ctx = format!("policy={policy:?} errors={errors:?}");
            assert_eq!(attempts, *expected_attempts, "{ctx}");
            assert_eq!(r.is_ok(), *expected_ok, "{ctx}: {r:?}");
            let messages = log.0.into_inner().unwrap();
            assert_eq!(messages.len(), expected_attempts - 1, "{ctx}");
            for (i, m) in messages.iter().enumerate() {
                let n = i + 1;
                let max = policy.max_retries;
                assert!(
                    m.starts_with("Fetching thing: transient error, retrying in ")
                        && m.contains(&format!("({n}/{max}): "))
                        && m.ends_with(TRANSIENT),
                    "{ctx}: {m}"
                );
            }
            if let Err(e) = r {
                let outer = e.to_string();
                match expected_context {
                    Some(c) => assert_eq!(outer, *c, "{ctx}"),
                    None => assert!(outer.starts_with("failed to invoke method"), "{ctx}: {e:#}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn test_with_retry_sleeps() {
        let policy = RetryPolicy {
            max_retries: 1,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(1),
        };
        let start = std::time::Instant::now();
        let mut failed = false;
        with_retry(&policy, "x", &NullReporter, || {
            let r = if failed {
                Ok(())
            } else {
                Err(proxy_failure("connection reset by peer"))
            };
            failed = true;
            std::future::ready(r)
        })
        .await
        .unwrap();
        // At least the delay minus the maximum jitter
        assert!(start.elapsed() >= Duration::from_millis(40));
    }
}
