#[derive(Clone, Copy, Debug)]
pub(crate) struct AutoScrollConfig {
    pub(crate) dead_zone_px: f64,
    pub(crate) pixels_per_tick_per_px: f64,
    pub(crate) max_pixels_per_tick: f64,
}

impl Default for AutoScrollConfig {
    fn default() -> Self {
        Self {
            dead_zone_px: 10.0,
            pixels_per_tick_per_px: 0.18,
            max_pixels_per_tick: 56.0,
        }
    }
}

pub(crate) fn auto_scroll_delta_per_tick(
    active: bool,
    anchor_y: f64,
    cursor_y: f64,
    config: AutoScrollConfig,
) -> f32 {
    if !active {
        return 0.0;
    }

    let distance = cursor_y - anchor_y;
    let magnitude = distance.abs();
    if magnitude <= config.dead_zone_px {
        return 0.0;
    }

    let speed = ((magnitude - config.dead_zone_px) * config.pixels_per_tick_per_px)
        .min(config.max_pixels_per_tick);
    speed.copysign(distance) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AutoScrollConfig {
        AutoScrollConfig {
            dead_zone_px: 10.0,
            pixels_per_tick_per_px: 0.2,
            max_pixels_per_tick: 30.0,
        }
    }

    #[test]
    fn inactive_session_does_not_scroll() {
        assert_eq!(
            auto_scroll_delta_per_tick(false, 100.0, 180.0, config()),
            0.0
        );
    }

    #[test]
    fn dead_zone_does_not_scroll() {
        assert_eq!(
            auto_scroll_delta_per_tick(true, 100.0, 109.0, config()),
            0.0
        );
        assert_eq!(auto_scroll_delta_per_tick(true, 100.0, 90.0, config()), 0.0);
    }

    #[test]
    fn below_anchor_scrolls_down() {
        let delta = auto_scroll_delta_per_tick(true, 100.0, 140.0, config());
        assert!(delta > 0.0);
        assert_eq!(delta, 6.0);
    }

    #[test]
    fn above_anchor_scrolls_up() {
        let delta = auto_scroll_delta_per_tick(true, 100.0, 60.0, config());
        assert!(delta < 0.0);
        assert_eq!(delta, -6.0);
    }

    #[test]
    fn speed_is_capped() {
        assert_eq!(
            auto_scroll_delta_per_tick(true, 100.0, 500.0, config()),
            30.0
        );
        assert_eq!(
            auto_scroll_delta_per_tick(true, 100.0, -300.0, config()),
            -30.0
        );
    }
}
