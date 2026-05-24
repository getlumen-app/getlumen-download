use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Tun,
    Proxy,
    WbStream,
}

impl TransportKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tun" => Some(Self::Tun),
            "proxy" => Some(Self::Proxy),
            "wbstream" => Some(Self::WbStream),
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
    SwitchToWbStream,
}

impl MonitorAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stay => "stay",
            Self::SwitchToWbStream => "switch_to_wbstream",
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

pub fn decide_action(
    transport: TransportKind,
    consecutive_failures: u8,
    policy: HealthPolicy,
) -> MonitorAction {
    if transport == TransportKind::Tun
        && consecutive_failures >= policy.consecutive_failures_to_switch
    {
        MonitorAction::SwitchToWbStream
    } else {
        MonitorAction::Stay
    }
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
    fn tun_switches_to_wbstream_after_threshold() {
        assert_eq!(
            decide_action(TransportKind::Tun, 2, HealthPolicy::default()),
            MonitorAction::SwitchToWbStream
        );
    }

    #[test]
    fn tun_does_not_switch_on_single_failure() {
        assert_eq!(
            decide_action(TransportKind::Tun, 1, HealthPolicy::default()),
            MonitorAction::Stay
        );
    }

    #[test]
    fn wbstream_does_not_switch_to_itself() {
        assert_eq!(
            decide_action(TransportKind::WbStream, 3, HealthPolicy::default()),
            MonitorAction::Stay
        );
    }

    #[test]
    fn proxy_mode_does_not_enter_tun_fallback() {
        assert_eq!(
            decide_action(TransportKind::Proxy, 3, HealthPolicy::default()),
            MonitorAction::Stay
        );
    }
}
