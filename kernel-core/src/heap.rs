//! Pure heap-policy logic. No allocator, no `unsafe`, no statics —
//! decisions about *when* and *by how much* to grow the heap given
//! its current state, suitable for host testing. The kernel side
//! owns the allocator, the frame supply, and the page-table walk;
//! it calls into here to decide whether to act.

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max_size: usize) -> WatermarkConfig {
        WatermarkConfig {
            max_size,
            free_threshold_pct: 25,
            grow_frames: 256,
            frame_size: 4096,
        }
    }

    fn stats(capacity: usize, used: usize) -> Stats {
        Stats { capacity, used, free: capacity - used }
    }

    #[test]
    fn grows_when_free_below_threshold_with_headroom() {
        // 4 MiB capacity, 3.5 MiB used → 0.5 MiB free.
        // Threshold = 25% of 4 MiB = 1 MiB. free (0.5) < threshold (1) → grow.
        let s = stats(4 * 1024 * 1024, 7 * 512 * 1024);
        let c = cfg(1024 * 1024 * 1024);
        assert_eq!(watermark_grow_decision(s, &c), Some(256));
    }

    #[test]
    fn does_not_grow_when_free_above_threshold() {
        // 4 MiB capacity, 2 MiB used → 2 MiB free. Threshold 1 MiB.
        let s = stats(4 * 1024 * 1024, 2 * 1024 * 1024);
        let c = cfg(1024 * 1024 * 1024);
        assert_eq!(watermark_grow_decision(s, &c), None);
    }

    #[test]
    fn does_not_grow_when_free_at_threshold_exactly() {
        // Strict less-than at threshold — equal value is "fine, not yet."
        let capacity = 4 * 1024 * 1024;
        let threshold = capacity / 4;
        let s = stats(capacity, capacity - threshold);
        assert_eq!(s.free, threshold);
        let c = cfg(1024 * 1024 * 1024);
        assert_eq!(watermark_grow_decision(s, &c), None);
    }

    #[test]
    fn does_not_grow_at_ceiling_even_under_pressure() {
        // capacity == max_size: nothing left to grow into.
        let max = 4 * 1024 * 1024;
        let s = stats(max, max - 1);
        let c = cfg(max);
        assert_eq!(watermark_grow_decision(s, &c), None);
    }

    #[test]
    fn clamps_grow_request_to_remaining_headroom() {
        // 1020 frames already mapped; max is 1024; request 256 → clamp to 4.
        let frame_size = 4096;
        let capacity = 1020 * frame_size;
        let max = 1024 * frame_size;
        // Force pressure: free = small.
        let s = stats(capacity, capacity - 1);
        let c = WatermarkConfig {
            max_size: max,
            free_threshold_pct: 25,
            grow_frames: 256,
            frame_size,
        };
        assert_eq!(watermark_grow_decision(s, &c), Some(4));
    }

    #[test]
    fn returns_none_when_capacity_is_zero() {
        // Init hasn't run yet — heap has no capacity, no decision to
        // make. Stays defensive: never reports "grow this empty thing."
        let s = stats(0, 0);
        let c = cfg(1024 * 1024 * 1024);
        assert_eq!(watermark_grow_decision(s, &c), None);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Total heap region size in bytes.
    pub capacity: usize,
    /// Sum of alignment-padded `layout.size()` across live allocations.
    /// Excludes hole-list metadata, so slightly undercounts unavailable
    /// bytes vs the true `capacity - free`.
    pub used: usize,
    /// Bytes the heap considers free — `capacity - used`, so does
    /// include hole-list metadata as "free" even though it isn't
    /// usable for allocations.
    pub free: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct WatermarkConfig {
    /// Ceiling on `capacity`. Once reached, no more growth.
    pub max_size: usize,
    /// Grow when `free < capacity * free_threshold_pct / 100`.
    /// Equal to threshold doesn't trigger (strict less-than).
    pub free_threshold_pct: u32,
    /// Frames to request per grow event, clamped to remaining headroom.
    pub grow_frames: usize,
    pub frame_size: usize,
}

/// Decide whether to grow the heap given its current `stats` and the
/// configured policy. Returns `Some(n_frames)` to request, or `None`
/// if no grow is warranted (above threshold, at ceiling, or
/// uninitialised). The returned count is clamped to fit under
/// `cfg.max_size`.
pub fn watermark_grow_decision(stats: Stats, cfg: &WatermarkConfig) -> Option<usize> {
    if stats.capacity == 0 || stats.capacity >= cfg.max_size {
        return None;
    }
    let threshold = stats.capacity / 100 * cfg.free_threshold_pct as usize;
    if stats.free >= threshold {
        return None;
    }
    let headroom_frames = (cfg.max_size - stats.capacity) / cfg.frame_size;
    Some(cfg.grow_frames.min(headroom_frames))
}
