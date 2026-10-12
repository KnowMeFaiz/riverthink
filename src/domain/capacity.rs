/// Rule A: `S_i = max(S_min, floor(0.10 * U_i))`.
///
/// Integer division is exactly `floor(U/10)`, so no floating point is involved
/// and the result is fully deterministic.
///
/// `S_i` is interpreted (see docs/DECISIONS.md D-05) as the number of new
/// ideologies that may be *founded* in channel `i` during one generation.
/// It never deletes or demotes an existing ideology.
pub fn capacity(active_users: u32, s_min: u32) -> u32 {
    s_min.max(active_users / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_applies_for_small_populations() {
        assert_eq!(capacity(0, 3), 3);
        assert_eq!(capacity(29, 3), 3);
        assert_eq!(capacity(30, 3), 3);
    }

    #[test]
    fn ten_percent_floor() {
        assert_eq!(capacity(39, 3), 3);
        assert_eq!(capacity(40, 3), 4);
        assert_eq!(capacity(100, 3), 10);
        assert_eq!(capacity(1999, 3), 199);
        assert_eq!(capacity(2000, 3), 200);
    }

    #[test]
    fn channels_are_independent() {
        // 100 users split unevenly: each channel uses only its own U_i.
        assert_eq!(capacity(80, 3), 8);
        assert_eq!(capacity(10, 3), 3);
        assert_eq!(capacity(10, 3), 3);
    }
}
