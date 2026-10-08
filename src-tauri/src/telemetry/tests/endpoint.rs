//! Security-review M3: `validate_endpoint` accepts the well-formed
//! shapes a real OTLP collector exposes and rejects everything else
//! with a non-empty reason string.

/// Security-review M3: validate_endpoint accepts the well-formed
/// shapes a real OTLP collector exposes (https + http to loopback)
/// and rejects everything else with a non-empty reason string.
#[test]
fn validate_endpoint_accepts_https_and_loopback_http() {
    use crate::telemetry::exporter::validate_endpoint;
    assert!(validate_endpoint("https://collector.example/v1/traces").is_ok());
    assert!(validate_endpoint("http://localhost:4318/v1/traces").is_ok());
    assert!(validate_endpoint("http://127.0.0.1:4318/v1/traces").is_ok());
}

#[test]
fn validate_endpoint_rejects_non_http_schemes() {
    use crate::telemetry::exporter::validate_endpoint;
    let cases = [
        "file:///etc/passwd",
        "ftp://collector.example",
        "ipc://something",
        "javascript:alert(1)",
    ];
    for c in cases {
        let err = validate_endpoint(c)
            .unwrap_err_or_else(|()| panic!("{c} should be rejected"));
        assert!(
            err.contains("scheme") || err.contains("URL"),
            "rejection reason for {c} should mention scheme/URL: {err}"
        );
    }
}

#[test]
fn validate_endpoint_rejects_garbage() {
    use crate::telemetry::exporter::validate_endpoint;
    assert!(validate_endpoint("not-a-url").is_err());
    assert!(validate_endpoint("").is_err());
}

/// Plain HTTP to a non-loopback host is allowed (some local-network
/// collectors only speak HTTP) but should NOT be silently equivalent
/// to https. We assert the call returns Ok; the warning emission is
/// observable via tracing in production but not asserted here to
/// keep the test free of subscriber installation.
#[test]
fn validate_endpoint_allows_plain_http_to_remote_with_warning_path() {
    use crate::telemetry::exporter::validate_endpoint;
    assert!(validate_endpoint("http://collector.example:4318/v1/traces").is_ok());
}

/// Tiny shim so the assertion in `validate_endpoint_rejects_non_http_schemes`
/// can stay readable. `Result::unwrap_err_or_else` does not exist in
/// std; we add the equivalent inline via a trait extension scoped to
/// this test module so the call site reads naturally.
trait ResultExt<T, E> {
    fn unwrap_err_or_else<F: FnOnce(()) -> E>(self, f: F) -> E;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn unwrap_err_or_else<F: FnOnce(()) -> E>(self, f: F) -> E {
        match self {
            Ok(_) => f(()),
            Err(e) => e,
        }
    }
}
