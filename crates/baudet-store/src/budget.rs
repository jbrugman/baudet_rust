//! What one conversation may cost, derived from the model rather than from a guess.
//!
//! Ported from the ceiling arithmetic of `dev.baudet.app.AppState` (`contextWindowTokens`,
//! `hardCeilingTokens`, `foldCeilingTokens`, `rollingCeilingTokens`, `trimBudgetTokens`,
//! `contextCeilingTokens`, `contextGuard`, `learnContextWindow`). [`crate::window`] decides how a
//! window is cut; this module decides how large it is allowed to be. The two stay apart because
//! they change for different reasons: the cut is a shape, the size is a claim about a model.
//!
//! Three rules carry the whole design, and all three came out of a backend that refused a turn:
//!
//!  * a refusal is evidence. A number the backend refused against beats the built-in table,
//!    because it describes this exact server and model while the table describes a family;
//!  * a typed budget is a cap, never a floor. It can lower a ceiling but cannot push a request
//!    past the model, so a small value keeps a conversation cheap on purpose;
//!  * nothing is invented. Where no window is known and nothing was typed, the ceiling is 0,
//!    which every reader here reads as "no opinion" rather than as "send nothing".

use crate::window::ContextStrategy;
use baudet_core::Msg;

/// Fixed allowance for the tool definitions plus the protocol text a tool turn prepends.
pub const INPUT_OVERHEAD_TOKENS: u32 = 2_000;
/// Floor so a small budget still lets the loop make at least one modest request.
pub const MIN_CONTEXT_CEILING_TOKENS: u32 = 1_024;
/// Refusals to absorb before giving up and reporting: each retry aims at a smaller window.
pub const MAX_OVERFLOW_RETRIES: u32 = 2;
/// How much of the previous window a retry reaches for, as a percentage — a shrinking bound.
pub const OVERFLOW_SHRINK_STEP: u32 = 60;
/// Room held back between the model's stated window and what we treat as usable. A window is the
/// total the backend counts, and a request landing exactly on it is still refused, so the ceiling
/// has to sit inside it by more than the reply alone.
pub const WINDOW_MARGIN_TOKENS: u32 = 4_096;
/// Where folding starts, as a percentage of the window; the refusal happens at its edge.
pub const FOLD_WINDOW_PERCENT: u32 = 75;
/// How much of the window the rolling strategy fills toward, seeking the model's maximum.
pub const ROLLING_WINDOW_PERCENT: u32 = 90;

/// Handed to the model whenever earlier turns were dropped or summarized, so it reports losing the
/// thread instead of quietly reopening a task that is no longer in its context. Model-facing, so
/// English whatever the interface language is.
///
/// `AppState.kt:1184`, verbatim: a model-facing string is part of the measured behaviour, so a
/// reworded version is a different experiment even when it reads better.
pub const CONTEXT_LOSS_NOTE: &str = "Earlier turns were dropped from this request; a summary may \
     stand in for them. Treat that summary as background only, never as a live plan: do not reopen \
     or re-announce a task you cannot confirm in the recent messages above. If a task you began \
     earlier is no longer visible, say you have lost the thread and ask, instead of silently acting \
     on it.";

/// The whole conversation, with the summary and the loss note folded into the instructions.
///
/// `instructionsFor` in `AppState.kt:753`. The summary rides here rather than as a replayed system
/// message because the request builder drops system messages, which would silently lose it after a
/// compaction. `truncated` says the head of the conversation did not make it into this request —
/// a fold left only a summary, or trimming rolled older turns off the window — and the note is what
/// stops a weak local model re-announcing a stale plan from that summary.
pub fn instructions(base: &str, summary: Option<&str>, truncated: bool) -> String {
    let mut out = match summary {
        None => base.to_string(),
        Some(summary) => format!("{base}\n\nSummary of the earlier conversation:\n{summary}"),
    };
    if summary.is_some() || truncated {
        out.push_str("\n\n");
        out.push_str(CONTEXT_LOSS_NOTE);
    }
    out
}

/// The measurements and the choices the ceilings are computed from.
///
/// A struct rather than six arguments because every caller needs all of them and a positional list
/// of five `u32`s is where a `budget` and a `max_output` get quietly swapped. Field names follow the
/// settings they read, and the defaults are the settings' defaults, so a caller with no opinion
/// about context — a test, or a build whose settings panel does not offer the subject yet — gets
/// Kotlin's behaviour without having to know what the numbers mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLimits {
    /// The strategy in force, which decides whether a ceiling refuses or trims.
    pub strategy: ContextStrategy,
    /// What Settings holds, in tokens. `0` means the app derives it from the window.
    pub budget_tokens: u32,
    /// The ceiling on the reply, which has to leave room inside the window. `0` means the endpoint
    /// decides, and is read as "reserve nothing", because reserving an invented number would shrink
    /// a window nobody asked to shrink.
    pub max_output_tokens: u32,
    /// The window a refusal taught us, or `0` when never learned.
    pub learned_context_window: u32,
    /// The `base_url|model` the number above was learned under.
    pub learned_context_key: String,
    /// The model the ceilings are about, which the table is consulted through. Kept beside the key
    /// rather than parsed out of it: the key is a stored identity and this is the live question.
    pub model: String,
    /// The endpoint the turn goes to, which is the other half of the identity a learned number
    /// belongs to.
    pub base_url: String,
    /// How many turns before the current one `RecentMessages` sends back.
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
            // `AppConfig.contextKeepRecentMessages`, which only `RecentMessages` ever reads.
            keep_recent_messages: 15,
        }
    }
}

impl ContextLimits {
    /// Limits for one server and model, with nothing learned and nothing typed.
    pub fn for_model(base_url: &str, model: &str) -> ContextLimits {
        ContextLimits {
            model: model.to_string(),
            base_url: base_url.to_string(),
            learned_context_key: window_key(base_url, model),
            ..Default::default()
        }
    }

    /// The identity a learned window belongs to: a number learned off another server or model is
    /// stale, so it is keyed rather than trusted outright.
    pub fn key(&self) -> &str {
        &self.learned_context_key
    }

    /// The key this conversation would store a learned window under, right now. A stored number is
    /// evidence only while it matches, which is what makes a server or model switch self-correcting
    /// without anybody clearing a field.
    pub fn identity_key(&self) -> String {
        window_key(&self.base_url, &self.model)
    }

    /// The window this model really has, or `None` when nobody told us.
    ///
    /// A learned number wins, but only under the key it was learned on, so switching server or
    /// model drops it back to the table instead of guarding against a window that belongs to a
    /// different model.
    pub fn context_window(&self) -> Option<u32> {
        if self.learned_context_window > 0 && self.learned_context_key == self.identity_key() {
            return Some(self.learned_context_window);
        }
        baudet_core::known_context_window(&self.model)
    }

    /// What Settings holds, or `None` when it is 0, which means "the app decides".
    pub fn typed_budget(&self) -> Option<u32> {
        (self.budget_tokens > 0).then_some(self.budget_tokens)
    }

    /// A typed budget can only lower a ceiling, never raise it: it is a cap, not a floor.
    pub fn capped_by_budget(&self, limit: u32) -> u32 {
        self.typed_budget().map_or(limit, |budget| budget.min(limit))
    }

    /// The last ceiling: at or above this the turn is refused, because this is where the backend
    /// would refuse it.
    ///
    /// The model's window less a margin, so a request cannot land exactly on the window and still
    /// be turned down. Where no window is known this is simply what Settings holds, and 0 there
    /// means off: nothing better may be invented.
    pub fn hard_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let inside = window.saturating_sub(WINDOW_MARGIN_TOKENS).max(MIN_CONTEXT_CEILING_TOKENS);
        self.capped_by_budget(inside)
    }

    /// The early ceiling, where folding starts.
    ///
    /// It sits well inside the window on purpose: the conversation is measured as characters over
    /// three, and dense JSON — which is what a tool transcript mostly is — costs more tokens per
    /// character than that estimate assumes, so a ceiling near the window is passed in reality
    /// before it is passed on paper. Folding at three quarters keeps that error inside the margin.
    pub fn fold_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let early = (u64::from(window) * u64::from(FOLD_WINDOW_PERCENT) / 100).max(u64::from(MIN_CONTEXT_CEILING_TOKENS)) as u32;
        // Never later than the refusal it exists to prevent.
        self.capped_by_budget(early).min(self.hard_ceiling())
    }

    /// The ceiling the rolling window fills toward.
    ///
    /// Where the automatic fold stops at three quarters to leave slack for an estimate that runs
    /// loose on dense tool output, the rolling strategy reaches for a larger slice of the window. It
    /// still sits inside the refusal edge, is lowered by a typed budget, and stays behind the
    /// oversized-turn clamp and the shrink-on-refusal retry that absorb any overshoot against a
    /// backend that counts differently than this estimate.
    pub fn rolling_ceiling(&self) -> u32 {
        let Some(window) = self.context_window() else {
            return self.typed_budget().unwrap_or(0);
        };
        let fill = (u64::from(window) * u64::from(ROLLING_WINDOW_PERCENT) / 100).max(u64::from(MIN_CONTEXT_CEILING_TOKENS)) as u32;
        self.capped_by_budget(fill).min(self.hard_ceiling())
    }

    /// The ceiling this strategy trims toward: the rolling one fills higher, keep-last-N keeps the
    /// conservative fold because its turn count bounds it anyway.
    pub fn trim_ceiling(&self) -> u32 {
        if self.strategy == ContextStrategy::RollingWindow {
            self.rolling_ceiling()
        } else {
            self.fold_ceiling()
        }
    }

    /// The token budget the sent window is trimmed to, `shrink_pct`/100 of the strategy's ceiling so
    /// a retry reaches for a smaller window than the one the backend just refused.
    ///
    /// `0` means no window is known, which [`crate::window::trim`] reads as leave the conversation
    /// alone. A caller never passes 100 by hand after a refusal: `shrink_pct` is multiplied down by
    /// [`OVERFLOW_SHRINK_STEP`] per retry, exactly as in `AppState.kt:836`.
    pub fn trim_budget(&self, shrink_pct: u32) -> usize {
        // U64 for the multiply: a 1M-token budget times 100 would overflow a u32 on a 32-bit
        // target, and a wrapped budget is a window nobody chose.
        (u64::from(self.trim_ceiling()) * u64::from(shrink_pct) / 100) as usize
    }

    /// The token estimate a chained tool turn may reach before its next request is doomed.
    ///
    /// Under a server session the backend replays and grows its own context, so the client cannot
    /// bound the request by trimming what it sends; the loop watches this ceiling and stops cleanly
    /// instead of firing a call the backend is certain to refuse. It mirrors [`Self::context_guard`]:
    /// the budget less the reply it must leave room for, less a fixed allowance for the tool
    /// definitions and protocol the loop prepends. 0 when no budget is configured, which disables
    /// the ceiling entirely.
    pub fn context_ceiling(&self) -> u64 {
        let budget = self.hard_ceiling();
        if budget == 0 {
            return 0;
        }
        let reserve = self.max_output_tokens.saturating_add(INPUT_OVERHEAD_TOKENS);
        u64::from(budget.saturating_sub(reserve).max(MIN_CONTEXT_CEILING_TOKENS))
    }

    /// Whether this conversation, plus the reply it must leave room for, reaches `ceiling`.
    ///
    /// A ceiling of 0 is no ceiling: an oversized conversation is not refused because nobody told us
    /// how large the model is.
    pub fn over(&self, estimate: u64, ceiling: u32) -> bool {
        if ceiling == 0 {
            return false;
        }
        estimate + u64::from(self.max_output_tokens) > u64::from(ceiling)
    }

    /// Why this turn must not go out as it stands, or `None` when it may.
    ///
    /// This refuses only under [`ContextStrategy::StopAtLimit`]: the trimming strategies bound the
    /// window instead of refusing, so they never reach a refusal here — that silence is the whole
    /// point of making trimming the default. Where it does refuse, it does so before the request
    /// rather than after the backend's answer, so nothing is billed and the text the person typed
    /// stays in the input box. It returns no text of its own: the wording belongs to the interface
    /// (item H), and a count never belongs in an error the person has to read.
    ///
    /// `history` is the conversation as stored: under `StopAtLimit` nothing is trimmed, so the whole
    /// transcript is what would go out and is what the estimate has to describe.
    pub fn context_guard(&self, history: &[Msg]) -> bool {
        self.strategy == ContextStrategy::StopAtLimit
            && self.over(crate::history::estimate_tokens(history) as u64, self.hard_ceiling())
    }

    /// The token estimate the status line and the guard compare against: the whole transcript under
    /// `StopAtLimit`, and the window that will actually be sent under a trimming strategy.
    ///
    /// `AppState.kt:1150` makes that distinction because showing the whole transcript while a window
    /// is being trimmed alarms without cause — the transcript keeps growing and that is harmless.
    /// `StopAtLimit` shows the whole conversation and keeps nudging toward the manual Compact button,
    /// which is the only way forward there.
    pub fn context_reading(&self, history: &[Msg]) -> ContextReading {
        let trimming = self.strategy != ContextStrategy::StopAtLimit;
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

/// What the status line shows: the size of the window that goes out, against the ceiling it is
/// bounded by. Kept as numbers rather than text, because text is item H's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReading {
    /// What the request will cost, estimated.
    pub used: u64,
    /// The early ceiling it is measured against, or 0 when no window is known.
    pub budget: u32,
    /// Whole percent of `budget`; 0 when there is no budget to be a percent of.
    pub percent: u64,
    /// Whether a trimming strategy is in force, which is what decides whether "compact" is a
    /// suggestion the app may act on or the only way forward.
    pub trimming: bool,
}

/// The identity a learned window is stored under. Two strings, not one opaque hash: the person may
/// have three servers on one machine and a number has to be attributable to one of them.
pub fn window_key(base_url: &str, model: &str) -> String {
    format!("{base_url}|{model}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::Msg;

    /// `gemma-3` is in the table at 32 768, so these limits describe a known family.
    fn known() -> ContextLimits {
        ContextLimits::for_model("http://127.0.0.1:8000/v1", "gemma-3-4b-it")
    }

    #[test]
    fn a_known_model_gets_its_window_and_two_ceilings_inside_it() {
        let limits = known();
        assert_eq!(limits.context_window(), Some(32_768));
        // 32 768 - 4 096 = 28 672 and 32 768 * 3/4 = 24 576, both above the floor.
        assert_eq!(limits.hard_ceiling(), 28_672);
        assert_eq!(limits.fold_ceiling(), 24_576);
        // 32 768 * 90 % = 29 491, which is past the refusal, so the refusal wins.
        assert_eq!(limits.rolling_ceiling(), limits.hard_ceiling());
        // The early ceiling never sits behind the refusal it exists to prevent.
        assert!(limits.fold_ceiling() < limits.hard_ceiling());
    }

    #[test]
    fn an_unknown_model_with_no_budget_has_no_opinion() {
        let limits = ContextLimits::for_model("http://127.0.0.1:8000/v1", "gpt-5");
        assert_eq!(limits.context_window(), None);
        // 0 is off, not empty: nothing better may be invented about a model nobody described.
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
        // Het budget ligt ónder de 24 576 van het venster, dus het wint: een `min` van twee constanten
        // zou hier niets testen, daarom staat het lagere getal er expliciet.
        assert_eq!(limits.fold_ceiling(), 8_000, "het vroegere plafond krimpt mee met het budget");
        // And above the window it changes nothing: a cap is not a floor.
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
        // 28 672 - (4 000 + 2 000) = 22 672.
        assert_eq!(limits.context_ceiling(), 22_672);
        // A budget so small that the reserve eats it still lets one modest request out.
        limits.budget_tokens = 3_000;
        assert_eq!(limits.context_ceiling(), u64::from(MIN_CONTEXT_CEILING_TOKENS));
    }

    #[test]
    fn only_stop_at_limit_refuses_and_it_refuses_before_the_request() {
        // 600 turns van vijf tekens kosten 1 016 tokens per turn, dus ~203 000 tokens: ruwwelig
        // boven welk plafond van deze test ook.
        let history: Vec<Msg> = (0..600).map(|i| Msg::user(format!("vraag {i}"))).collect();
        let mut limits = known();
        // Een geleerd venster van 12 288 met een typed budget van 1 500: het budget wint.
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
        // `AppState.kt:836` shrinks the percentage itself, so the third try is 36 % of the ceiling.
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
    fn the_loss_note_rides_with_a_summary_and_with_a_cut_window() {
        let base = "Wees nauwkeurig.";
        // Nothing folded and nothing cut: the instructions say exactly what was set.
        assert_eq!(instructions(base, None, false), base);
        assert!(!instructions(base, None, true).contains("Summary"), "niets samengevat, dus geen kop");
        let with_summary = instructions(base, Some("er was een plan"), false);
        assert!(with_summary.starts_with(&format!("{base}\n\nSummary of the earlier conversation:\ner was een plan")));
        // Both cases end on the note, which is what stops a model reopening finished work.
        for text in [with_summary, instructions(base, None, true), instructions(base, Some("s"), true)] {
            assert!(text.ends_with(CONTEXT_LOSS_NOTE), "{text}");
        }
    }

    #[test]
    fn a_small_window_still_leaves_one_usable_request() {
        // `phi` is a 4k family, so window - margin would go under the floor without it.
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
