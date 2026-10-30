use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Nil UUIDs are not valid persistent identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an identifier cannot be the nil UUID")]
pub struct InvalidId;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(try_from = "Uuid", into = "Uuid")]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TryFrom<Uuid> for $name {
            type Error = InvalidId;
            fn try_from(value: Uuid) -> Result<Self, Self::Error> {
                if value.is_nil() {
                    Err(InvalidId)
                } else {
                    Ok(Self(value))
                }
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

identifier!(ProjectId);
identifier!(MediaId);
identifier!(TrackId);
identifier!(ClipId);
identifier!(RequestId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_ids_round_trip_and_reject_nil() {
        let id = ProjectId::try_from(Uuid::from_u128(17)).unwrap();
        let encoded = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ProjectId>(&encoded).unwrap(), id);
        assert!(
            serde_json::from_str::<ProjectId>("\"00000000-0000-0000-0000-000000000000\"").is_err()
        );
    }
}
