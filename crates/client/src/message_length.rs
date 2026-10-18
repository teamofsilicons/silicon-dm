use crate::{Actor, ActorType, Client, MessageCreate, PageRequest, runtime::store::SessionError};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashSet;

const SAFE_CHARACTERS: usize = 140;
pub const BLOCKED: &str = "message too long, not delivered. Your carbon would likely not read this long message, you can break this message down into multiple smaller messages, or just write a single short message, if you wanna still send the longer version you can send it by adding the flag --dangerously-send-long-message";
pub const SENT_WARNING: &str =
    "Message sent but it was above the 140 characters safe carbon read limits";

/// The parts of a message a Carbon takes in that are over 140 Unicode characters: its text
/// (a spoken message's caption) and its spoken words (the script without cues, which a Carbon
/// hears and reads as the transcript), with their lengths.
pub fn long_parts(message: &MessageCreate) -> Vec<(&'static str, usize)> {
    let mut parts = Vec::new();
    if let Some(length) = message
        .text
        .as_deref()
        .map(|text| text.chars().count())
        .filter(|length| *length > SAFE_CHARACTERS)
    {
        parts.push(("text", length));
    }
    if let Some(length) = message
        .speech
        .as_ref()
        .map(|speech| crate::speech::spoken_words(&speech.script).chars().count())
        .filter(|length| *length > SAFE_CHARACTERS)
    {
        parts.push(("spoken words (cues removed)", length));
    }
    parts
}

pub fn needs_check(actor: &Actor, message: &MessageCreate) -> bool {
    actor.actor_type == ActorType::Silicon && !long_parts(message).is_empty()
}

/// The refusal: the contract's exact words, with what was counted.
fn too_long(message: &MessageCreate) -> anyhow::Error {
    let counted = long_parts(message)
        .iter()
        .map(|(part, length)| format!("{part}: {length} characters"))
        .collect::<Vec<_>>()
        .join("; ");
    anyhow::Error::new(SessionError {
        code: "message_too_long",
        message: BLOCKED.to_owned(),
        hint: format!(
            "Counted {counted}; a Carbon reads at most {SAFE_CHARACTERS} comfortably. Nothing was queued."
        ),
    })
}

/// Resolve recipients before submitting anything to the durable send queue.
/// Addressing one participant does not hide the message from other members.
pub async fn check(
    client: &Client,
    actor: &Actor,
    conversation: impl std::fmt::Display,
    message: &MessageCreate,
    allow_long: bool,
) -> Result<bool> {
    let conversation = conversation.to_string();
    if !needs_check(actor, message) {
        return Ok(false);
    }
    // Sending to an account id names the recipient's kind: c: is a Carbon.
    if let Some(kind) = ActorType::of_id(&conversation)
        && !conversation.contains("::")
        && !conversation.starts_with("g:")
    {
        let carbon = kind == ActorType::Carbon;
        if carbon && !allow_long {
            return Err(too_long(message));
        }
        return Ok(carbon);
    }
    let conversation = if uuid::Uuid::parse_str(&conversation).is_ok() {
        match client.group(&conversation).await {
            Ok(group) => group.id,
            Err(crate::Error::Api { status: 404, .. }) => conversation,
            Err(error) => return Err(error.into()),
        }
    } else {
        conversation
    };
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
                return Err(too_long(message));
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
        Actor::new(actor_type, "si:test")
    }

    #[test]
    fn unicode_boundary_and_authenticated_sender() -> Result<()> {
        for (text, expected) in [
            ("x".repeat(140), false),
            ("界".repeat(140), false),
            ("🙂".repeat(141), true),
        ] {
            let message: MessageCreate =
                serde_json::from_value(json!({"text":text, "sender_id":"carbon"}))?;
            assert_eq!(needs_check(&actor(ActorType::Silicon), &message), expected);
            assert!(!needs_check(&actor(ActorType::Carbon), &message));
        }
        Ok(())
    }

    #[test]
    fn spoken_words_are_counted_without_their_cues() -> Result<()> {
        let speech = |script: String| -> Result<MessageCreate> {
            Ok(serde_json::from_value(
                json!({"message":"", "speech":{"script":script}}),
            )?)
        };
        let cues = "[excited] ".repeat(20);
        // 140 words plus many cues is safe; one more word is not.
        let safe = speech(format!("{cues}{}", "a".repeat(140)))?;
        assert!(long_parts(&safe).is_empty());
        assert!(!needs_check(&actor(ActorType::Silicon), &safe));
        let long = speech(format!("{cues}{} [laughs]", "界".repeat(141)))?;
        assert_eq!(
            long_parts(&long),
            vec![("spoken words (cues removed)", 141)]
        );
        assert!(needs_check(&actor(ActorType::Silicon), &long));
        assert!(!needs_check(&actor(ActorType::Carbon), &long));
        let error = too_long(&long);
        let reason = error.downcast_ref::<SessionError>().expect("a coded error");
        assert_eq!(reason.code, "message_too_long");
        assert_eq!(reason.message, BLOCKED);
        assert!(
            reason
                .hint
                .contains("spoken words (cues removed): 141 characters")
        );
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
