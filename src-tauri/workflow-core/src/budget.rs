#![forbid(unsafe_code)]

//! Per-run token budget.
//!
//! A budget belongs to one **run**, not the whole process. Mework hosts multiple conversations
//! concurrently, so this type neither reads a process-global counter nor stores mutable counts.
//! Callers must supply the current accumulated count for the run's owning conversation at every
//! query. This data flow structurally prevents one run from reading another conversation's counter.
//!
//! The limit is a hard cap, not scheduling advice. When [`RunBudget::exhausted`] is `true`, the
//! S15 driver must stop dispatching steps and return [`crate::WorkflowError::BudgetExhausted`].
//! This module only makes stateless accounting decisions; it does not hide admission policy in
//! counting methods.

/// Token budget for a run, relative to its owning conversation counter's starting point.
///
/// `total == None` means unlimited: [`RunBudget::remaining`] returns `None` and
/// [`RunBudget::exhausted`] is always `false`. Avoid a sentinel maximum integer so a counter near
/// `u64::MAX` cannot incorrectly treat unlimited as exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunBudget {
    total: Option<u64>,
    baseline: u64,
}

impl RunBudget {
    /// Creates a budget with the owning conversation counter value at run start.
    pub fn new(total: Option<u64>, baseline: u64) -> Self {
        Self { total, baseline }
    }

    /// Returns the hard cap for this run; `None` means unlimited.
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// Returns tokens spent since run start; callers must supply the owning conversation's count.
    ///
    /// External restoration or reset can temporarily put the counter below its baseline. Saturating
    /// at zero avoids underflow turning that state into a near-`u64::MAX` spend that blocks steps.
    pub fn spent(&self, counter: u64) -> u64 {
        counter.saturating_sub(self.baseline)
    }

    /// Returns tokens still available; `None` means no configured limit.
    ///
    /// Spending beyond the hard cap saturates at zero to preserve exhaustion semantics and avoid
    /// unsigned underflow.
    pub fn remaining(&self, counter: u64) -> Option<u64> {
        self.total
            .map(|total| total.saturating_sub(self.spent(counter)))
    }

    /// Reports whether the hard cap has been reached; unlimited budgets never exhaust.
    pub fn exhausted(&self, counter: u64) -> bool {
        self.remaining(counter) == Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlimited_budget_never_reports_remaining_tokens_or_exhaustion() {
        let budget = RunBudget::new(None, 42);

        assert_eq!(budget.total(), None);
        assert_eq!(budget.remaining(u64::MAX), None);
        assert!(!budget.exhausted(u64::MAX));
    }

    #[test]
    fn a_finite_budget_accounts_normally_and_saturates_after_the_hard_cap() {
        let budget = RunBudget::new(Some(50), 100);

        assert_eq!(budget.total(), Some(50));
        assert_eq!(budget.spent(120), 20);
        assert_eq!(budget.remaining(120), Some(30));
        assert_eq!(budget.remaining(150), Some(0));
        assert!(budget.exhausted(150));
        assert_eq!(budget.remaining(200), Some(0));
        assert!(budget.exhausted(200));
    }

    #[test]
    fn a_counter_reset_below_the_baseline_is_defensively_treated_as_zero_spend() {
        let budget = RunBudget::new(Some(50), 100);

        assert_eq!(budget.spent(75), 0);
        assert_eq!(budget.remaining(75), Some(50));
        assert!(!budget.exhausted(75));
    }

    #[test]
    fn budgets_with_different_baselines_account_only_from_their_own_session_counters() {
        let first_session = RunBudget::new(Some(100), 1_000);
        let second_session = RunBudget::new(Some(100), 50_000);

        assert_eq!(first_session.spent(1_025), 25);
        assert_eq!(first_session.remaining(1_025), Some(75));
        assert_eq!(second_session.spent(50_090), 90);
        assert_eq!(second_session.remaining(50_090), Some(10));

        // Exhausting one session's budget does not affect another session's counter.
        assert!(second_session.exhausted(50_100));
        assert!(!first_session.exhausted(1_025));
        assert_eq!(first_session.remaining(1_025), Some(75));
    }

    #[test]
    fn a_zero_total_budget_is_exhausted_at_its_baseline() {
        let budget = RunBudget::new(Some(0), 12_345);

        assert_eq!(budget.spent(12_345), 0);
        assert_eq!(budget.remaining(12_345), Some(0));
        assert!(budget.exhausted(12_345));
    }
}
