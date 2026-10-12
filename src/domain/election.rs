//! Weekly selection algorithm (docs/DECISIONS.md D-06..D-09).
//!
//! Deterministic and channel-local: callers run it once per channel, so no
//! channel can influence another's field.

/// Final Nine seats per universal channel (3 x 3 = 9).
pub const SEATS_PER_CHANNEL: usize = 3;
/// Field size that survives selection 1, 2, 3 and 4 (the last is the seats).
pub const STAGE_CUTOFFS: [usize; 4] = [10, 7, 5, SEATS_PER_CHANNEL];
/// A seat is only awarded to an ideology with at least this much support.
pub const MIN_SUPPORT_FOR_SEAT: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub ideology_id: i64,
    pub support: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked {
    pub ideology_id: i64,
    pub support: i64,
    /// 1-based position within the channel at this selection.
    pub rank: usize,
    pub advanced: bool,
}

/// Run selection `stage` (1..=4) over one channel's remaining candidates.
///
/// Order: higher support first; ties go to the older ideology, then the
/// lower id. Incumbency by itself gives no protection: an incumbent only
/// wins ties because it is older.
pub fn select(stage: u8, mut cands: Vec<Candidate>) -> Vec<Ranked> {
    assert!((1..=4).contains(&stage), "stage must be 1..=4");
    cands.sort_by(|a, b| {
        b.support
            .cmp(&a.support)
            .then(a.created_at.cmp(&b.created_at))
            .then(a.ideology_id.cmp(&b.ideology_id))
    });
    let cutoff = STAGE_CUTOFFS[stage as usize - 1];
    cands
        .into_iter()
        .enumerate()
        .map(|(i, c)| Ranked {
            ideology_id: c.ideology_id,
            support: c.support,
            rank: i + 1,
            advanced: i < cutoff && (stage < 4 || c.support >= MIN_SUPPORT_FOR_SEAT),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: i64, support: i64, created_at: i64) -> Candidate {
        Candidate { ideology_id: id, support, created_at }
    }

    #[test]
    fn ranks_by_support_desc() {
        let r = select(1, vec![c(1, 5, 0), c(2, 9, 0), c(3, 7, 0)]);
        let ids: Vec<_> = r.iter().map(|x| x.ideology_id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
        assert_eq!(r[0].rank, 1);
    }

    #[test]
    fn ties_older_then_lower_id() {
        let r = select(1, vec![c(5, 4, 200), c(4, 4, 100), c(3, 4, 100)]);
        let ids: Vec<_> = r.iter().map(|x| x.ideology_id).collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    #[test]
    fn deterministic_regardless_of_input_order() {
        let a = vec![c(1, 3, 1), c(2, 3, 1), c(3, 8, 5), c(4, 0, 9)];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(select(2, a), select(2, b));
    }

    #[test]
    fn cutoffs_per_stage() {
        let field: Vec<_> = (1..=12).map(|i| c(i, 100 - i, i)).collect();
        for (stage, want) in [(1u8, 10usize), (2, 7), (3, 5), (4, 3)] {
            let n = select(stage, field.clone()).iter().filter(|r| r.advanced).count();
            assert_eq!(n, want, "stage {stage}");
        }
    }

    #[test]
    fn small_field_all_advance_until_final() {
        let r = select(1, vec![c(1, 0, 1), c(2, 0, 2)]);
        assert!(r.iter().all(|x| x.advanced));
    }

    #[test]
    fn final_stage_needs_support_for_a_seat() {
        let r = select(4, vec![c(1, 3, 1), c(2, 0, 2), c(3, 0, 3)]);
        assert_eq!(r.iter().filter(|x| x.advanced).count(), 1);
        assert!(r[0].advanced && !r[1].advanced);
    }

    #[test]
    fn never_more_than_seats() {
        let field: Vec<_> = (1..=50).map(|i| c(i, 10, i)).collect();
        assert_eq!(select(4, field).iter().filter(|r| r.advanced).count(), SEATS_PER_CHANNEL);
    }
}
