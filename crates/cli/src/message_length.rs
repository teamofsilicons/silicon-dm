use anyhow::{Context, Result, bail};
use serde_json::Value;
use silicon_dm_client::{Actor, ActorType, Client, MessageCreate, PageRequest};
use std::collections::HashSet;
use uuid::Uuid;

const SAFE_CHARACTERS: usize = 400;
pub const BLOCKED: &str = "message too long, not delivered. Your carbon would likely not read this long message, you can break this message down into multiple smaller messages, or just write a single short message, if you wanna still send the longer version you can send it by adding the flag --dangerously-send-long-message";
pub const SENT_WARNING: &str = "Message sent but it was above the 400 characters safe read limits.";

pub fn needs_check(actor: &Actor, message: &MessageCreate) -> bool {
    actor.actor_type == ActorType::Silicon
        && message
            .text
            .as_deref()
            .is_some_and(|text| text.chars().count() > SAFE_CHARACTERS)
}

/// Resolve recipients before submitting anything to the durable send queue.
/// Addressing one participant does not hide the message from other members.
pub async fn check(
    client: &Client,
    actor: &Actor,
    conversation: Uuid,
    message: &MessageCreate,
    allow_long: bool,
) -> Result<bool> {
    if !needs_check(actor, message) {
        return Ok(false);
    }
    let mut page = PageRequest {
        limit: Some(100),
        cursor: None,
    };
    let mut cursors = HashSet::new();
    loop {
        let result = client
            .conversations(&page)
            .await
            .context("cannot verify conversation recipients; message not submitted")?;
        if let Some(found) = result.items.iter().find(|item| item.id == conversation) {
            let has_carbon = found
                .participants
                .iter()
                .any(|p| p.actor_type == ActorType::Carbon);
            if has_carbon && !allow_long {
                bail!(BLOCKED);
            }
            return Ok(has_carbon);
        }
        let Some(cursor) = result.next_cursor else {
            bail!("cannot find conversation to verify recipients; message not submitted");
        };
        if !cursors.insert(cursor.clone()) {
            bail!("conversation pagination did not advance; message not submitted");
        }
        page.cursor = Some(cursor);
    }
}

pub fn sent_warning(overridden: bool, result: &Value) -> bool {
    overridden
        && result["response"]["state"] == "completed"
        && result["response"]["error"].is_null()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn actor(actor_type: ActorType) -> Actor {
        Actor {
            actor_type,
            id: "test:org".into(),
        }
    }

    #[test]
    fn unicode_boundary_and_authenticated_sender() -> Result<()> {
        for (text, expected) in [
            ("x".repeat(400), false),
            ("界".repeat(400), false),
            ("🙂".repeat(401), true),
        ] {
            let message: MessageCreate =
                serde_json::from_value(json!({"text":text, "sender_id":"carbon"}))?;
            assert_eq!(needs_check(&actor(ActorType::Silicon), &message), expected);
            assert!(!needs_check(&actor(ActorType::Carbon), &message));
        }
        Ok(())
    }

    #[test]
    fn warning_requires_successful_override() {
        for state in ["pending", "failed", "completed"] {
            let result = json!({"response":{"state":state,"error":null}});
            assert_eq!(sent_warning(true, &result), state == "completed");
            assert!(!sent_warning(false, &result));
        }
        assert!(!sent_warning(
            true,
            &json!({"response":{"state":"completed","error":{"code":"failed"}}})
        ));
    }
}
