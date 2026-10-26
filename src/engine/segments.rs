//! Splitting a known content length into byte ranges, one per connection.

/// A half-open-on-the-wire byte range. HTTP `Range` is inclusive on both ends,
/// so `end` is the last byte index, not one past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start: u64,
    /// Inclusive last byte.
    pub end: u64,
}

impl Segment {
    /// Number of bytes this segment covers. (Used by tests and resume math.)
    #[allow(dead_code)]
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Divide `total` bytes into at most `connections` contiguous segments.
///
/// The last segment absorbs the remainder, so the segments always cover
/// `0..=total-1` exactly with no gap or overlap. `connections` is clamped to at
/// least 1, and never exceeds `total` (no empty segments). A `total` of 0
/// yields no segments.
pub fn split(total: u64, connections: usize) -> Vec<Segment> {
    if total == 0 {
        return Vec::new();
    }
    let n = (connections.max(1) as u64).min(total);
    let base = total / n;
    let mut segments = Vec::with_capacity(n as usize);
    let mut start = 0u64;
    for i in 0..n {
        // The final segment takes whatever is left, covering any remainder.
        let end = if i == n - 1 { total - 1 } else { start + base - 1 };
        segments.push(Segment { start, end });
        start = end + 1;
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covers_exactly(total: u64, segs: &[Segment]) {
        assert_eq!(segs.first().unwrap().start, 0);
        assert_eq!(segs.last().unwrap().end, total - 1);
        for pair in segs.windows(2) {
            // Contiguous: each segment starts right after the previous ends.
            assert_eq!(pair[1].start, pair[0].end + 1);
        }
        let sum: u64 = segs.iter().map(|s| s.len()).sum();
        assert_eq!(sum, total);
    }

    #[test]
    fn even_split() {
        let s = split(1000, 4);
        assert_eq!(s.len(), 4);
        covers_exactly(1000, &s);
        assert_eq!(s[0].len(), 250);
    }

    #[test]
    fn remainder_goes_to_last() {
        let s = split(1003, 4);
        assert_eq!(s.len(), 4);
        covers_exactly(1003, &s);
        assert_eq!(s[3].len(), 253);
    }

    #[test]
    fn more_connections_than_bytes() {
        let s = split(3, 8);
        assert_eq!(s.len(), 3);
        covers_exactly(3, &s);
    }

    #[test]
    fn single_byte() {
        let s = split(1, 8);
        assert_eq!(s, vec![Segment { start: 0, end: 0 }]);
    }

    #[test]
    fn empty() {
        assert!(split(0, 4).is_empty());
    }

    #[test]
    fn zero_connections_clamped() {
        let s = split(100, 0);
        assert_eq!(s.len(), 1);
        covers_exactly(100, &s);
    }
}
