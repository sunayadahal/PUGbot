//! Normal-distribution helpers used by the TrueSkill adapter.
//!
//! These are self-contained so the rating code has no numeric dependency.

use std::f64::consts::{PI, SQRT_2};

/// Standard normal probability density.
pub fn pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * PI).sqrt()
}

/// Standard normal cumulative distribution.
pub fn cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / SQRT_2))
}

/// Inverse standard normal CDF (Acklam's rational approximation, refined by a
/// single Halley step; accurate to roughly 1e-15).
pub fn inv_cdf(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }

    // Acklam's published coefficients, kept verbatim so they can be checked
    // against the reference. Some have more digits than an f64 can hold; the
    // excess is harmless and truncating them would obscure the provenance.
    #[allow(clippy::excessive_precision)]
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_690e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239e0,
    ];
    #[allow(clippy::excessive_precision)]
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    #[allow(clippy::excessive_precision)]
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838e0,
        -2.549_732_539_343_734e0,
        4.374_664_141_464_968e0,
        2.938_163_982_698_783e0,
    ];
    #[allow(clippy::excessive_precision)]
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996e0,
        3.754_408_661_907_416e0,
    ];
    const P_LOW: f64 = 0.02425;

    let x = if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };

    // One Halley refinement against the true CDF.
    let e = cdf(x) - p;
    let u = e * (2.0 * PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

/// Error function (Abramowitz & Stegun 7.1.26 with a Chebyshev tail), max
/// absolute error below 1.2e-7 — well inside rating display precision.
pub fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;

    let t = 1.0 / (1.0 + P * x);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-x * x).exp();
    sign * y
}

/// TrueSkill's `V` for a win: the truncated-Gaussian mean shift.
pub fn v_win(t: f64, epsilon: f64) -> f64 {
    let denominator = cdf(t - epsilon);
    if denominator < 1e-9 {
        // Deep in the tail the ratio is numerically unstable; the limit is
        // linear in the argument.
        return epsilon - t;
    }
    pdf(t - epsilon) / denominator
}

/// TrueSkill's `W` for a win: the variance multiplier.
pub fn w_win(t: f64, epsilon: f64) -> f64 {
    let v = v_win(t, epsilon);
    let w = v * (v + t - epsilon);
    w.clamp(0.0, 1.0)
}

/// TrueSkill's `V` for a draw.
pub fn v_draw(t: f64, epsilon: f64) -> f64 {
    let abs_t = t.abs();
    let denominator = cdf(epsilon - abs_t) - cdf(-epsilon - abs_t);
    if denominator < 1e-9 {
        return if t < 0.0 { -t - epsilon } else { -t + epsilon };
    }
    let numerator = pdf(-epsilon - abs_t) - pdf(epsilon - abs_t);
    let value = numerator / denominator;
    if t < 0.0 {
        -value
    } else {
        value
    }
}

/// TrueSkill's `W` for a draw.
pub fn w_draw(t: f64, epsilon: f64) -> f64 {
    let abs_t = t.abs();
    let denominator = cdf(epsilon - abs_t) - cdf(-epsilon - abs_t);
    if denominator < 1e-9 {
        return 1.0;
    }
    let v = v_draw(t, epsilon);
    let w = v * v
        + ((epsilon - abs_t) * pdf(epsilon - abs_t) + (epsilon + abs_t) * pdf(-epsilon - abs_t))
            / denominator;
    w.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) {
        assert!((a - b).abs() < tolerance, "{a} != {b} (± {tolerance})");
    }

    #[test]
    fn cdf_matches_known_values() {
        close(cdf(0.0), 0.5, 1e-7);
        close(cdf(1.0), 0.841_344_746, 1e-6);
        close(cdf(-1.96), 0.024_997_9, 1e-6);
        close(cdf(2.575_829), 0.995, 1e-5);
    }

    #[test]
    fn pdf_matches_known_values() {
        close(pdf(0.0), 0.398_942_280, 1e-9);
        close(pdf(1.0), 0.241_970_724, 1e-9);
    }

    #[test]
    fn inv_cdf_inverts_cdf() {
        for p in [0.001, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 0.999] {
            close(cdf(inv_cdf(p)), p, 1e-6);
        }
        assert!(inv_cdf(0.0).is_infinite());
        assert!(inv_cdf(1.0).is_infinite());
    }

    #[test]
    fn erf_is_odd_and_bounded() {
        close(erf(0.0), 0.0, 1e-9);
        close(erf(1.0), 0.842_700_79, 1e-6);
        close(erf(-1.0), -0.842_700_79, 1e-6);
        assert!(erf(5.0) <= 1.0 && erf(5.0) > 0.999_999);
    }

    #[test]
    fn truncated_gaussian_helpers_stay_in_range() {
        for t in [-10.0, -3.0, -0.5, 0.0, 0.5, 3.0, 10.0] {
            let w = w_win(t, 0.1);
            assert!((0.0..=1.0).contains(&w), "w_win({t}) = {w}");
            let wd = w_draw(t, 0.5);
            assert!((0.0..=1.0).contains(&wd), "w_draw({t}) = {wd}");
        }
    }

    #[test]
    fn v_win_is_larger_for_an_unexpected_result() {
        // A heavy underdog winning shifts the mean far more than a favourite.
        assert!(v_win(-2.0, 0.0) > v_win(2.0, 0.0));
    }
}
