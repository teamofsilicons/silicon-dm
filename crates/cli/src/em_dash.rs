use silicon_dm_client::{Actor, ActorType, MessageCreate};

pub const NOTICE: &str = "Replaced em dashes (—) with normal dashes (-) in the message. To keep em dashes for this send, add --dangerously-use-em-dash.";

/// Normalize only CLI send text, before checking its final character count.
pub fn replace(actor: &Actor, message: &mut MessageCreate, preserve: bool) -> bool {
    if preserve || actor.actor_type != ActorType::Silicon {
        return false;
    }
    let Some(text) = message.text.as_mut().filter(|text| text.contains('—')) else {
        return false;
    };
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '—' {
            if !normalized.ends_with(char::is_whitespace) {
                normalized.push(' ');
            }
            normalized.push('-');
            if !chars.peek().is_some_and(|next| next.is_whitespace()) {
                normalized.push(' ');
            }
        } else {
            normalized.push(character);
        }
    }
    *text = normalized;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_preserves_existing_whitespace_and_other_punctuation() {
        let actor = Actor {
            actor_type: ActorType::Silicon,
            id: "si:writer".into(),
        };
        for (input, expected) in [
            ("hello—world", "hello - world"),
            ("hello —world", "hello - world"),
            ("hello— world", "hello - world"),
            ("hello  —  world", "hello  -  world"),
            ("hello\t—\nworld", "hello\t-\nworld"),
            ("—hello—", " - hello - "),
            ("a——b", "a - - b"),
            ("🙂—é – -", "🙂 - é – -"),
        ] {
            let mut message = MessageCreate {
                text: Some(input.into()),
                ..Default::default()
            };
            assert!(replace(&actor, &mut message, false));
            assert_eq!(message.text.as_deref(), Some(expected));
            assert!(!replace(&actor, &mut message, false));
        }
    }
}
