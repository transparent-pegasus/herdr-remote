//! An agent's own transcript, which holds what the terminal threw away: a pane
//! running on the alternate screen keeps no scrollback, so the file is the only
//! place its finished answers still exist.

mod preamble;
mod claude;

use serde::Serialize;

/// Only the two speakers survive normalization. Tool calls, tool output,
/// thinking, and system preambles are dropped where they are parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// `seq` is the message's position in its file, and doubles as the cursor the
/// phone sends back as `before=` when it reaches further into the past.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub seq: u64,
    pub role: Role,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_serialize_in_the_shape_the_phone_reads() {
        let message = Message {
            seq: 3,
            role: Role::Assistant,
            text: "done".into(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(json, r#"{"seq":3,"role":"assistant","text":"done"}"#);
    }
}
