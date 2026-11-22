//! Shared motion primitives: easings + page-enter transitions (stock GPUI
//! `with_animation` only — no vendored patches, no extra deps).
//!
//! Rationale (matches Argus's measured call): pages slide, they don't fade.
//! A fade "jumps halfway in the first frames, then creeps"; a 250ms
//! ease-out slide reads as one compositor blit.

/// Page-enter slide duration (matches Argus `PAGE_ENTER`, keeps feel parity).
pub const PAGE_ENTER_MS: u64 = 250;
/// Page-enter travel distance in px (horizontal; Argus uses 32px).
pub const PAGE_ENTER_PX: f32 = 32.0;

/// ease-out cubic: fast start, soft landing. The default for entrances.
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let p = 1.0 - t.clamp(0.0, 1.0);
    1.0 - p * p * p
}

/// ease-in-out cubic: for symmetric motion (pills, indicators).
#[inline]
#[allow(dead_code, reason = "used by the upcoming segmented-control glide")]
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_hit_endpoints_and_stay_bounded() {
        for f in [ease_out_cubic as fn(f32) -> f32, ease_in_out_cubic] {
            assert_eq!(f(0.0), 0.0);
            assert_eq!(f(1.0), 1.0);
            for i in 0..=100 {
                let v = f(i as f32 / 100.0);
                assert!((0.0..=1.0).contains(&v), "out of bounds at {i}: {v}");
            }
        }
    }

    #[test]
    fn ease_out_is_monotonic_and_front_loaded() {
        // Front-loaded: halfway through time, progress is well past halfway.
        let mid = ease_out_cubic(0.5);
        assert!(mid > 0.5, "ease-out must front-load, got {mid}");
        let mut prev = 0.0f32;
        for i in 1..=100 {
            let v = ease_out_cubic(i as f32 / 100.0);
            assert!(v >= prev, "must be monotonic");
            prev = v;
        }
    }

    #[test]
    fn ease_in_out_is_symmetric() {
        for i in 0..=50 {
            let t = i as f32 / 100.0;
            assert!((ease_in_out_cubic(t) + ease_in_out_cubic(1.0 - t) - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn page_enter_constants_match_argus_feel() {
        assert_eq!(PAGE_ENTER_MS, 250);
        assert_eq!(PAGE_ENTER_PX, 32.0);
    }
}
