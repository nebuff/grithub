//! Versioned persistence for `state/runtime.bin`.
//!
//! The first rule is that a schema change must never brick the app: an
//! unreadable or future-versioned blob falls back to defaults but keeps the
//! token, so the user does not have to re-authorise a device.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Board;

pub const SCHEMA_VERSION: u8 = 1;

/// The host documents 256 KiB for the runtime state file.
pub const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Persisted {
    pub v: u8,
    /// The OAuth App the user entered on the setup screen. Optional so that a
    /// state file written before the setup screen existed still decodes.
    #[serde(default)]
    pub client_id: Option<String>,
    pub token: Option<String>,
    pub login: Option<String>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub field_name: Option<String>,
    pub board: Option<Board>,
    pub page: usize,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            v: SCHEMA_VERSION,
            client_id: None,
            token: None,
            login: None,
            project_id: None,
            project_title: None,
            field_name: None,
            board: None,
            page: 0,
        }
    }
}

impl Persisted {
    /// Lenient by design: anything unexpected yields defaults.
    pub fn decode(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::default();
        }

        let value: Value = match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };

        let version = value.get("v").and_then(Value::as_u64).unwrap_or(0) as u8;
        if version == SCHEMA_VERSION {
            if let Ok(parsed) = serde_json::from_value::<Persisted>(value.clone()) {
                return parsed;
            }
        }

        // Migration failure: drop the cache, keep the credentials.
        Self {
            v: SCHEMA_VERSION,
            token: value
                .get("token")
                .and_then(Value::as_str)
                .map(str::to_owned),
            login: value
                .get("login")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Self::default()
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut work = self.clone();
        work.v = SCHEMA_VERSION;

        let mut bytes = serde_json::to_vec(&work).unwrap_or_default();
        if bytes.len() <= MAX_BYTES {
            return bytes;
        }

        // Too big: the cache is the only expendable part.
        work.board = None;
        bytes = serde_json::to_vec(&work).unwrap_or_default();
        if bytes.len() <= MAX_BYTES {
            return bytes;
        }

        // Last resort: keep only the credentials.
        let floor = Persisted {
            token: work.token,
            login: work.login,
            ..Persisted::default()
        };
        serde_json::to_vec(&floor).unwrap_or_default()
    }

    pub fn approx_bytes(&self) -> usize {
        self.encode().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Card, Column};

    fn cached() -> Persisted {
        Persisted {
            token: Some("gho_x".to_owned()),
            login: Some("octocat".to_owned()),
            project_id: Some("P_1".to_owned()),
            project_title: Some("Roadmap".to_owned()),
            field_name: Some("Status".to_owned()),
            board: Some(Board {
                project_id: "P_1".to_owned(),
                project_title: "Roadmap".to_owned(),
                field_name: "Status".to_owned(),
                columns: vec![Column {
                    name: "Todo".to_owned(),
                    cards: vec![Card {
                        id: "i1".to_owned(),
                        title: "Ship it".to_owned(),
                        number: Some(7),
                    }],
                }],
                fetched_at: 99,
                truncated: false,
            }),
            page: 2,
            ..Persisted::default()
        }
    }

    #[test]
    fn round_trips_a_cached_board() {
        let original = cached();
        let decoded = Persisted::decode(&original.encode());
        assert_eq!(decoded.token.as_deref(), Some("gho_x"));
        assert_eq!(decoded.page, 2);
        assert_eq!(
            decoded.board.unwrap().columns[0].cards[0].title,
            "Ship it"
        );
    }

    #[test]
    fn an_unknown_version_keeps_the_token_and_drops_the_cache() {
        let stale = r#"{"v":99,"token":"gho_old","login":"octocat","board":{"whatever":true}}"#;
        let decoded = Persisted::decode(stale.as_bytes());
        assert_eq!(decoded.token.as_deref(), Some("gho_old"));
        assert_eq!(decoded.login.as_deref(), Some("octocat"));
        assert!(decoded.board.is_none());
        assert_eq!(decoded.v, SCHEMA_VERSION);
    }

    #[test]
    fn a_state_file_from_before_the_setup_screen_keeps_its_token() {
        let old = r#"{"v":1,"token":"gho_x","login":"octocat","project_id":null,"project_title":null,"field_name":null,"board":null,"page":0}"#;
        let decoded = Persisted::decode(old.as_bytes());
        assert_eq!(decoded.token.as_deref(), Some("gho_x"));
        assert!(decoded.client_id.is_none());
    }

    #[test]
    fn the_client_id_round_trips_with_the_board() {
        let mut saved = cached();
        saved.client_id = Some("Iv1.0123456789abcdef".to_owned());
        let decoded = Persisted::decode(&saved.encode());
        assert_eq!(decoded.client_id.as_deref(), Some("Iv1.0123456789abcdef"));
        assert!(decoded.board.is_some());
    }

    #[test]
    fn garbage_yields_defaults_instead_of_a_crash() {
        for bytes in [&b"not json"[..], &b"{}"[..], &b"\x00\x01"[..]] {
            let decoded = Persisted::decode(bytes);
            assert!(decoded.token.is_none());
            assert!(decoded.board.is_none());
        }
    }

    #[test]
    fn oversized_state_drops_the_cache_before_the_token() {
        let mut huge = cached();
        huge.board.as_mut().unwrap().columns = (0..7000)
            .map(|i| Column {
                name: format!("column {i}"),
                cards: vec![Card {
                    id: format!("id{i}"),
                    title: "x".repeat(120),
                    number: Some(i),
                }],
            })
            .collect();
        let encoded = huge.encode();
        assert!(encoded.len() <= MAX_BYTES);
        let decoded = Persisted::decode(&encoded);
        assert_eq!(decoded.token.as_deref(), Some("gho_x"));
        assert!(decoded.board.is_none());
    }
}
