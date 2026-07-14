use atat::atat_derive::AtatLen;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, AtatLen)]
pub struct ContextId(pub u8);

impl From<u8> for ContextId {
    fn from(value: u8) -> Self {
        ContextId(value)
    }
}
