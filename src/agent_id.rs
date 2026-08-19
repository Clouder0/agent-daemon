//! Logical agent identity and NATS subject encoding (whitepaper §2.3).
//!
//! Grammar:
//!
//! ```text
//! agent_id := segment ("/" segment)*
//! segment  := [a-z0-9][a-z0-9_-]{0,62}
//! ```
//!
//! `.` cannot appear in a segment, so the `/`-separated form maps injectively
//! onto the `.`-separated NATS subject form.

use std::fmt;

use crate::error::AgentdError;

/// Subject prefix every agent's events live under (whitepaper §5.2).
pub const SUBJECT_PREFIX: &str = "agent.events";

/// Maximum number of segments in one agent_id (sanity bound; keeps subjects
/// well within NATS limits).
const MAX_SEGMENTS: usize = 16;

/// Maximum length of one segment.
const MAX_SEGMENT_LEN: usize = 63;

/// A validated agent id, e.g. `coding/main`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId {
    raw: String,
    segments: Vec<String>,
}

impl AgentId {
    /// Parse and validate an agent id per the v0 grammar.
    pub fn parse(input: &str) -> Result<Self, AgentdError> {
        let segments = input.split('/').map(str::to_owned).collect::<Vec<String>>();
        if segments.len() > MAX_SEGMENTS {
            return Err(AgentdError::invalid_agent_id(format!(
                "too many segments ({} > {MAX_SEGMENTS})",
                segments.len()
            )));
        }
        for segment in &segments {
            validate_segment(segment)?;
        }
        Ok(Self {
            raw: input.to_owned(),
            segments,
        })
    }

    /// The canonical `/`-separated form as given at parse time.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The JetStream filter subject for this agent, e.g. `agent.events.coding.main`.
    pub fn subject(&self) -> String {
        format!("{SUBJECT_PREFIX}.{}", self.segments.join("."))
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

fn validate_segment(segment: &str) -> Result<(), AgentdError> {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return Err(AgentdError::invalid_agent_id("empty segment".into()));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(AgentdError::invalid_agent_id(format!(
            "segment {segment:?}: first character must be [a-z0-9]"
        )));
    }
    let len = first.len_utf8() + chars.as_str().len();
    if len > MAX_SEGMENT_LEN {
        return Err(AgentdError::invalid_agent_id(format!(
            "segment {segment:?}: length {len} > {MAX_SEGMENT_LEN}"
        )));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(AgentdError::invalid_agent_id(format!(
                "segment {segment:?}: character {c:?} not allowed"
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
        let id = AgentId::parse("coding/main").unwrap();
        assert_eq!(id.subject(), "agent.events.coding.main");
        assert_eq!(id.to_string(), "coding/main");

        let id = AgentId::parse("a").unwrap();
        assert_eq!(id.subject(), "agent.events.a");
    }

    #[test]
    fn accepts_multi_segment_and_allowed_characters() {
        for s in [
            "assistant/personal",
            "research/market",
            "a-b_c/0123-x",
            "z9",
        ] {
            assert!(AgentId::parse(s).is_ok(), "should accept {s:?}");
        }
    }

    #[test]
    fn rejects_invalid_forms() {
        for s in [
            "",              // empty
            "/a",            // leading slash → empty first segment
            "a/",            // trailing slash → empty last segment
            "a//b",          // empty middle segment
            "Coding/main",   // uppercase
            "-abc/x",        // invalid first character
            "_abc",          // invalid first character
            "a.b/c",         // '.' not in grammar (also guards subject injectivity)
            "a b",           // whitespace
            "a/b c",         // whitespace in later segment
            &"x".repeat(64), // segment too long
        ] {
            assert!(AgentId::parse(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn rejects_too_many_segments() {
        let id = vec!["s"; MAX_SEGMENTS + 1].join("/");
        assert!(AgentId::parse(&id).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id = AgentId::parse("coding/main").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"coding/main\"");
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
            prop_assert_eq!(parsed.to_string(), id);
            let subject = parsed.subject();
            prop_assert!(subject.starts_with("agent.events."));
            prop_assert!(!subject.contains('/'));
        }
    }

    /// Generates ids matching the grammar: [a-z0-9][a-z0-9_-]{0,62} segments
    /// joined by '/'. Index space avoids regex dependencies.
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
    const ALNUM_COUNT: usize = 36;

    fn valid_segment() -> impl proptest::prelude::Strategy<Value = String> {
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
        proptest::collection::vec(valid_segment(), 1..4).prop_map(|segs| segs.join("/"))
    }
}
