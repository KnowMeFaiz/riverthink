/// Voting policy (docs/DECISIONS.md D-02, D-03):
/// * one account = one active vote;
/// * a vote may be moved, but only once per `cooldown` after the last cast/move;
/// * during week 4 of a running election every vote operation is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoteDecision {
    Allow,
    AlreadySupporting,
    Cooldown { until: i64 },
    LockedFinalWeek,
}

/// `current` is `(ideology_id, cast_at)` of the user's existing vote, if any.
pub fn decide(
    current: Option<(i64, i64)>,
    target: i64,
    now: i64,
    final_week: bool,
    cooldown_secs: i64,
) -> VoteDecision {
    if final_week {
        return VoteDecision::LockedFinalWeek;
    }
    match current {
        None => VoteDecision::Allow,
        Some((id, _)) if id == target => VoteDecision::AlreadySupporting,
        Some((_, cast_at)) => {
            let until = cast_at + cooldown_secs;
            if now < until {
                VoteDecision::Cooldown { until }
            } else {
                VoteDecision::Allow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CD: i64 = 7 * 86_400;

    #[test]
    fn first_vote_allowed() {
        assert_eq!(decide(None, 1, 100, false, CD), VoteDecision::Allow);
    }

    #[test]
    fn same_target_is_noop() {
        assert_eq!(decide(Some((1, 0)), 1, 100, false, CD), VoteDecision::AlreadySupporting);
    }

    #[test]
    fn cooldown_blocks_then_releases() {
        assert_eq!(decide(Some((1, 0)), 2, CD - 1, false, CD), VoteDecision::Cooldown { until: CD });
        assert_eq!(decide(Some((1, 0)), 2, CD, false, CD), VoteDecision::Allow);
    }

    #[test]
    fn final_week_locks_everything() {
        assert_eq!(decide(None, 1, 0, true, CD), VoteDecision::LockedFinalWeek);
        assert_eq!(decide(Some((1, 0)), 2, 10 * CD, true, CD), VoteDecision::LockedFinalWeek);
        assert_eq!(decide(Some((1, 0)), 1, 10 * CD, true, CD), VoteDecision::LockedFinalWeek);
    }
}
