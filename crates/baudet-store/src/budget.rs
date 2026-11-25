
use crate::window::ContextStrategy;
use baudet_core::Msg;

pub const INPUT_OVERHEAD_TOKENS: u32 = 2_000;
pub const MIN_CONTEXT_CEILING_TOKENS: u32 = 1_024;
pub const MAX_OVERFLOW_RETRIES: u32 = 2;
pub const OVERFLOW_SHRINK_STEP: u32 = 60;
pub const WINDOW_MARGIN_TOKENS: u32 = 4_096;
pub const FOLD_WINDOW_PERCENT: u32 = 75;
pub const ROLLING_WINDOW_PERCENT: u32 = 90;

pub const CONTEXT_LOSS_NOTE: &str = "Earlier turns were dropped from this request; a summary may \
     stand in for them. Treat that summary as background only, never as a live plan: do not reopen \
     or re-announce a task you cannot confirm in the recent messages above. If a task you began \
     earlier is no longer visible, say you have lost the thread and ask, instead of silently acting \
     on it.";

pub fn instructions(base: &str, summary: Option<&str>, truncated: bool) -> String {
    let mut out = match summary {
        None => base.to_string(),
        Some(summary) => format!(
            "{base}\n\nSummary of the earlier conversation:\n{summary}\n\n+             Use this summary with the recent messages to continue any unfinished user request. +             Follow the latest user instructions and account for work already completed."
        ),
    };
    if truncated {
        out.push_str("\n\n");
        out.push_str(CONTEXT_LOSS_NOTE);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLimits {
    pub strategy: ContextStrategy,
    pub budget_tokens: u32,
    pub max_output_tokens: u32,
    pub learned_context_window: u32,
    pub learned_context_key: String,
    pub model: String,
    pub base_url: String,
    pub keep_recent_messages: usize,
}

impl Default for ContextLimits {
    fn default() -> ContextLimits {
        ContextLimits {
            strategy: ContextStrategy::default(),
            budget_tokens: 0,
            max_output_tokens: 0,
            learned_context_window: 0,
            learned_context_key: String::new(),
            model: String::new(),
            base_url: String::new(),
            keep_recent_messages: 15,
        }
    }
}

impl ContextLimits {
    pub fn for_model(base_url: &str, model: &str) -> ContextLimits {
        ContextLimits {
            model: model.to_string(),
            base_url: base_url.to_string(),
            learned_context_key: window_key(base_url, model),
            ..Default::default()
        }
    }

    pub fn key(&self) -> &str {
        &self.learned_context_key
    }

    pub fn identity_key(&self) -> String {
        window_key(&self.base_url, &self.model)
    }

    // A learned window belongs to one endpoint and model.
    pub fn context_window(&self) -> Option<u32> {
        if self.learned_context_window > 0 && self.learned_context_key == self.identity_key() {
            return Some(self.learned_context_window);
        }
        baudet_core::known_context_window(&self.model)
    }

    pub fn typed_budget(&self) -> Option<u32> {
        (self.budget_tokens > 0).then_some(self.budget_tokens)
    }

    pub fn capped_by_budget(&self, limit: u32) -> u32 {
        self.typed_budget().map_or(limit, |budget| budget.min(limit))
    }

    pub fn hard_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let inside = window.saturating_sub(WINDOW_MARGIN_TOKENS).max(MIN_CONTEXT_CEILING_TOKENS);
        self.capped_by_budget(inside)
    }

    pub fn fold_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let early = (u64::from(window) * u64::from(FOLD_WINDOW_PERCENT) / 100).max(u64::from(MIN_CONTEXT_CEILING_TOKENS)) as u32;
        self.capped_by_budget(early).min(self.hard_ceiling())
    }

    pub fn rolling_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let fill = (u64::from(window) * u64::from(ROLLING_WINDOW_PERCENT) / 100).max(u64::from(MIN_CONTEXT_CEILING_TOKENS)) as u32;
        self.capped_by_budget(fill).min(self.hard_ceiling())
    }

    pub fn trim_ceiling(&self) -> u32 {
        if self.strategy == ContextStrategy::RollingWindow {
            self.rolling_ceiling()
        } else {
            self.fold_ceiling()
        }
    }

    pub fn trim_budget(&self, shrink_pct: u32) -> usize {
        (u64::from(self.trim_ceiling()) * u64::from(shrink_pct) / 100) as usize
    }

    pub fn context_ceiling(&self) -> u64 {
        let budget = self.hard_ceiling();
        if budget == 0 {
            return 0;
        }
        let reserve = self.max_output_tokens.saturating_add(INPUT_OVERHEAD_TOKENS);
        u64::from(budget.saturating_sub(reserve).max(MIN_CONTEXT_CEILING_TOKENS))
    }

    pub fn over(&self, estimate: u64, ceiling: u32) -> bool {
        if ceiling == 0 {
            return false;
        }
        estimate + u64::from(self.max_output_tokens) > u64::from(ceiling)
    }

    pub fn context_guard(&self, history: &[Msg]) -> bool {
        self.strategy == ContextStrategy::StopAtLimit
            && self.over(crate::history::estimate_tokens(history) as u64, self.hard_ceiling())
    }

    pub fn context_reading(&self, history: &[Msg]) -> ContextReading {
        let trimming = self.strategy.trims_history();
        let budget = self.fold_ceiling();
        let used = if trimming {
            crate::window::bound(history, self.strategy, self.trim_budget(100), self.keep_recent_messages)
        } else {
            history.to_vec()
        };
        let used = crate::history::estimate_tokens(&used) as u64;
        let percent = if budget == 0 { 0 } else { used * 100 / u64::from(budget) };
        ContextReading { used, budget, percent, trimming }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReading {
    pub used: u64,
    pub budget: u32,
    pub percent: u64,
    pub trimming: bool,
}

pub fn window_key(base_url: &str, model: &str) -> String {
    format!("{base_url}|{model}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::Msg;

    fn known() -> ContextLimits {
        ContextLimits::for_model("http://127.0.0.1:8000/v1", "gemma-3-4b-it")
    }

    #[test]
    fn a_known_model_gets_its_window_and_two_ceilings_inside_it() {
        let limits = known();
        assert_eq!(limits.context_window(), Some(32_768));
        assert_eq!(limits.hard_ceiling(), 28_672);
        assert_eq!(limits.fold_ceiling(), 24_576);
        assert_eq!(limits.rolling_ceiling(), limits.hard_ceiling());
        assert!(limits.fold_ceiling() < limits.hard_ceiling());
    }

    #[test]
    fn an_unknown_model_with_no_budget_has_no_opinion() {
        let limits = ContextLimits::for_model("http://127.0.0.1:8000/v1", "gpt-5");
        assert_eq!(limits.context_window(), None);
        assert_eq!(limits.hard_ceiling(), 0);
        assert_eq!(limits.fold_ceiling(), 0);
        assert_eq!(limits.trim_budget(100), 0);
        assert_eq!(limits.context_ceiling(), 0);
        let history = vec![Msg::user("x".repeat(400_000))];
        assert!(!limits.context_guard(&history));
    }

    #[test]
    fn a_typed_budget_lowers_ceilings_and_cannot_raise_one() {
        let mut limits = known();
        limits.budget_tokens = 8_000;
        assert_eq!(limits.hard_ceiling(), 8_000, "het getal van de persoon ligt onder het venster");
        assert_eq!(limits.fold_ceiling(), 8_000, "het vroegere plafond krimpt mee met het budget");
        limits.budget_tokens = 1_000_000;
        assert_eq!(limits.hard_ceiling(), 28_672, "een groot getal duwt het verzoek niet voorbij het model");
    }

    #[test]
    fn a_learned_window_wins_only_under_the_key_it_came_from() {
        let mut limits = known();
        limits.learned_context_window = 4_096;
        assert_eq!(limits.context_window(), Some(4_096), "een weigering is bewijs over deze server");
        limits.learned_context_key = window_key("http://127.0.0.1:8000/v1", "another-model");
        assert_eq!(limits.context_window(), Some(32_768), "een andere model makes it stale");
    }

    #[test]
    fn the_loop_ceiling_leaves_room_for_the_reply_and_the_protocol() {
        let mut limits = known();
        limits.max_output_tokens = 4_000;
        assert_eq!(limits.context_ceiling(), 22_672);
        limits.budget_tokens = 3_000;
        assert_eq!(limits.context_ceiling(), u64::from(MIN_CONTEXT_CEILING_TOKENS));
    }

    #[test]
    fn only_stop_at_limit_refuses_and_it_refuses_before_the_request() {
        let history: Vec<Msg> = (0..600).map(|i| Msg::user(format!("vraag {i}"))).collect();
        let mut limits = known();
        limits.learned_context_window = 12_288;
        limits.budget_tokens = 1_500;
        assert_eq!(limits.hard_ceiling(), 1_500);
        for strategy in [ContextStrategy::RecentMessages, ContextStrategy::RollingWindow] {
            limits.strategy = strategy;
            assert!(!limits.context_guard(&history), "{strategy:?} knipt in plaats van te weigeren");
        }
        limits.strategy = ContextStrategy::StopAtLimit;
        assert!(limits.context_guard(&history), "daar moet de beurt blijven staan");
    }

    #[test]
    fn a_retry_reaches_for_a_smaller_window_every_time() {
        let limits = known();
        let first = limits.trim_budget(100);
        let second = limits.trim_budget(100 * OVERFLOW_SHRINK_STEP / 100);
        let third = limits.trim_budget(100 * OVERFLOW_SHRINK_STEP / 100 * OVERFLOW_SHRINK_STEP / 100);
        assert_eq!((first, second, third), (24_576, 14_745, 8_847));
        assert!(third > 0, "elke poging stuurt nog iets");
    }

    #[test]
    fn rolling_reaches_higher_than_the_fold_and_both_stay_behind_the_refusal() {
        let limits = known();
        assert!(limits.rolling_ceiling() > limits.fold_ceiling());
        assert!(limits.rolling_ceiling() <= limits.hard_ceiling());
        assert_eq!(limits.trim_ceiling(), limits.fold_ceiling());
        let rolling = ContextLimits { strategy: ContextStrategy::RollingWindow, ..known() };
        assert_eq!(rolling.trim_ceiling(), rolling.rolling_ceiling());
    }

    #[test]
    fn the_reading_shows_the_window_when_a_trimming_strategy_is_in_force() {
        let history: Vec<Msg> =
            (0..40).flat_map(|i| vec![Msg::user(format!("vraag {i}")), Msg::assistant("antwoord".repeat(20))]).collect();
        let whole = crate::history::estimate_tokens(&history) as u64;
        let limits = known();
        let reading = limits.context_reading(&history);
        assert!(reading.used < whole, "onder een knippende strategie toont de lijn het verzoek");
        assert_eq!(reading.budget, limits.fold_ceiling());
        assert!(reading.trimming);
        assert!(reading.percent > 0 && reading.percent <= 100);

        let stopped = ContextLimits { strategy: ContextStrategy::StopAtLimit, ..known() };
        let reading = stopped.context_reading(&history);
        assert_eq!(reading.used, whole, "StopAtLimit toont het hele gesprek en wijst naar Compact");
        assert!(!reading.trimming);
    }

    #[test]
    fn the_loss_note_only_reports_a_cut_window() {
        let base = "Wees nauwkeurig.";
        assert_eq!(instructions(base, None, false), base);
        assert!(!instructions(base, None, true).contains("Summary"), "niets samengevat, dus geen kop");
        let with_summary = instructions(base, Some("er was een plan"), false);
        assert!(with_summary.starts_with(&format!("{base}\n\nSummary of the earlier conversation:\ner was een plan")));
        assert!(!with_summary.contains(CONTEXT_LOSS_NOTE));
        for text in [instructions(base, None, true), instructions(base, Some("s"), true)] {
            assert!(text.ends_with(CONTEXT_LOSS_NOTE), "{text}");
        }
    }

    #[test]
    fn a_small_window_still_leaves_one_usable_request() {
        let limits = ContextLimits::for_model("http://127.0.0.1:8000/v1", "phi-3-mini");
        assert_eq!(limits.context_window(), Some(4_096));
        assert_eq!(limits.hard_ceiling(), MIN_CONTEXT_CEILING_TOKENS);
        assert_eq!(limits.fold_ceiling(), MIN_CONTEXT_CEILING_TOKENS);
    }

    #[test]
    fn the_key_names_one_server_and_one_model() {
        assert_eq!(window_key("http://a:8000/v1", "m"), "http://a:8000/v1|m");
        let limits = ContextLimits::for_model("http://a:8000/v1", "m");
        assert_eq!(limits.key(), "http://a:8000/v1|m");
    }
}
