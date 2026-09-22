use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Tun,
    Proxy,
}

impl TransportKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tun" => Some(Self::Tun),
            "proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeOutcome {
    Healthy,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorAction {
    Stay,
    ActivateFallback,
}

impl MonitorAction {
    pub fn as_str(self) -> &'static str {
        match self {
            MonitorAction::Stay => "stay",
            MonitorAction::ActivateFallback => "activate_fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthPolicy {
    pub consecutive_failures_to_switch: u8,
}

#[derive(Debug, Serialize)]
pub struct HealthDecision {
    pub consecutive_failures: u8,
    pub action: &'static str,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            consecutive_failures_to_switch: 2,
        }
    }
}

pub fn next_failure_count(previous: u8, outcome: ProbeOutcome) -> u8 {
    match outcome {
        ProbeOutcome::Healthy => 0,
        ProbeOutcome::Failed => previous.saturating_add(1),
    }
}

/// Telemost fallback (2026-09-22): when a verified Telemost manifest is cached
/// and the TUN transport keeps failing, ask the frontend to start the local
/// joiner sidecars. The `whitelist-auto` urltest then picks up `telemost-local`
/// and hands the route back to a foreign exit once one probes healthy again.
pub fn decide_action(
    transport: TransportKind,
    consecutive_failures: u8,
    policy: HealthPolicy,
    fallback_available: bool,
) -> MonitorAction {
    if transport == TransportKind::Tun
        && fallback_available
        && consecutive_failures >= policy.consecutive_failures_to_switch
    {
        return MonitorAction::ActivateFallback;
    }
    MonitorAction::Stay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_probe_resets_failure_count() {
        assert_eq!(next_failure_count(2, ProbeOutcome::Healthy), 0);
    }

    #[test]
    fn failed_probe_increments_failure_count() {
        assert_eq!(next_failure_count(1, ProbeOutcome::Failed), 2);
    }

    #[test]
    fn tun_switches_after_threshold_when_fallback_available() {
        assert_eq!(
            decide_action(TransportKind::Tun, 2, HealthPolicy::default(), true),
            MonitorAction::ActivateFallback
        );
    }

    #[test]
    fn tun_stays_after_threshold_when_no_fallback() {
        assert_eq!(
            decide_action(TransportKind::Tun, 2, HealthPolicy::default(), false),
            MonitorAction::Stay
        );
    }

    #[test]
    fn tun_does_not_switch_on_single_failure() {
        assert_eq!(
            decide_action(TransportKind::Tun, 1, HealthPolicy::default(), true),
            MonitorAction::Stay
        );
    }

    #[test]
    fn proxy_mode_does_not_switch() {
        assert_eq!(
            decide_action(TransportKind::Proxy, 3, HealthPolicy::default(), true),
            MonitorAction::Stay
        );
    }
}
