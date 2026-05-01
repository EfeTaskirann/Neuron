//! WP-W3-06 — per-span sampling decision.
//!
//! The export ratio is read once from `NEURON_OTEL_SAMPLING_RATIO`
//! and cached in a `OnceLock<f64>`. Values outside `0.0..=1.0` and
//! parse failures fall back to `1.0` (export everything) — a
//! conservative default that matches the migration's `sampled_in
//! NOT NULL DEFAULT 1`. Doing the env-var read once also means
//! changing the ratio mid-process requires a restart, which is fine
//! for a desktop app and avoids a `RwLock` on the hot path.
//!
//! Per WP §"Scope": sampling is per-span in this WP, NOT per-run. A
//! later WP can layer a per-run hash-based sampler on top without
//! breaking the storage shape.

use std::sync::OnceLock;

/// Read and cache the sampling ratio from `NEURON_OTEL_SAMPLING_RATIO`.
/// Returns `1.0` (export everything) if the env var is unset, fails
/// to parse, or falls outside `[0.0, 1.0]`.
pub fn sampling_ratio() -> f64 {
    static RATIO: OnceLock<f64> = OnceLock::new();
    *RATIO.get_or_init(|| {
        std::env::var("NEURON_OTEL_SAMPLING_RATIO")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(1.0)
    })
}

/// Roll a per-span sampling decision against [`sampling_ratio`].
///
/// Branches:
/// - `ratio >= 1.0` → always include (the common case — env var
///   unset / set to "1" — hits this path with no PRNG cost).
/// - `ratio <= 0.0` → never include (export disabled but the rest
///   of the runtime keeps writing rows for in-app inspection).
/// - else → `rand::random::<f64>() < ratio` (uniform).
pub fn sampled_in() -> bool {
    let ratio = sampling_ratio();
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 {
        return false;
    }
    rand::random::<f64>() < ratio
}

#[cfg(test)]
mod tests {
    //! These tests cannot exercise [`sampling_ratio`] directly because
    //! the `OnceLock` initialiser fires once per process, and Cargo
    //! test runners share the process across `#[test]` invocations.
    //! We instead test the underlying math via a parameter-injectable
    //! helper and assert that the env-var path is wired through the
    //! `unwrap_or(1.0)` fallback in the documented way.

    /// Local twin of [`super::sampled_in`] that takes the ratio as a
    /// parameter so tests can iterate without polluting the
    /// `OnceLock`. Mirrors the production logic byte-for-byte.
    fn sampled_in_with(ratio: f64) -> bool {
        if ratio >= 1.0 {
            return true;
        }
        if ratio <= 0.0 {
            return false;
        }
        rand::random::<f64>() < ratio
    }

    /// `=1.0` always passes — no PRNG roll happens at all.
    #[test]
    fn ratio_one_always_samples_in() {
        for _ in 0..1000 {
            assert!(sampled_in_with(1.0));
        }
    }

    /// `=0.0` always rejects — symmetric with the `>= 1.0` short
    /// circuit so the export pipeline can be disabled without code
    /// changes by setting the env var to `0`.
    #[test]
    fn ratio_zero_always_samples_out() {
        for _ in 0..1000 {
            assert!(!sampled_in_with(0.0));
        }
    }

    /// `=0.5` over 1000 trials falls within a binomial ±3σ window.
    /// `σ = sqrt(n * p * (1-p))` for n=1000, p=0.5 → σ ≈ 15.81, so
    /// ±3σ ≈ ±48 samples. Window: [452, 548]. We give an extra
    /// handful of headroom for non-deterministic flakes (the global
    /// `rand` PRNG is seeded from OS entropy).
    #[test]
    fn ratio_half_is_within_binomial_tolerance() {
        let n = 1000;
        let mut hits = 0;
        for _ in 0..n {
            if sampled_in_with(0.5) {
                hits += 1;
            }
        }
        // 3σ ≈ 48, so [450, 550] is a safe band even with seeded-
        // entropy variance. Float comparison is intentionally
        // tolerance-based, NOT equality (per WP §"Notes / risks":
        // "binomial ±3σ tolerance, NOT equality").
        assert!(
            (450..=550).contains(&hits),
            "expected ~500/1000 hits at p=0.5, got {hits}"
        );
    }

    /// Out-of-range ratios fall back to `1.0` per the documented
    /// `unwrap_or(1.0)` chain. Tested by simulating the same parse
    /// chain locally.
    #[test]
    fn out_of_range_parse_falls_back_to_one() {
        let parsed: f64 = "1.5"
            .parse::<f64>()
            .ok()
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(1.0);
        assert_eq!(parsed, 1.0);

        let parsed: f64 = "-0.1"
            .parse::<f64>()
            .ok()
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(1.0);
        assert_eq!(parsed, 1.0);

        let parsed: f64 = "not a number"
            .parse::<f64>()
            .ok()
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(1.0);
        assert_eq!(parsed, 1.0);
    }
}
