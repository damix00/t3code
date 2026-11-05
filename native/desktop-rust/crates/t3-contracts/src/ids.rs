use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! entity_id {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// A fresh random id, the way clients allocate command, message and thread ids.
            pub fn random() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self::new(id)
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }
    )*};
}

entity_id!(
    ThreadId,
    ProjectId,
    CommandId,
    MessageId,
    RunId,
    RunAttemptId,
    NodeId,
    TurnItemId,
    RuntimeRequestId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_plain_strings() {
        let id = ThreadId::new("t-1");
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""t-1""#);
        let back: ThreadId = serde_json::from_str(r#""t-1""#).unwrap();
        assert_eq!(back, id);
        assert_eq!(back.to_string(), "t-1");
    }

    #[test]
    fn random_ids_are_uuid_v4_and_distinct() {
        let a = CommandId::random();
        let b = CommandId::random();
        assert_ne!(a, b);
        let parsed = uuid::Uuid::parse_str(a.as_str()).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
    }
}
