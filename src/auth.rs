//! GitHub OAuth device flow as a resumable state machine.
//!
//! The flow is ticked by the app's frame loop, never by a blocking `sleep`. Two
//! pacing rules from RFC 8628 matter and are both enforced here:
//!
//! * the interval is measured from the *previous response*, not the previous
//!   request, so a slow round trip can never turn into an early poll;
//! * `slow_down` raises the interval by 5 s permanently for the rest of the flow.

use serde_json::Value;

pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const SCOPE: &str = "read:project";

const DEFAULT_INTERVAL_MS: u64 = 5_000;
const SLOW_DOWN_STEP_MS: u64 = 5_000;
const DEFAULT_EXPIRES_S: u64 = 900;

/// Validate what the user typed on the setup screen.
///
/// The client id is public, but a typo has to be caught here: GitHub answers a
/// bad client id with a generic error, which is much harder to read than this
/// message. The grammar is GitHub's own: alphanumerics plus `.`, `-` and `_`.
pub fn normalize_client_id(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter the OAuth App client ID.");
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("Letters, digits, dots, dashes and underscores only.");
    }
    if !(8..=64).contains(&trimmed.len()) {
        return Err("A client ID is 8 to 64 characters long.");
    }
    Ok(trimmed.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No attempt in flight; the user has to start one.
    SignedOut,
    /// Device-code request is on the wire.
    RequestingCode,
    /// Code is displayed; we are polling until the user authorises or it expires.
    AwaitingUser,
    SignedIn,
    Denied,
    Expired,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    Authorized(String),
    Pending,
    SlowDown,
    Expired,
    Denied,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct DeviceFlow {
    pub phase: Phase,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub device_code: Option<String>,
    pub interval_ms: u64,
    pub next_poll_at: u64,
    pub deadline: u64,
}

impl Default for DeviceFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceFlow {
    pub fn new() -> Self {
        Self {
            phase: Phase::SignedOut,
            user_code: None,
            verification_uri: None,
            device_code: None,
            interval_ms: DEFAULT_INTERVAL_MS,
            next_poll_at: 0,
            deadline: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Called when a device-code request is handed to the host.
    pub fn begin(&mut self) {
        self.user_code = None;
        self.device_code = None;
        self.interval_ms = DEFAULT_INTERVAL_MS;
        self.deadline = 0;
        self.next_poll_at = 0;
        self.phase = Phase::RequestingCode;
    }

    pub fn is_awaiting(&self) -> bool {
        matches!(self.phase, Phase::AwaitingUser)
    }

    /// Advance the wall clock: expires the code if its window has closed.
    /// Returns true when the phase changed (the caller should repaint).
    pub fn tick(&mut self, now: u64) -> bool {
        if self.is_awaiting() && self.deadline > 0 && now >= self.deadline {
            self.phase = Phase::Expired;
            return true;
        }
        false
    }

    pub fn poll_due(&self, now: u64) -> bool {
        self.is_awaiting()
            && self.device_code.is_some()
            && now >= self.next_poll_at
            && (self.deadline == 0 || now < self.deadline)
    }

    /// Parse `POST /login/device/code`.
    pub fn apply_device_code(&mut self, body: &str, now: u64) -> Result<(), String> {
        let v: Value = serde_json::from_str(body)
            .map_err(|e| format!("device-code response was not JSON: {e}"))?;

        if let Some(err) = v.get("error").and_then(Value::as_str) {
            let detail = v
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or(err);
            self.phase = Phase::Failed(detail.to_owned());
            return Err(detail.to_owned());
        }

        let device_code = v
            .get("device_code")
            .and_then(Value::as_str)
            .ok_or("device_code missing from response")?
            .to_owned();
        let user_code = v
            .get("user_code")
            .and_then(Value::as_str)
            .ok_or("user_code missing from response")?
            .to_owned();
        let verification_uri = v
            .get("verification_uri")
            .and_then(Value::as_str)
            .unwrap_or("https://github.com/login/device")
            .to_owned();
        let expires_in = v
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_EXPIRES_S);
        let interval = v.get("interval").and_then(Value::as_u64).unwrap_or(5);

        self.device_code = Some(device_code);
        self.user_code = Some(user_code);
        self.verification_uri = Some(verification_uri);
        self.interval_ms = interval.max(1) * 1_000;
        self.deadline = now + expires_in * 1_000;
        self.next_poll_at = now + self.interval_ms;
        self.phase = Phase::AwaitingUser;
        Ok(())
    }

    /// Parse one poll of `POST /login/oauth/access_token`.
    pub fn apply_poll(&mut self, body: &str, now: u64) -> Poll {
        let v: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("token response was not JSON: {e}");
                self.phase = Phase::Failed(msg.clone());
                return Poll::Failed(msg);
            }
        };

        if let Some(token) = v.get("access_token").and_then(Value::as_str) {
            self.phase = Phase::SignedIn;
            return Poll::Authorized(token.to_owned());
        }

        let err = v.get("error").and_then(Value::as_str).unwrap_or_default();
        let detail = v
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(err);

        match err {
            "authorization_pending" => {
                self.next_poll_at = now + self.interval_ms;
                Poll::Pending
            }
            "slow_down" => {
                self.interval_ms += SLOW_DOWN_STEP_MS;
                self.next_poll_at = now + self.interval_ms;
                Poll::SlowDown
            }
            "expired_token" => {
                self.phase = Phase::Expired;
                Poll::Expired
            }
            "access_denied" => {
                self.phase = Phase::Denied;
                Poll::Denied
            }
            "" => {
                let msg = "token endpoint returned neither a token nor an error".to_owned();
                self.phase = Phase::Failed(msg.clone());
                Poll::Failed(msg)
            }
            other => {
                let msg = format!("{other}: {detail}");
                self.phase = Phase::Failed(msg.clone());
                Poll::Failed(msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_BODY: &str = r#"{
        "device_code": "3584d83530557fdd",
        "user_code": "BFEE-895F",
        "verification_uri": "https://github.com/login/device",
        "expires_in": 900,
        "interval": 5
    }"#;

    fn awaiting() -> (DeviceFlow, u64) {
        let mut f = DeviceFlow::new();
        f.begin();
        assert_eq!(f.phase, Phase::RequestingCode);
        f.apply_device_code(DEVICE_BODY, 10_000).unwrap();
        (f, 10_000)
    }

    #[test]
    fn device_code_arms_the_first_poll_one_interval_out() {
        let (f, now) = awaiting();
        assert_eq!(f.phase, Phase::AwaitingUser);
        assert_eq!(f.user_code.as_deref(), Some("BFEE-895F"));
        assert_eq!(f.interval_ms, 5_000);
        assert_eq!(f.deadline, now + 900_000);
        assert!(!f.poll_due(now + 4_999));
        assert!(f.poll_due(now + 5_000));
    }

    #[test]
    fn pending_schedules_from_the_response_received_at() {
        let (mut f, _) = awaiting();
        // A round trip that took 3 s must not let the next poll fire at 5 s.
        let responded_at = 13_000;
        assert_eq!(
            f.apply_poll(r#"{"error":"authorization_pending"}"#, responded_at),
            Poll::Pending
        );
        assert!(!f.poll_due(17_999));
        assert!(f.poll_due(18_000));
    }

    #[test]
    fn slow_down_permanently_raises_the_interval() {
        let (mut f, _) = awaiting();
        assert_eq!(f.apply_poll(r#"{"error":"slow_down"}"#, 20_000), Poll::SlowDown);
        assert_eq!(f.interval_ms, 10_000);

        f.apply_poll(r#"{"error":"authorization_pending"}"#, 30_000);
        assert!(!f.poll_due(39_999));
        assert!(f.poll_due(40_000));
    }

    #[test]
    fn authorized_returns_the_token() {
        let (mut f, _) = awaiting();
        let poll = f.apply_poll(
            r#"{"access_token":"gho_abc","token_type":"bearer","scope":"read:project"}"#,
            20_000,
        );
        assert_eq!(poll, Poll::Authorized("gho_abc".to_owned()));
        assert_eq!(f.phase, Phase::SignedIn);
        assert!(!f.poll_due(60_000));
    }

    #[test]
    fn expiry_is_terminal_and_detected_by_the_clock() {
        let (mut f, now) = awaiting();
        assert_eq!(f.apply_poll(r#"{"error":"expired_token"}"#, now), Poll::Expired);
        assert_eq!(f.phase, Phase::Expired);

        let (mut g, now) = awaiting();
        assert!(!g.tick(now + 899_999));
        assert!(g.tick(now + 900_000));
        assert_eq!(g.phase, Phase::Expired);
    }

    #[test]
    fn access_denied_stops_polling() {
        let (mut f, _) = awaiting();
        assert_eq!(f.apply_poll(r#"{"error":"access_denied"}"#, 20_000), Poll::Denied);
        assert_eq!(f.phase, Phase::Denied);
        assert!(!f.poll_due(30_000));
    }

    #[test]
    fn a_client_id_error_surfaces_instead_of_hanging() {
        let mut f = DeviceFlow::new();
        f.begin();
        let err = f
            .apply_device_code(r#"{"error":"unauthorized_client"}"#, 0)
            .unwrap_err();
        assert!(err.contains("unauthorized_client"));
        assert!(matches!(f.phase, Phase::Failed(_)));
    }

    #[test]
    fn a_client_id_is_trimmed_and_checked_locally() {
        assert_eq!(
            normalize_client_id("  Iv1.0123456789abcdef ").as_deref(),
            Ok("Iv1.0123456789abcdef")
        );
        assert!(normalize_client_id("Ov23liAbCdEf").is_ok());

        for bad in ["", "   ", "abc", "Iv1/abc", "has space", &"x".repeat(65)] {
            assert!(normalize_client_id(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_non_json_body_is_reported_not_swallowed() {
        let mut f = DeviceFlow::new();
        f.begin();
        let err = f.apply_device_code("<html>502</html>", 0).unwrap_err();
        assert!(err.contains("not JSON"));
    }
}
