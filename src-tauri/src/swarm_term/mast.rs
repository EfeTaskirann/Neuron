//! MAST failure modes (Cemri et al., "Why Do Multi-Agent LLM Systems
//! Fail?", NeurIPS 2025): the label every `TASK_FAILED` carries, so the
//! reference history gives a failure distribution instead of a count.
//!
//! The coordinator labels the give-ups it writes itself (`TASK_FAILED <id>
//! — [FM-x.y] …`, the list in its persona); the bridge labels the closures
//! it synthesises with fixed modes ([`HATA`], [`STALL`], [`REWORK_LIMIT`],
//! [`TOKEN_OVERRUN`]). The live harness reads the label back from the
//! body ([`failure_mode_of`]) into the run record.

/// One of MAST's fourteen failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureMode {
    // FC1 — specification and system design
    DisobeyTask,
    DisobeyRole,
    StepRepetition,
    HistoryLoss,
    TerminationUnaware,
    // FC2 — inter-agent misalignment
    ConversationReset,
    NoClarification,
    TaskDerailment,
    InfoWithholding,
    IgnoredInput,
    ReasoningActionMismatch,
    // FC3 — task verification
    PrematureTermination,
    IncompleteVerification,
    IncorrectVerification,
}

/// An agent gave the task up (`hata`): the work ended before the goal.
pub const HATA: FailureMode = FailureMode::PrematureTermination;
/// Nobody answered the reminders: the work ended before the goal.
pub const STALL: FailureMode = FailureMode::PrematureTermination;
/// Fix rounds that never converged.
pub const REWORK_LIMIT: FailureMode = FailureMode::StepRepetition;
/// Twice the token budget: turns that went round without landing.
pub const TOKEN_OVERRUN: FailureMode = FailureMode::StepRepetition;

impl FailureMode {
    pub const ALL: [FailureMode; 14] = [
        FailureMode::DisobeyTask,
        FailureMode::DisobeyRole,
        FailureMode::StepRepetition,
        FailureMode::HistoryLoss,
        FailureMode::TerminationUnaware,
        FailureMode::ConversationReset,
        FailureMode::NoClarification,
        FailureMode::TaskDerailment,
        FailureMode::InfoWithholding,
        FailureMode::IgnoredInput,
        FailureMode::ReasoningActionMismatch,
        FailureMode::PrematureTermination,
        FailureMode::IncompleteVerification,
        FailureMode::IncorrectVerification,
    ];

    /// MAST's code, `FM-<category>.<mode>`.
    pub fn code(self) -> &'static str {
        match self {
            FailureMode::DisobeyTask => "FM-1.1",
            FailureMode::DisobeyRole => "FM-1.2",
            FailureMode::StepRepetition => "FM-1.3",
            FailureMode::HistoryLoss => "FM-1.4",
            FailureMode::TerminationUnaware => "FM-1.5",
            FailureMode::ConversationReset => "FM-2.1",
            FailureMode::NoClarification => "FM-2.2",
            FailureMode::TaskDerailment => "FM-2.3",
            FailureMode::InfoWithholding => "FM-2.4",
            FailureMode::IgnoredInput => "FM-2.5",
            FailureMode::ReasoningActionMismatch => "FM-2.6",
            FailureMode::PrematureTermination => "FM-3.1",
            FailureMode::IncompleteVerification => "FM-3.2",
            FailureMode::IncorrectVerification => "FM-3.3",
        }
    }

    /// The short Turkish name the personas use.
    pub fn name(self) -> &'static str {
        match self {
            FailureMode::DisobeyTask => "görev tanımına aykırı",
            FailureMode::DisobeyRole => "rol tanımına aykırı",
            FailureMode::StepRepetition => "adım tekrarı",
            FailureMode::HistoryLoss => "bağlam kaybı",
            FailureMode::TerminationUnaware => "bitiş koşulunu bilmeme",
            FailureMode::ConversationReset => "konuşma sıfırlanması",
            FailureMode::NoClarification => "netleştirme istememe",
            FailureMode::TaskDerailment => "görevden sapma",
            FailureMode::InfoWithholding => "bilgi saklama",
            FailureMode::IgnoredInput => "diğer ajanın girdisini yok sayma",
            FailureMode::ReasoningActionMismatch => "akıl yürütme–eylem uyumsuzluğu",
            FailureMode::PrematureTermination => "erken bitirme",
            FailureMode::IncompleteVerification => "doğrulama yok/eksik",
            FailureMode::IncorrectVerification => "yanlış doğrulama",
        }
    }

    /// The tag a body carries: `[FM-1.3]`.
    pub fn tag(self) -> String {
        format!("[{}]", self.code())
    }

    /// Pure: the mode named by `FM-1.3`, `[fm-1.3]`, `FM 1.3`, `fm1.3`…
    pub fn parse(s: &str) -> Option<Self> {
        let t: String = s
            .trim()
            .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | '`' | '.' | ',' | ';'))
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let rest = t.strip_prefix("fm")?.trim_start_matches(['-', '_', ':']);
        FailureMode::ALL.iter().copied().find(|m| &m.code()[3..] == rest)
    }
}

/// Pure: the first mode a `TASK_FAILED` body names — a `[FM-x.y]` tag or a
/// bare `FM-x.y` in the coordinator's prose.
pub fn failure_mode_of(body: &str) -> Option<FailureMode> {
    body.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')'))
        .find_map(FailureMode::parse)
}

/// One line naming every mode with its code, for a persona bullet.
pub fn persona_list() -> String {
    FailureMode::ALL
        .iter()
        .map(|m| format!("{} {}", m.code(), m.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_unique_code_that_parses_back() {
        assert_eq!(FailureMode::ALL.len(), 14);
        let mut codes: Vec<&str> = FailureMode::ALL.iter().map(|m| m.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), 14);
        for m in FailureMode::ALL {
            assert_eq!(FailureMode::parse(m.code()), Some(m), "{}", m.code());
            assert_eq!(m.tag(), format!("[{}]", m.code()));
            assert!(!m.name().is_empty());
        }
        assert_eq!(FailureMode::parse("[fm-2.3]"), Some(FailureMode::TaskDerailment));
        assert_eq!(FailureMode::parse("FM 3.1"), Some(FailureMode::PrematureTermination));
        assert_eq!(FailureMode::parse("fm1.4"), Some(FailureMode::HistoryLoss));
        assert_eq!(FailureMode::parse("FM-4.1"), None);
        assert_eq!(FailureMode::parse("FM-1.6"), None);
        assert_eq!(FailureMode::parse("kod"), None);
    }

    #[test]
    fn a_task_failed_body_carries_its_mode() {
        assert_eq!(
            failure_mode_of("TASK_FAILED T1 — [FM-3.1] @backend-builder hata bildirdi: cannot build"),
            Some(FailureMode::PrematureTermination)
        );
        // A bare code in the coordinator's prose counts too.
        assert_eq!(
            failure_mode_of("TASK_FAILED T2 — builder wandered off, FM-2.3, redesigned the module"),
            Some(FailureMode::TaskDerailment)
        );
        assert_eq!(failure_mode_of("TASK_FAILED T3 — no label at all"), None);
        assert_eq!(failure_mode_of("TASK_FAILED T4 — [kod] test_x fails"), None);
        // The bridge's own closures have fixed labels.
        assert_eq!(HATA, FailureMode::PrematureTermination);
        assert_eq!(STALL, FailureMode::PrematureTermination);
        assert_eq!(REWORK_LIMIT, FailureMode::StepRepetition);
        assert_eq!(TOKEN_OVERRUN, FailureMode::StepRepetition);
        assert_eq!(HATA.tag(), "[FM-3.1]");
    }

    #[test]
    fn the_persona_list_names_all_fourteen_modes() {
        let list = persona_list();
        for m in FailureMode::ALL {
            assert!(list.contains(m.code()) && list.contains(m.name()), "{}", m.code());
        }
        assert!(!list.contains('\n'), "one line, for a persona bullet");
    }
}
