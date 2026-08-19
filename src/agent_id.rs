//! Logical agent identity and NATS subject encoding (whitepaper §2.3,
//! ADR-0006).
//!
//! Grammar:
//!
//! ```text
//! agent_id := token ("." token)*
//! token    := [a-z0-9][a-z0-9_-]{0,62}
//! ```
//!
//! `.` is both the id separator and NATS's subject separator, so the id, its
//! filter subject, and its config filename are all the same dot-form (zero
//! transforms, injective — see ADR-0004).

use std::fmt;

use crate::error::AgentdError;

/// Subject prefix every agent's events live under (whitepaper §5.2).
pub const SUBJECT_PREFIX: &str = "agent.events";

/// Maximum number of tokens in one agent_id (sanity bound; keeps subjects
/// well within NATS limits).
const MAX_TOKENS: usize = 16;

/// Maximum length of one token.
const MAX_TOKEN_LEN: usize = 63;

/// A validated agent id, e.g. `coding_main`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId {
    raw: String,
}

impl AgentId {
    /// Parse and validate an agent id per the v0 grammar.
    pub fn parse(input: &str) -> Result<Self, AgentdError> {
        let tokens = input.split('_').collect::<Vec<_>>();
        if tokens.len() > MAX_TOKENS {
            return Err(AgentdError::invalid_agent_id(format!(
                "too many tokens ({} > {MAX_TOKENS})",
                tokens.len()
            )));
        }
        for token in tokens {
            validate_token(token)?;
        }
        Ok(Self {
            raw: input.to_owned(),
        })
    }

    /// The canonical dot-separated form as given at parse time.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The JetStream filter subject for this agent, e.g. `agent.events.coding_main`.
    /// Identity: prefix only — the id is a single NATS token (ADR-0006).
    pub fn subject(&self) -> String {
        let raw = &self.raw;
        format!("{SUBJECT_PREFIX}.{raw}")
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for AgentId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for AgentId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        AgentId::parse(&s).map_err(serde::de::Error::custom)
    }
}

fn validate_token(token: &str) -> Result<(), AgentdError> {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return Err(AgentdError::invalid_agent_id("empty token".into()));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(AgentdError::invalid_agent_id(format!(
            "token {token:?}: first character must be [a-z0-9]"
        )));
    }
    let len = first.len_utf8() + chars.as_str().len();
    if len > MAX_TOKEN_LEN {
        return Err(AgentdError::invalid_agent_id(format!(
            "token {token:?}: length {len} > {MAX_TOKEN_LEN}"
        )));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(AgentdError::invalid_agent_id(format!(
                "token {token:?}: character {c:?} not allowed"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_and_encodes_subject() {
        let id = AgentId::parse("coding_main").unwrap();
        assert_eq!(id.subject(), "agent.events.coding_main");
        assert_eq!(id.to_string(), "coding_main");

        let id = AgentId::parse("a").unwrap();
        assert_eq!(id.subject(), "agent.events.a");
    }

    #[test]
    fn accepts_multi_token_and_allowed_characters() {
        for s in ["assistant_personal", "research_market", "a-b_c-0123x", "z9"] {
            assert!(AgentId::parse(s).is_ok(), "should accept {s:?}");
        }
    }

    #[test]
    fn rejects_invalid_forms() {
        for s in [
            "",              // empty
            "_a",            // leading underscore → empty first token
            "a_",            // trailing underscore → empty last token
            "a__b",          // empty middle token
            "Coding_main",   // uppercase
            "-abc_x",        // invalid first character
            "a.b",           // '.' not in grammar (ADR-0006: underscore-separated)
            "a/b",           // '/' not in grammar
            "a.b_c",         // '.' still banned alongside the separator
            "a b",           // whitespace
            "a_b c",         // whitespace in later token
            "a.b-c",         // '.' not in grammar
            &"x".repeat(64), // token too long
        ] {
            assert!(AgentId::parse(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn rejects_too_many_tokens() {
        let id = vec!["s"; MAX_TOKENS + 1].join("_");
        assert!(AgentId::parse(&id).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id = AgentId::parse("coding_main").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"coding_main\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_str::<AgentId>("\"bad id\"").is_err());
    }

    proptest::proptest! {
        #[test]
        fn generated_ids_roundtrip(id in valid_agent_id()) {
            let parsed = match AgentId::parse(&id) {
                Ok(p) => p,
                Err(e) => panic!("generated id {id:?} rejected: {e}"),
            };
            prop_assert_eq!(parsed.to_string(), id.as_str());
            let subject = parsed.subject();
            prop_assert!(subject.starts_with("agent.events."));
            prop_assert_eq!(
                subject.strip_prefix("agent.events.").unwrap(),
                id.as_str(),
                "identity: subject must equal id (ADR-0006)"
            );
        }
    }

    /// Generates ids matching the grammar: [a-z0-9][a-z0-9-]{0,62} tokens
    /// joined by '_'. Index space avoids regex dependencies.
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";
    const ALNUM_COUNT: usize = 36;

    fn valid_token() -> impl proptest::prelude::Strategy<Value = String> {
        use proptest::prelude::*;
        (
            0..ALNUM_COUNT,
            proptest::collection::vec(0..CHARSET.len(), 0..20),
        )
            .prop_map(|(first, rest)| {
                let mut s = String::new();
                s.push(CHARSET[first] as char);
                for r in rest {
                    s.push(CHARSET[r] as char);
                }
                s
            })
    }

    fn valid_agent_id() -> impl proptest::prelude::Strategy<Value = String> {
        use proptest::prelude::*;
        proptest::collection::vec(valid_token(), 1..4).prop_map(|toks| toks.join("_"))
    }
}
