//! Voices and spoken messages.
//!
//! Every account speaks in one voice of a small catalog (`DM_VOICES`). A Silicon "speaks" by
//! sending a script: words, with square-bracket audio cues (`[excited]`, `[whispers]…[/whispers]`)
//! that shape the delivery and are never read aloud. DM pins the voice on the message when it
//! is accepted, so a later voice change never changes what an old message sounds like.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Most Unicode scalar values a script may have, cues included.
pub const MAX_SCRIPT_CHARACTERS: usize = 5_000;
/// Longest cue (the text between `[` and `]`): cues are short directions, and a longer one
/// is almost always a missing `]`.
pub const MAX_CUE_CHARACTERS: usize = 120;
/// Most voices a catalog may list.
pub const MAX_VOICES: usize = 50;

/// One voice of the catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    /// Public identifier used by the API, the CLI and the website (`mark`).
    pub key: String,
    /// Display name (`Mark`).
    pub name: String,
    /// The provider's voice id (an `ElevenLabs` voice id), returned for transparency.
    pub voice_id: String,
    /// How the voice sounds, in one sentence for people choosing it.
    pub description: String,
}

/// The voices accounts may speak in: never empty, keys unique.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoiceCatalog(Vec<Voice>);

/// The catalog DM ships: the five most used `ElevenLabs` voices in October 2026, each checked
/// to synthesize through Deepgram's managed `ElevenLabs` model `eleven_v4`.
const DEFAULT_VOICES: [(&str, &str, &str, &str); 5] = [
    (
        "mark",
        "Mark",
        "UgBBYS2sOqTuMpoF3BR0",
        "Relaxed and natural, like a friend catching you up. Male, American.",
    ),
    (
        "brittney",
        "Brittney",
        "kPzsL2i3teMYv0FxEYQ6",
        "Bright and upbeat. Female, American.",
    ),
    (
        "jamal",
        "Young Jamal",
        "6OzrBCQf8cjERkYgzSg8",
        "Chill and easygoing. Male, American.",
    ),
    (
        "jessa",
        "Jessa",
        "yj30vwTGJxSHezdAGsv9",
        "Warm and casual. Female, American.",
    ),
    (
        "hope",
        "Hope",
        "tnSpp4vdxKPjI9w0GnoV",
        "Clear and cheerful. Female, American.",
    ),
];

/// The rule behind every account's default voice, as the API states it.
pub const DEFAULT_VOICE_RULE: &str = "Until an account (or a Silicon's custodian) chooses a voice, the account speaks in the catalog voice at index u64(first 8 bytes of SHA-256(account uuid), big-endian) mod the number of voices, so the default never changes and different Silicons usually sound different.";

impl Default for VoiceCatalog {
    fn default() -> Self {
        Self(
            DEFAULT_VOICES
                .iter()
                .map(|(key, name, voice_id, description)| Voice {
                    key: (*key).to_owned(),
                    name: (*name).to_owned(),
                    voice_id: (*voice_id).to_owned(),
                    description: (*description).to_owned(),
                })
                .collect(),
        )
    }
}

impl VoiceCatalog {
    /// Parses `DM_VOICES`: a JSON array of `{key, name, voice_id, description}`.
    ///
    /// # Errors
    /// Says which entry is wrong and why.
    pub fn parse(json: &str) -> Result<Self, String> {
        let voices: Vec<Voice> = serde_json::from_str(json).map_err(|error| {
            format!(
                "must be a JSON array of {{\"key\",\"name\",\"voice_id\",\"description\"}} objects ({error})"
            )
        })?;
        Self::new(voices)
    }

    /// Validates a catalog.
    ///
    /// # Errors
    /// Says which entry is wrong and why.
    pub fn new(voices: Vec<Voice>) -> Result<Self, String> {
        if voices.is_empty() || voices.len() > MAX_VOICES {
            return Err(format!("must list 1 to {MAX_VOICES} voices"));
        }
        for (index, voice) in voices.iter().enumerate() {
            if !valid_voice_key(&voice.key) {
                return Err(format!(
                    "voice {index}: key `{}` must be 1 to 32 lowercase letters, digits or -, starting with a letter",
                    voice.key
                ));
            }
            if voices[..index].iter().any(|other| other.key == voice.key) {
                return Err(format!(
                    "voice {index}: key `{}` is listed twice",
                    voice.key
                ));
            }
            if !valid_provider_voice_id(&voice.voice_id) {
                return Err(format!(
                    "voice {index} ({}): voice_id must be 1 to 128 ASCII letters, digits, _ or -",
                    voice.key
                ));
            }
            if voice.name.trim().is_empty()
                || voice.name.chars().count() > 80
                || voice.name.chars().any(char::is_control)
            {
                return Err(format!(
                    "voice {index} ({}): name must be 1 to 80 characters",
                    voice.key
                ));
            }
            if voice.description.chars().count() > 300
                || voice.description.chars().any(char::is_control)
            {
                return Err(format!(
                    "voice {index} ({}): description must be at most 300 characters",
                    voice.key
                ));
            }
        }
        Ok(Self(voices))
    }

    /// The voices in catalog order.
    #[must_use]
    pub fn voices(&self) -> &[Voice] {
        &self.0
    }

    /// The voice with this key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Voice> {
        self.0.iter().find(|voice| voice.key == key)
    }

    /// Every key, in catalog order (for error details).
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.0.iter().map(|voice| voice.key.as_str()).collect()
    }

    /// The voice an account speaks in until a voice is chosen for it: deterministic, so it
    /// never changes, and spread over the catalog, so Silicons usually sound different.
    #[must_use]
    pub fn default_for(&self, account_uuid: &str) -> &Voice {
        let digest = Sha256::digest(account_uuid.as_bytes());
        let mut first = [0_u8; 8];
        first.copy_from_slice(&digest[..8]);
        let length = u64::try_from(self.0.len()).unwrap_or(1).max(1);
        let index = usize::try_from(u64::from_be_bytes(first) % length).unwrap_or(0);
        &self.0[index.min(self.0.len().saturating_sub(1))]
    }
}

/// Whether `key` has the shape of a catalog key.
#[must_use]
pub fn valid_voice_key(key: &str) -> bool {
    (1..=32).contains(&key.len())
        && key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `id` has the shape of a provider voice id.
#[must_use]
pub fn valid_provider_voice_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// The voice DM fixed on a spoken message when it accepted it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PinnedVoice {
    /// Catalog key.
    pub key: String,
    /// Display name at the time.
    pub name: String,
    /// The provider's voice id.
    pub voice_id: String,
    /// The speech model (`eleven_v4`).
    pub model: String,
}

/// A spoken message: what to say and how.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Speech {
    /// Words and square-bracket audio cues.
    pub script: String,
    /// The voice the sender asked for (a catalog key); absent means the sender's voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Set by DM when the message is accepted (whatever a request sends is replaced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<PinnedVoice>,
}

impl Speech {
    /// The words a listener hears: the script without its cues, whitespace collapsed.
    #[must_use]
    pub fn spoken_words(&self) -> String {
        spoken_words(&self.script)
    }

    /// The cache key of the pinned voice saying the script.
    #[must_use]
    pub fn cache_key(&self) -> Option<[u8; 32]> {
        self.pinned
            .as_ref()
            .map(|pinned| cache_key(&pinned.model, &pinned.voice_id, &self.script))
    }
}

/// The audio cache key of one rendering: SHA-256 of `model|voice_id|script`. Models and voice
/// ids never contain `|`, so the key is unambiguous.
#[must_use]
pub fn cache_key(model: &str, voice_id: &str, script: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update(b"|");
    hasher.update(voice_id.as_bytes());
    hasher.update(b"|");
    hasher.update(script.as_bytes());
    hasher.finalize().into()
}

/// Checks a script: 1 to 5,000 characters, no control characters but line breaks and tabs,
/// cues balanced and not nested (`[` … `]`), no empty or overlong cue, every closing cue
/// `[/name]` closes an open `[name]` (compared without case), and some words besides the cues.
///
/// # Errors
/// What is wrong and where (1-based character position).
pub fn validate_script(script: &str) -> Result<(), String> {
    let characters = script.chars().count();
    if script.trim().is_empty() {
        return Err("speech.script is empty: write the words to say (cues like [warmly] go before the words they colour)".to_owned());
    }
    if characters > MAX_SCRIPT_CHARACTERS {
        return Err(format!(
            "speech.script has {characters} characters; a spoken message holds at most {MAX_SCRIPT_CHARACTERS} (cues included). Split it into several messages."
        ));
    }
    let mut open: Vec<String> = Vec::new();
    let mut cue: Option<(usize, String)> = None;
    for (index, character) in script.chars().enumerate() {
        let position = index + 1;
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            return Err(format!(
                "speech.script has a control character at position {position}; use plain text, line breaks and tabs only"
            ));
        }
        match (character, &mut cue) {
            ('[', Some((start, _))) => {
                return Err(format!(
                    "speech.script opens a cue at position {position} inside the cue opened at position {start}; close it with ] first (cues cannot be nested)"
                ));
            }
            ('[', None) => cue = Some((position, String::new())),
            (']', None) => {
                return Err(format!(
                    "speech.script has a ] at position {position} without an opening [; square brackets only mark cues such as [laughs]"
                ));
            }
            (']', Some((start, text))) => {
                let name = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if name.is_empty() {
                    return Err(format!(
                        "speech.script has an empty cue [] at position {start}; write a direction such as [warmly] or remove the brackets"
                    ));
                }
                if let Some(closing) = name.strip_prefix('/') {
                    let closing = closing.trim().to_lowercase();
                    let Some(found) = open.iter().rposition(|opened| *opened == closing) else {
                        return Err(format!(
                            "speech.script closes [/{closing}] at position {start} without an open [{closing}] before it; paired cues look like [whispers] … [/whispers]"
                        ));
                    };
                    open.remove(found);
                } else {
                    open.push(name.to_lowercase());
                }
                cue = None;
            }
            (_, Some((start, text))) => {
                text.push(character);
                if text.chars().count() > MAX_CUE_CHARACTERS {
                    return Err(format!(
                        "speech.script has a cue at position {start} longer than {MAX_CUE_CHARACTERS} characters; is a ] missing?"
                    ));
                }
            }
            (_, None) => {}
        }
    }
    if let Some((start, _)) = cue {
        return Err(format!(
            "speech.script has an unclosed [ at position {start}; finish the cue with ] (for example [excited])"
        ));
    }
    if spoken_words(script).is_empty() {
        // Verified with the Voice Agent: a script of cues alone fails to speak.
        return Err("speech.script has only cues and no words to say; the voice cannot perform a cue alone. Add the words it colours, for example [laughs] Ha!".to_owned());
    }
    Ok(())
}

/// The words of a script without its cues, whitespace collapsed. An unclosed `[` keeps the
/// rest as words (validated scripts never have one).
#[must_use]
pub fn spoken_words(script: &str) -> String {
    let mut words = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(start) = rest.find('[') {
        words.push_str(&rest[..start]);
        words.push(' ');
        if let Some(end) = rest[start..].find(']') {
            rest = &rest[start + end + 1..];
        } else {
            rest = &rest[start..];
            break;
        }
    }
    words.push_str(rest);
    collapse_whitespace(&words)
}

/// A plain text message as a script, for listening to it: brackets become parentheses (in a
/// text message they are words, never cues), backticks go, links are read as "link".
#[must_use]
pub fn plain_text_script(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let words: Vec<String> = line
            .split_whitespace()
            .map(|word| {
                if word.starts_with("https://") || word.starts_with("http://") {
                    "link".to_owned()
                } else {
                    word.replace('[', "(").replace(']', ")").replace('`', "")
                }
            })
            .filter(|word| !word.is_empty())
            .collect();
        if !words.is_empty() {
            lines.push(words.join(" "));
        }
    }
    lines.join("\n")
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        Speech, VoiceCatalog, cache_key, plain_text_script, spoken_words, validate_script,
    };

    #[test]
    fn the_default_catalog_has_the_five_voices() {
        let catalog = VoiceCatalog::default();
        assert_eq!(
            catalog.keys(),
            vec!["mark", "brittney", "jamal", "jessa", "hope"]
        );
        assert_eq!(
            catalog.get("jamal").map(|voice| voice.name.as_str()),
            Some("Young Jamal")
        );
        assert!(catalog.get("george").is_none());
    }

    #[test]
    fn default_voices_follow_the_sha256_rule() {
        let catalog = VoiceCatalog::default();
        let digest = <sha2::Sha256 as sha2::Digest>::digest(b"a8K");
        let mut first = [0_u8; 8];
        first.copy_from_slice(&digest[..8]);
        let expected = usize::try_from(u64::from_be_bytes(first) % 5).unwrap_or(0);
        assert_eq!(
            catalog.default_for("a8K").key,
            catalog.voices()[expected].key
        );
        assert_eq!(
            catalog.default_for("a8K"),
            catalog.default_for("a8K"),
            "stable"
        );
        let spread: std::collections::BTreeSet<_> = (0..40)
            .map(|n| catalog.default_for(&format!("uuid-{n}")).key.clone())
            .collect();
        assert!(
            spread.len() >= 4,
            "defaults spread over the catalog: {spread:?}"
        );
    }

    #[test]
    fn catalogs_from_configuration_are_validated() {
        assert!(VoiceCatalog::parse("[]").is_err());
        assert!(VoiceCatalog::parse("{}").is_err());
        let duplicate = r#"[{"key":"a","name":"A","voice_id":"x","description":""},{"key":"a","name":"B","voice_id":"y","description":""}]"#;
        assert!(VoiceCatalog::parse(duplicate).is_err_and(|error| error.contains("listed twice")));
        let bad_id = r#"[{"key":"a","name":"A","voice_id":"x y","description":""}]"#;
        assert!(VoiceCatalog::parse(bad_id).is_err());
        let extra = r#"[{"key":"a","name":"A","voice_id":"x","description":"","age":3}]"#;
        assert!(VoiceCatalog::parse(extra).is_err());
        let good = r#"[{"key":"nova","name":"Nova","voice_id":"abc_123","description":"Calm."}]"#;
        assert!(
            VoiceCatalog::parse(good).is_ok_and(|catalog| catalog.default_for("x").key == "nova")
        );
    }

    #[test]
    fn scripts_follow_the_cue_rules() {
        for good in [
            "[excited] It works!",
            "[whispers] Just between us. [/whispers] Now, back to work.",
            "[Whispers] quiet [/whispers]",
            "[laughs] Ha!",
            "Plain words.\nA second line.",
            "[sighs] That took a while... [short pause] but it is DONE.",
        ] {
            assert!(validate_script(good).is_ok(), "{good}");
        }
        for (bad, needle) in [
            ("", "empty"),
            ("   ", "empty"),
            ("[excited It works", "unclosed [ at position 1"),
            ("It works]", "] at position 9"),
            ("[a [b] c]", "inside the cue"),
            ("[] hello", "empty cue"),
            ("[ ] hello", "empty cue"),
            ("hello [/whispers]", "without an open [whispers]"),
            ("bell \u{7}", "control character at position 6"),
            ("[laughs]", "only cues"),
            ("[sighs] [short pause]", "only cues"),
        ] {
            let error = validate_script(bad).err().unwrap_or_default();
            assert!(error.contains(needle), "{bad:?}: {error}");
        }
        assert!(validate_script(&"a".repeat(5_000)).is_ok());
        assert!(
            validate_script(&"a".repeat(5_001))
                .is_err_and(|error| error.contains("5001 characters"))
        );
        assert!(validate_script(&format!("[{}] hi", "x".repeat(121))).is_err());
    }

    #[test]
    fn spoken_words_drop_cues_and_collapse_whitespace() {
        assert_eq!(spoken_words("[excited] It works!"), "It works!");
        assert_eq!(
            spoken_words("[whispers] Just  between us.\n[/whispers] Now."),
            "Just between us. Now."
        );
        assert_eq!(spoken_words("[laughs]"), "");
        assert_eq!(spoken_words("broken [cue"), "broken [cue");
        let speech = Speech {
            script: "[warmly] Hi there".to_owned(),
            voice: None,
            pinned: None,
        };
        assert_eq!(speech.spoken_words(), "Hi there");
        assert!(
            speech.cache_key().is_none(),
            "no key before DM pins a voice"
        );
    }

    #[test]
    fn cache_keys_cover_model_voice_and_script() {
        let key = cache_key("eleven_v4", "voice", "Hello");
        assert_ne!(key, cache_key("eleven_v3", "voice", "Hello"));
        assert_ne!(key, cache_key("eleven_v4", "other", "Hello"));
        assert_ne!(key, cache_key("eleven_v4", "voice", "Hello!"));
        assert_eq!(key, cache_key("eleven_v4", "voice", "Hello"));
    }

    #[test]
    fn text_messages_become_safe_scripts() {
        assert_eq!(
            plain_text_script("See [docs] at https://example.com/x `now`"),
            "See (docs) at link now"
        );
        assert_eq!(plain_text_script("one\n\n  two  "), "one\ntwo");
        assert_eq!(plain_text_script("   "), "");
    }
}
