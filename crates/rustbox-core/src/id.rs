use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7().to_string())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(s.to_string()))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

define_id!(SandboxId);
define_id!(SnapshotId);
define_id!(CommandId);

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! id_tests {
        ($id_type:ident, $mod_name:ident) => {
            mod $mod_name {
                use super::*;

                #[test]
                fn uniqueness() {
                    let a = $id_type::new();
                    let b = $id_type::new();
                    assert_ne!(a, b);
                }

                #[test]
                fn display_fromstr_roundtrip() {
                    let id = $id_type::new();
                    let s = id.to_string();
                    let parsed: $id_type = s.parse().unwrap();
                    assert_eq!(id, parsed);
                }

                #[test]
                fn serde_json_roundtrip() {
                    let id = $id_type::new();
                    let json = serde_json::to_string(&id).unwrap();
                    let back: $id_type = serde_json::from_str(&json).unwrap();
                    assert_eq!(id, back);
                }

                #[test]
                fn default_produces_valid_uuid() {
                    let id = $id_type::default();
                    assert!(!id.0.is_empty());
                    // Should be a valid UUID string
                    uuid::Uuid::parse_str(&id.0).expect("default should produce valid UUID");
                }
            }
        };
    }

    id_tests!(SandboxId, sandbox_id);
    id_tests!(SnapshotId, snapshot_id);
    id_tests!(CommandId, command_id);
}
