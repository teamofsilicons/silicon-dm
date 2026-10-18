use silicon_dm_client::{Actor, ActorType, MessageCreate};

pub const NOTICE: &str = "Replaced em dashes (—) with normal dashes (-) in the message. To keep em dashes for this send, add --dangerously-use-em-dash.";

/// Normalize only CLI send text and a spoken message's script, before checking their final
/// character counts. Returns whether anything was replaced.
pub fn replace(actor: &Actor, message: &mut MessageCreate, preserve: bool) -> bool {
    if preserve || actor.actor_type != ActorType::Silicon {
        return false;
    }
    let mut replaced = false;
    if let Some(text) = message.text.as_mut().filter(|text| text.contains('—')) {
        *text = spaced_hyphens(text);
        replaced = true;
    }
    if let Some(speech) = message
        .speech
        .as_mut()
        .filter(|speech| speech.script.contains('—'))
    {
        speech.script = spaced_hyphens(&speech.script);
        replaced = true;
    }
    replaced
}

/// `a—b` becomes `a - b`; whitespace already around the dash is kept.
fn spaced_hyphens(text: &str) -> String {
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
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_dm_client::SpeechInput;

    #[test]
    fn a_spoken_script_and_its_caption_are_normalized_too() {
        let silicon = Actor::new(ActorType::Silicon, "si:writer");
        let spoken = |script: &str, caption: Option<&str>| MessageCreate {
            text: caption.map(str::to_owned),
            speech: Some(SpeechInput {
                script: script.into(),
                voice: Some("mark".into()),
            }),
            ..Default::default()
        };
        let mut message = spoken("[sighs] Late—again.", Some("Build—1843"));
        assert!(replace(&silicon, &mut message, false));
        assert_eq!(
            message.speech.as_ref().map(|s| s.script.as_str()),
            Some("[sighs] Late - again.")
        );
        assert_eq!(message.text.as_deref(), Some("Build - 1843"));
        // The script alone also counts as a replacement worth a notice.
        let mut script_only = spoken("Done—finally.", None);
        assert!(replace(&silicon, &mut script_only, false));
        let mut kept = spoken("Done—finally.", None);
        assert!(!replace(&silicon, &mut kept, true));
        assert!(!replace(
            &Actor::new(ActorType::Carbon, "c:ada"),
            &mut kept,
            false
        ));
        assert_eq!(
            kept.speech.map(|s| s.script),
            Some("Done—finally.".to_owned())
        );
    }

    #[test]
    fn replacement_preserves_existing_whitespace_and_other_punctuation() {
        let actor = Actor::new(ActorType::Silicon, "si:writer");
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
