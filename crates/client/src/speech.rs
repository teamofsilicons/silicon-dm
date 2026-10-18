//! Spoken messages, checked on this machine before anything is sent: a script's square-bracket
//! cues, the words a listener hears, notes on what tends to sound wrong, WAV details, and the
//! audio formats DM transcribes (with a download that never carries DM credentials).
//!
//! DM speaks a Silicon's script through ElevenLabs v4: cues such as `[warmly]`, `[laughs]` or a
//! span `[whispers] … [/whispers]` steer the delivery and are not read aloud. The service sets a
//! spoken message's `voice_transcript` to [`spoken_words`], and the CLI applies its
//! 140-character rule for Carbons to those words, so both count the same way.
use std::fmt;
use std::time::Duration;

/// A script holds 1 to 5000 Unicode scalar values, cues included.
pub const MAX_SCRIPT_CHARACTERS: usize = 5000;
/// The largest audio DM transcribes in one request by default (`DM_TRANSCRIBE_MAX_BYTES`).
pub const DEFAULT_TRANSCRIBE_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// The audio types `POST /api/v1/speech/transcriptions` accepts.
pub const TRANSCRIPTION_TYPES: &[&str] = &[
    "audio/webm",
    "audio/ogg",
    "audio/wav",
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/flac",
];

/// A problem found locally, before (or instead of) asking the DM service: a script that cannot
/// be spoken as written, or audio that cannot be transcribed. It carries a stable `code` and, for
/// scripts, the rule (`reason`) and the 1-based character `position`, so a caller can fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechError {
    /// `invalid_speech_script`, `unsupported_audio_type`, `audio_too_large`,
    /// `insecure_audio_url` or `audio_download_failed`.
    pub code: &'static str,
    /// For `invalid_speech_script`: `empty`, `too_long`, `control_character`, `unclosed_cue`,
    /// `unopened_cue`, `nested_cue`, `empty_cue`, `long_cue`, `unmatched_closing_cue` or
    /// `no_words`.
    pub reason: Option<&'static str>,
    /// What is wrong, exactly.
    pub message: String,
    /// What to do about it.
    pub hint: String,
    /// Where in the script (1-based Unicode scalar values), when the problem has a place.
    pub position: Option<usize>,
}
impl SpeechError {
    /// The code of every script problem.
    pub const INVALID_SCRIPT: &'static str = "invalid_speech_script";

    fn script(reason: &'static str, position: Option<usize>, message: String, hint: &str) -> Self {
        Self {
            code: Self::INVALID_SCRIPT,
            reason: Some(reason),
            message,
            hint: hint.to_owned(),
            position,
        }
    }
    fn audio(code: &'static str, message: String, hint: impl Into<String>) -> Self {
        Self {
            code,
            reason: None,
            message,
            hint: hint.into(),
            position: None,
        }
    }
}
impl fmt::Display for SpeechError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SpeechError {}

/// The longest cue (the text between `[` and `]`) DM accepts: cues are short directions, and a
/// longer one is almost always a missing `]`.
pub const MAX_CUE_CHARACTERS: usize = 120;

const CUE_HINT: &str = "A cue is a short phrase in square brackets before the words it colours: [warmly] Thanks for waiting. A span opens and closes with the same words: [whispers] just between us [/whispers]. dm docs expressive has the rules and examples.";

/// One square-bracket cue of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    /// The words inside the brackets, trimmed (`whispers`, `short pause`), without the `/` of
    /// a closing cue.
    pub name: String,
    /// `[/name]`: the end of a span.
    pub closing: bool,
    /// 1-based position of the `[`.
    pub position: usize,
    /// The cue's text runs onto a new line (allowed, but usually a missing `]`).
    pub multiline: bool,
}

/// Cue names compare without regard to case or repeated spaces: `[Whispers]` … `[/whispers]`.
fn cue_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A few characters of the script from `position` on, for error messages.
fn excerpt(script: &str, position: usize) -> String {
    let text: String = script.chars().skip(position - 1).take(24).collect();
    let more = script.chars().count() > position - 1 + 24;
    let text = text.replace(['\n', '\r'], " ");
    if more { format!("{text}…") } else { text }
}

/// The cues of a script, in order, or the first bracket problem: a `[` without `]`, a `]`
/// without `[`, a `[` inside a cue, an empty cue, or a cue over [`MAX_CUE_CHARACTERS`].
///
/// # Errors
/// A [`SpeechError`] with code `invalid_speech_script`.
pub fn cues(script: &str) -> Result<Vec<Cue>, SpeechError> {
    let mut found = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (index, character) in script.chars().enumerate() {
        let position = index + 1;
        match (character, open.as_mut()) {
            ('[', None) => open = Some((position, String::new())),
            ('[', Some((start, _))) => {
                return Err(SpeechError::script(
                    "nested_cue",
                    Some(position),
                    format!(
                        "The `[` at character {position} opens a cue inside the cue that starts at character {start} (`{}`); a cue cannot contain brackets.",
                        excerpt(script, *start)
                    ),
                    "Close the first cue with `]` before opening the next: [sighs] [short pause] Okay.",
                ));
            }
            (']', None) => {
                return Err(SpeechError::script(
                    "unopened_cue",
                    Some(position),
                    format!(
                        "The `]` at character {position} closes a cue that was never opened (no `[` before it). Square brackets only mark cues in a script."
                    ),
                    CUE_HINT,
                ));
            }
            (']', Some((start, inner))) => {
                let start = *start;
                let multiline = inner.contains(['\n', '\r']);
                let inner = inner.trim();
                let (closing, name) = match inner.strip_prefix('/') {
                    Some(rest) => (true, rest.trim()),
                    None => (false, inner),
                };
                if name.is_empty() {
                    let shown = if closing { "[/]" } else { "[]" };
                    return Err(SpeechError::script(
                        "empty_cue",
                        Some(start),
                        format!(
                            "`{shown}` at character {start} is an empty cue: there is no direction inside the brackets."
                        ),
                        "Write the direction inside the brackets ([warmly], [short pause]) or remove them.",
                    ));
                }
                found.push(Cue {
                    name: name.to_owned(),
                    closing,
                    position: start,
                    multiline,
                });
                open = None;
            }
            (character, Some((start, inner))) => {
                inner.push(character);
                if inner.chars().count() > MAX_CUE_CHARACTERS {
                    return Err(SpeechError::script(
                        "long_cue",
                        Some(*start),
                        format!(
                            "The cue that starts at character {start} (`{}`) is longer than {MAX_CUE_CHARACTERS} characters without a closing `]`.",
                            excerpt(script, *start)
                        ),
                        "Close the cue with `]` where the direction ends; cues are a few words: [warmly], [short pause].",
                    ));
                }
            }
            _ => {}
        }
    }
    if let Some((start, _)) = open {
        return Err(SpeechError::script(
            "unclosed_cue",
            Some(start),
            format!(
                "The cue that starts at character {start} (`{}`) has no closing `]`.",
                excerpt(script, start)
            ),
            CUE_HINT,
        ));
    }
    Ok(found)
}

/// Spans that cross instead of nesting (`[a] … [b] … [/a] … [/b]`): the closing cue that came
/// too late, the cue it closes and the earlier closing cue that ended the span around it.
fn crossed_span(found: &[Cue]) -> Option<(&Cue, &Cue, &Cue)> {
    let mut open: Vec<&Cue> = Vec::new();
    let mut ended: Vec<(&Cue, &Cue)> = Vec::new();
    for cue in found {
        if !cue.closing {
            open.push(cue);
            continue;
        }
        let key = cue_key(&cue.name);
        if let Some(index) = open.iter().rposition(|opened| cue_key(&opened.name) == key) {
            for inner in open.drain(index + 1..) {
                ended.push((inner, cue));
            }
            open.pop();
        } else if let Some((inner, outer_close)) = ended
            .iter()
            .rev()
            .find(|(inner, _)| cue_key(&inner.name) == key)
        {
            return Some((cue, inner, outer_close));
        }
    }
    None
}

/// Checks that DM can speak a script as written, by the same rules the DM service applies:
/// 1 to 5000 characters, no control characters except line breaks and tabs, cues well formed
/// and at most 120 characters, and every `[/x]` closing an `[x]` opened before it (compared
/// without case). A cue that is never closed simply colours what follows it.
///
/// [`script_notes`] reports what is allowed but usually sounds wrong.
///
/// # Errors
/// A [`SpeechError`] with code `invalid_speech_script`, the rule broken (`reason`), where
/// (`position`) and how to fix it (`hint`).
pub fn validate_script(script: &str) -> Result<(), SpeechError> {
    let length = script.chars().count();
    if script.trim().is_empty() {
        return Err(SpeechError::script(
            "empty",
            None,
            "The script is empty: there is nothing to say.".into(),
            "Pass the words to speak: --speak '[warmly] The report is ready.'",
        ));
    }
    if length > MAX_SCRIPT_CHARACTERS {
        return Err(SpeechError::script(
            "too_long",
            Some(MAX_SCRIPT_CHARACTERS + 1),
            format!(
                "The script is {length} characters; a spoken message holds at most {MAX_SCRIPT_CHARACTERS}, cues included."
            ),
            "Split it into several spoken messages, one idea each; listeners follow short messages best.",
        ));
    }
    if let Some(position) = script
        .chars()
        .position(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(SpeechError::script(
            "control_character",
            Some(position + 1),
            format!(
                "The script has a control character (U+{:04X}) at character {}.",
                u32::from(script.chars().nth(position).unwrap_or_default()),
                position + 1
            ),
            "Use plain text, line breaks and tabs only.",
        ));
    }
    let found = cues(script)?;
    let mut open: Vec<String> = Vec::new();
    for cue in &found {
        let key = cue_key(&cue.name);
        if !cue.closing {
            open.push(key);
        } else if let Some(index) = open.iter().rposition(|opened| *opened == key) {
            open.remove(index);
        } else {
            return Err(SpeechError::script(
                "unmatched_closing_cue",
                Some(cue.position),
                format!(
                    "`[/{}]` at character {} closes a span that was never opened: there is no `[{}]` before it.",
                    cue.name, cue.position, cue.name
                ),
                "Open the span with the same words before the text it covers: [whispers] just between us [/whispers].",
            ));
        }
    }
    if spoken_words(script).is_empty() {
        // Checked against the Voice Agent on 2026-10-17: `[laughs]` or `[sighs]` alone fails
        // with FAILED_TO_SPEAK, while `[laughs] Ha!` renders the laugh.
        return Err(SpeechError::script(
            "no_words",
            None,
            "The script has cues but no words; the voice cannot render a cue on its own.".into(),
            "Give the cue something to colour: [laughs] Ha! for a laugh, [sighs] Okay. for a sigh.",
        ));
    }
    Ok(())
}

/// The words a listener hears: the script with every `[…]` cue replaced by a space and
/// whitespace collapsed, exactly as the DM service computes a spoken message's
/// `voice_transcript`. The 140-character rule for Carbons counts these words.
pub fn spoken_words(script: &str) -> String {
    let mut words = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(start) = rest.find('[') {
        words.push_str(&rest[..start]);
        words.push(' ');
        match rest[start..].find(']') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                // Never in a valid script; the rest stays words, as the service keeps it.
                rest = &rest[start..];
                break;
            }
        }
    }
    words.push_str(rest);
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Something in a script that usually sounds wrong when spoken. Notes never block a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptNote {
    /// `markdown`, `url`, `emoji`, `abbreviated_unit`, `stage_direction`, `long_cue`,
    /// `cue_line_break`, `span_end`, `crossed_cues` or `capitals`.
    pub code: &'static str,
    /// What was found, why it matters and what to write instead.
    pub message: String,
}

fn is_emoji(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE0F
    )
}

/// `3m`, `250ms`, `2.5h`, `40MB`: a number written straight against a short unit. `30s` is
/// left alone (it may be a decade), and so is `3D`.
fn abbreviated_unit(token: &str) -> bool {
    let digits = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let number = &token[..digits];
    if !number.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    match token[digits..].to_ascii_lowercase().as_str() {
        "s" => number.contains('.') || !number.ends_with('0'),
        unit => matches!(
            unit,
            "ms" | "sec"
                | "secs"
                | "m"
                | "min"
                | "mins"
                | "h"
                | "hr"
                | "hrs"
                | "kb"
                | "mb"
                | "gb"
                | "tb"
        ),
    }
}

const REACTIONS: &[&str] = &[
    "laughs",
    "laugh",
    "laughing",
    "sighs",
    "sigh",
    "pause",
    "pauses",
    "whispers",
    "whispering",
    "chuckles",
    "chuckle",
    "giggles",
    "coughs",
    "cough",
    "cries",
    "crying",
    "gasps",
    "smiles",
    "grins",
    "clears throat",
    "sniffs",
    "yawns",
];

/// Things in a script that usually sound wrong: Markdown, URLs, emoji, units written as `3m`,
/// stage directions in parentheses or asterisks, long cues, cues across a line break, words after
/// a closing cue, spans that cross, and runs of capitals. Each kind is reported once, in that
/// order. A malformed script is reported by [`validate_script`] instead.
pub fn script_notes(script: &str) -> Vec<ScriptNote> {
    let mut notes = Vec::new();
    let words = spoken_words(script);
    let markdown = script.contains('`')
        || script.contains("**")
        || script.contains("__")
        || script.contains("](")
        || script.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with('#') || line.starts_with("- ") || line.starts_with("> ")
        });
    if markdown {
        notes.push(ScriptNote {
            code: "markdown",
            message: "The script contains Markdown (backticks, **, #, list markers or links). Speech has no formatting: the symbols are dropped, and the structure with them. Write plain sentences.".into(),
        });
    }
    if ["http://", "https://", "www."]
        .iter()
        .any(|scheme| words.to_lowercase().contains(scheme))
    {
        notes.push(ScriptNote {
            code: "url",
            message: "The script contains a URL. It is read out piece by piece (one link stretched a sentence from 2.5 to 8 seconds). Say where to find it (\"the link is in the caption\") and put the URL in --text.".into(),
        });
    }
    if words.chars().any(is_emoji) {
        notes.push(ScriptNote {
            code: "emoji",
            message: "The script contains emoji, which are dropped from speech. Say it in words, or use a cue such as [laughs].".into(),
        });
    }
    if let Some(token) = words
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '!' | '?' | '(' | ')'))
        .map(|token| token.trim_end_matches(['.', ':']))
        .find(|token| abbreviated_unit(token))
    {
        notes.push(ScriptNote {
            code: "abbreviated_unit",
            message: format!(
                "`{token}` is expanded from context: \"took 3m\" was said as \"three minutes\" but \"is 3m long\" as \"three meters\". Spell the unit out when it matters: \"three minutes\", \"two hundred milliseconds\"."
            ),
        });
    }
    let lower = words.to_lowercase();
    let parenthesised = REACTIONS.iter().find(|reaction| {
        lower.contains(&format!("({reaction})")) || lower.contains(&format!("*{reaction}*"))
    });
    if let Some(reaction) = parenthesised {
        notes.push(ScriptNote {
            code: "stage_direction",
            message: format!(
                "`({reaction})` and `*{reaction}*` are not cues: the voice may perform them, drop them or read them out (`(pause)` paused, `*laughs*` vanished without a laugh). Write reactions as cues: [{reaction}]."
            ),
        });
    }
    if let Ok(found) = cues(script) {
        if let Some(cue) = found
            .iter()
            .find(|cue| cue.name.chars().count() > 40 || cue.name.split_whitespace().count() > 5)
        {
            notes.push(ScriptNote {
                code: "long_cue",
                message: format!(
                    "The cue at character {} is long. A long instruction can be read aloud as words (a 22-word cue was). Keep cues to a few words: [warmly], [short pause], [excited].",
                    cue.position
                ),
            });
        }
        if let Some(cue) = found.iter().find(|cue| cue.multiline) {
            notes.push(ScriptNote {
                code: "cue_line_break",
                message: format!(
                    "The cue at character {} runs onto a new line before its `]`; check that the `]` is where the direction ends.",
                    cue.position
                ),
            });
        }
        if let Some(cue) = found.iter().find(|cue| {
            let from_cue: String = script.chars().skip(cue.position - 1).collect();
            let after = from_cue.split_once(']').map_or("", |(_, after)| after);
            cue.closing && !spoken_words(after).is_empty()
        }) {
            notes.push(ScriptNote {
                code: "span_end",
                message: format!(
                    "`[/{}]` at character {} is followed by more words. A closing cue is only a hint: in checks the voice kept whispering (and kept its excitement) after the span ended. Put the coloured words last, or send them as their own message.",
                    cue.name, cue.position
                ),
            });
        }
        if let Some((late, inner, outer)) = crossed_span(&found) {
            notes.push(ScriptNote {
                code: "crossed_cues",
                message: format!(
                    "`[/{}]` at character {} closes `[{}]` from character {} after `[/{}]` at character {} ended the span around it. Spans that cross have no clear scope; nest them: [whispers] a [laughs] b [/laughs] c [/whispers].",
                    late.name, late.position, inner.name, inner.position, outer.name, outer.position
                ),
            });
        }
    }
    let mut run = 0;
    let mut shouted = None;
    for word in words.split_whitespace() {
        let letters: String = word.chars().filter(|c| c.is_alphabetic()).collect();
        if letters.chars().count() >= 2 && letters.chars().all(char::is_uppercase) {
            run += 1;
            if run == 3 {
                shouted = Some(word.to_owned());
                break;
            }
        } else {
            run = 0;
        }
    }
    if shouted.is_some() {
        notes.push(ScriptNote {
            code: "capitals",
            message: "Three or more words in a row are in capitals. Capitals change the delivery unpredictably (a held word, or nothing). Stress one word, or give it a short sentence of its own: \"Stop the deploy. Now.\"".into(),
        });
    }
    notes
}

/// The format of a WAV file DM returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// Bytes of audio after the header.
    pub data_bytes: u64,
    pub duration_milliseconds: u64,
}

/// Where a WAV's format and samples are: `(format code, channels, sample rate, bits per sample,
/// data offset, data length)`.
fn wav_layout(bytes: &[u8]) -> Option<(u16, u16, u32, u16, usize, usize)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let mut offset = 12;
    let mut format = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32_at(offset + 4)? as usize;
        let body = offset + 8;
        if id == b"fmt " {
            format = Some((
                u16_at(body)?,
                u16_at(body + 2)?,
                u32_at(body + 4)?,
                u16_at(body + 14)?,
            ));
        } else if id == b"data" {
            let (code, channels, sample_rate, bits) = format?;
            // A streamed WAV may carry a placeholder size; trust the bytes that are there.
            return Some((
                code,
                channels,
                sample_rate,
                bits,
                body,
                size.min(bytes.len() - body),
            ));
        }
        offset = body.checked_add(size)?.checked_add(size % 2)?;
    }
    None
}

/// Reads a RIFF/WAVE header (DM's speech is 24 kHz mono 16-bit PCM). `None` for anything else.
pub fn wav_info(bytes: &[u8]) -> Option<WavInfo> {
    let (_, channels, sample_rate, bits_per_sample, _, data) = wav_layout(bytes)?;
    let bytes_per_second =
        u64::from(sample_rate) * u64::from(channels) * u64::from(bits_per_sample) / 8;
    if bytes_per_second == 0 {
        return None;
    }
    Some(WavInfo {
        sample_rate,
        channels,
        bits_per_sample,
        data_bytes: data as u64,
        duration_milliseconds: data as u64 * 1000 / bytes_per_second,
    })
}

/// The silence [`with_leading_silence`] puts before speech that starts at once.
pub const TRANSCRIPTION_LEAD_MILLISECONDS: u64 = 400;

/// A copy of a 16-bit PCM WAV with `milliseconds` of silence in front, when its sound starts
/// within the first 100 ms; `None` when it is not such a WAV or already starts quietly.
///
/// Transcription can drop the first word, or everything, of audio that starts speaking at once,
/// as DM's rendered speech does. Checked on 2026-10-17 with Deepgram nova-3: three short renders
/// came back empty or without their first word, and complete with 400 ms of silence in front.
pub fn with_leading_silence(wav: &[u8], milliseconds: u64) -> Option<Vec<u8>> {
    let (code, channels, sample_rate, bits, offset, length) = wav_layout(wav)?;
    if code != 1 || bits != 16 || channels == 0 {
        return None;
    }
    let data = &wav[offset..offset + length];
    let frame = usize::from(channels) * 2;
    let window = (sample_rate as usize / 10) * frame;
    let head = &data[..window.min(data.len()) / 2 * 2];
    let samples = head.len() / 2;
    if samples == 0 {
        return None;
    }
    let energy: f64 = head
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f64::from(i16::from_le_bytes(*pair)).powi(2))
        .sum();
    // -50 dBFS: quieter than that is already silence.
    if (energy / samples as f64).sqrt() < 32768.0 * 0.00316 {
        return None;
    }
    let silence = (sample_rate as usize * milliseconds as usize / 1000) * frame;
    let total = silence + data.len();
    let mut padded = Vec::with_capacity(44 + total);
    padded.extend_from_slice(b"RIFF");
    padded.extend_from_slice(&u32::try_from(36 + total).ok()?.to_le_bytes());
    padded.extend_from_slice(b"WAVEfmt ");
    padded.extend_from_slice(&16_u32.to_le_bytes());
    padded.extend_from_slice(&1_u16.to_le_bytes());
    padded.extend_from_slice(&channels.to_le_bytes());
    padded.extend_from_slice(&sample_rate.to_le_bytes());
    padded.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    padded.extend_from_slice(&(channels * 2).to_le_bytes());
    padded.extend_from_slice(&16_u16.to_le_bytes());
    padded.extend_from_slice(b"data");
    padded.extend_from_slice(&u32::try_from(total).ok()?.to_le_bytes());
    padded.resize(44 + silence, 0);
    padded.extend_from_slice(data);
    Some(padded)
}

/// `m:ss.t` for a duration in milliseconds (`0:04.6`).
pub fn format_duration(milliseconds: u64) -> String {
    let tenths = milliseconds / 100;
    format!("{}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

/// The transcription type for a media type DM or an attachment host reports, ignoring
/// parameters and accepting common aliases (`audio/x-wav`, `audio/mp3`, `video/webm`…).
pub fn normalize_audio_type(content_type: &str) -> Option<&'static str> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Some(match essence.as_str() {
        "audio/webm" | "video/webm" => "audio/webm",
        "audio/ogg" | "application/ogg" | "audio/opus" | "audio/x-opus+ogg" => "audio/ogg",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "audio/wav",
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" => "audio/mpeg",
        "audio/mp4" | "video/mp4" => "audio/mp4",
        "audio/x-m4a" | "audio/m4a" => "audio/x-m4a",
        "audio/aac" | "audio/x-aac" | "audio/aacp" => "audio/aac",
        "audio/flac" | "audio/x-flac" => "audio/flac",
        _ => return None,
    })
}

/// The transcription type a file name suggests (`.wav`, `.webm`, `.ogg`, `.opus`, `.mp3`,
/// `.mp4`, `.m4a`, `.aac`, `.flac`).
pub fn audio_type_for_name(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "webm" | "weba" => "audio/webm",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" | "wave" => "audio/wav",
        "mp3" | "mpga" => "audio/mpeg",
        "mp4" => "audio/mp4",
        "m4a" => "audio/x-m4a",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// The transcription type the first bytes of a file show, when they are a known audio format.
pub fn sniff_audio_type(bytes: &[u8]) -> Option<&'static str> {
    let starts = |prefix: &[u8]| bytes.starts_with(prefix);
    if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    if starts(b"OggS") {
        return Some("audio/ogg");
    }
    if starts(b"fLaC") {
        return Some("audio/flac");
    }
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some("audio/webm");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(if &bytes[8..11] == b"M4A" {
            "audio/x-m4a"
        } else {
            "audio/mp4"
        });
    }
    if starts(b"ID3") {
        return Some("audio/mpeg");
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0 {
        // MPEG frame sync: layer bits 00 are ADTS (AAC), anything else MPEG audio (MP3).
        return Some(if (bytes[1] >> 1) & 0x03 == 0 {
            "audio/aac"
        } else {
            "audio/mpeg"
        });
    }
    None
}

/// The URL without its query or fragment, for messages (a signed link's query is a secret).
pub fn display_url(url: &url::Url) -> String {
    let mut shown = url.clone();
    shown.set_query(None);
    shown.set_fragment(None);
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.to_string()
}

/// Audio downloaded for transcription.
#[derive(Clone, Debug)]
pub struct FetchedAudio {
    pub bytes: Vec<u8>,
    /// One of [`TRANSCRIPTION_TYPES`]: from the host's `Content-Type`, else the file's first
    /// bytes, else its name.
    pub content_type: &'static str,
    /// Where it was finally read from, after redirects.
    pub url: url::Url,
}

/// Where [`fetch_audio_from`] may download from. A message's links are the sender's to choose,
/// so a download must never become a way for the sender to make your machine read something
/// only it can reach (this machine's own services, a private network, a cloud metadata
/// address), not even through a redirect or a name that resolves there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioHosts {
    /// Public internet hosts only, over https: what to use with any DM service elsewhere.
    Public,
    /// Also this machine and private networks (plain http only on this machine): for a DM
    /// service on this machine, in local development, whose files live on this machine too.
    AlsoLocal,
}

/// Whether an address is on the public internet: not this machine, a private or shared
/// network, a link-local, documentation, benchmarking, multicast or reserved range.
#[must_use]
pub fn is_public_address(address: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match address {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (18..20).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_address(IpAddr::V4(v4));
            }
            let segments = v6.segments();
            // NAT64 (64:ff9b::/96) carries an IPv4 address.
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [_, _, _, _, _, _, high, low] = segments;
                let embedded = std::net::Ipv4Addr::new(
                    (high >> 8) as u8,
                    high as u8,
                    (low >> 8) as u8,
                    low as u8,
                );
                return is_public_address(IpAddr::V4(embedded));
            }
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique local
                || (segments[0] & 0xffc0) == 0xfe80 // link-local
                || (segments[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
                || (segments[0] == 0x0100 && segments[1..4] == [0, 0, 0]) // discard
                || segments[..6] == [0, 0, 0, 0, 0, 0]) // IPv4-compatible (deprecated)
        }
    }
}

fn private_host(url: &url::Url) -> SpeechError {
    SpeechError::audio(
        "insecure_audio_url",
        format!(
            "{} points at this machine or a private network; dm downloads a message's audio only from public hosts, so a sender's link cannot make it read what only this machine can reach.",
            display_url(url)
        ),
        "Download the file yourself if you trust it, and run dm speech transcribe FILE.",
    )
}

fn allowed_audio_url(url: &url::Url, hosts: AudioHosts) -> Result<(), SpeechError> {
    let plain_local =
        url.scheme() == "http" && hosts == AudioHosts::AlsoLocal && crate::is_loopback(url);
    if url.scheme() != "https" && !plain_local {
        return Err(SpeechError::audio(
            "insecure_audio_url",
            format!(
                "{} is not an https link; dm downloads audio only over https (plain http only from this machine, in local development).",
                display_url(url)
            ),
            "Ask the sender for an https link, or download the file yourself and run dm speech transcribe FILE.",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SpeechError::audio(
            "insecure_audio_url",
            "The audio link carries a user name or password; dm does not send credentials to attachment hosts.".into(),
            "Download the file yourself and run dm speech transcribe FILE.",
        ));
    }
    if hosts == AudioHosts::Public {
        let local = match url.host() {
            Some(url::Host::Domain(domain)) => {
                let domain = domain.trim_end_matches('.').to_ascii_lowercase();
                domain == "localhost" || domain.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(ip)) => !is_public_address(ip.into()),
            Some(url::Host::Ipv6(ip)) => !is_public_address(ip.into()),
            None => true,
        };
        if local {
            return Err(private_host(url));
        }
    }
    Ok(())
}

/// Resolves host names for audio downloads keeping public addresses only, so a name that
/// resolves (or is made to resolve) to this machine or a private network is never connected
/// to. Literal addresses are checked by [`allowed_audio_url`].
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| is_public_address(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(
                    format!("{host} resolves only to this machine or private networks").into(),
                );
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Downloads audio to transcribe from a public attachment host, at most `max_bytes`: as
/// [`fetch_audio_from`] with [`AudioHosts::Public`].
///
/// # Errors
/// As [`fetch_audio_from`].
pub async fn fetch_audio(url: &str, max_bytes: u64) -> Result<FetchedAudio, SpeechError> {
    fetch_audio_from(url, max_bytes, AudioHosts::Public).await
}

/// Downloads audio to transcribe from an attachment host, at most `max_bytes`.
///
/// The request is a plain GET from a separate HTTP client: it never carries DM's credentials,
/// cookies or contract headers, follows at most 5 redirects, and gives up after 10 seconds to
/// connect or 5 minutes in total. With [`AudioHosts::Public`] every hop must be https to a
/// public host, and host names are connected to only at the public addresses they resolve to.
/// The answer must be audio DM can transcribe, by its `Content-Type`, or by its first bytes
/// when the host only says `application/octet-stream`.
///
/// # Errors
/// [`SpeechError`] with `insecure_audio_url`, `audio_too_large`, `audio_download_failed` or
/// `unsupported_audio_type`.
pub async fn fetch_audio_from(
    url: &str,
    max_bytes: u64,
    hosts: AudioHosts,
) -> Result<FetchedAudio, SpeechError> {
    let url = url::Url::parse(url).map_err(|error| {
        SpeechError::audio(
            "audio_download_failed",
            format!("The audio link is not a valid URL ({error})."),
            "dm messages show CONVERSATION MESSAGE lists the message's links.",
        )
    })?;
    allowed_audio_url(&url, hosts)?;
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("more than 5 redirects")
        } else if allowed_audio_url(attempt.url(), hosts).is_err() {
            attempt.error("a redirect went to a link dm does not download from (not https, or this machine or a private network)")
        } else {
            attempt.follow()
        }
    });
    let mut builder = reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("silicon-dm-client/", env!("CARGO_PKG_VERSION")));
    if hosts == AudioHosts::Public {
        builder = builder.dns_resolver(std::sync::Arc::new(PublicOnly));
    }
    let http = builder.build().map_err(|error| {
        SpeechError::audio(
            "audio_download_failed",
            format!("dm could not prepare the download: {error}"),
            "Retry; if it persists, report it with dm report.",
        )
    })?;
    let shown = display_url(&url);
    let failed = |what: String| {
        SpeechError::audio(
            "audio_download_failed",
            what,
            "Check that the link still works in a browser; dm speech transcribe FILE transcribes a copy you have.",
        )
    };
    let mut response = http.get(url.clone()).send().await.map_err(|error| {
        failed(format!(
            "Downloading {shown} failed: {}",
            error.without_url()
        ))
    })?;
    if !response.status().is_success() {
        return Err(failed(format!(
            "The attachment host answered HTTP {} for {shown}.",
            response.status().as_u16()
        )));
    }
    let too_large = |size: u64| {
        SpeechError::audio(
            "audio_too_large",
            format!(
                "The audio at {shown} is {} MiB; DM transcribes at most {} MiB in one request.",
                size.div_ceil(1024 * 1024),
                max_bytes / (1024 * 1024)
            ),
            "Transcribe a shorter recording (split it with any audio editor).",
        )
    };
    if let Some(length) = response.content_length()
        && length > max_bytes
    {
        return Err(too_large(length));
    }
    let declared = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let final_url = response.url().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        failed(format!(
            "Downloading {shown} failed: {}",
            error.without_url()
        ))
    })? {
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(too_large(bytes.len() as u64 + chunk.len() as u64));
        }
        bytes.extend_from_slice(&chunk);
    }
    let content_type = declared
        .as_deref()
        .and_then(normalize_audio_type)
        .or_else(|| sniff_audio_type(&bytes))
        .or_else(|| audio_type_for_name(final_url.path()))
        .ok_or_else(|| {
            SpeechError::audio(
                "unsupported_audio_type",
                format!(
                    "{shown} is {}, not audio DM can transcribe ({}).",
                    declared.as_deref().unwrap_or("of an unknown type"),
                    TRANSCRIPTION_TYPES.join(", ")
                ),
                "Convert it to one of those formats and run dm speech transcribe FILE.",
            )
        })?;
    Ok(FetchedAudio {
        bytes,
        content_type,
        url: final_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(script: &str) -> Option<&'static str> {
        validate_script(script).err().and_then(|error| error.reason)
    }

    #[test]
    fn well_formed_scripts_pass() {
        for script in [
            "Done.",
            "[warmly] Thanks for waiting.",
            "[whispers] just between us [/whispers] Now, back to work.",
            "[Whispers] quiet [/whispers  ] loud",
            "[laughs] a [laughs] b [/laughs]",
            "[whispers] a [laughs] b [/whispers] c",
            "[excited] It works! [short pause] [sighs] Finally.",
            "Shipping [strong British accent] Thursday [/strong British accent].",
            // Allowed, with a note: a cue across a line, spans that cross.
            "Hi [warmly\n] there",
            "[a] x [b] y [/a] z [/b]",
        ] {
            assert_eq!(validate_script(script), Ok(()), "{script}");
        }
    }

    #[test]
    fn bracket_problems_name_the_rule_and_the_place() {
        for (script, expected, position) in [
            ("", "empty", None),
            ("   \n", "empty", None),
            ("Hi [warmly there", "unclosed_cue", Some(4)),
            ("Hi] there", "unopened_cue", Some(3)),
            ("[a [b] c", "nested_cue", Some(4)),
            ("Hi [] there", "empty_cue", Some(4)),
            ("Hi [ / ] there", "empty_cue", Some(4)),
            ("bell \u{7}", "control_character", Some(6)),
            ("Hi there [/whispers]", "unmatched_closing_cue", Some(10)),
            ("[laughs]", "no_words", None),
            ("[sighs] [short pause]", "no_words", None),
        ] {
            let error = validate_script(script).expect_err(script);
            assert_eq!(error.code, "invalid_speech_script");
            assert_eq!(error.reason, Some(expected), "{script}: {}", error.message);
            assert_eq!(error.position, position, "{script}: {}", error.message);
            assert!(!error.hint.is_empty());
        }
        let long = "a".repeat(MAX_SCRIPT_CHARACTERS + 1);
        assert_eq!(reason(&long), Some("too_long"));
        assert_eq!(
            reason(&format!("[{}] hi", "x".repeat(MAX_CUE_CHARACTERS))),
            None
        );
        assert_eq!(
            reason(&format!("[{}] hi", "x".repeat(MAX_CUE_CHARACTERS + 1))),
            Some("long_cue")
        );
        assert_eq!(reason(&"界".repeat(MAX_SCRIPT_CHARACTERS)), None);
    }

    #[test]
    fn spoken_words_drop_cues_and_collapse_whitespace() {
        for (script, words) in [
            ("[excited] It works!", "It works!"),
            (
                "[whispers]  just between us [/whispers]\nNow,   back.",
                "just between us Now, back.",
            ),
            ("Hello[laughs]world", "Hello world"),
            ("[laughs]", ""),
            ("no cues at all", "no cues at all"),
        ] {
            assert_eq!(spoken_words(script), words, "{script}");
        }
    }

    #[test]
    fn notes_flag_what_sounds_wrong_once_each() {
        let codes = |script: &str| {
            script_notes(script)
                .into_iter()
                .map(|note| note.code)
                .collect::<Vec<_>>()
        };
        assert!(codes("[warmly] The build took three minutes.").is_empty());
        assert_eq!(codes("The **build** passed"), ["markdown"]);
        assert_eq!(codes("See https://example.com/a now"), ["url"]);
        assert_eq!(codes("Shipped 🚀"), ["emoji"]);
        assert_eq!(codes("The build took 3m."), ["abbreviated_unit"]);
        assert_eq!(codes("Took 250ms, then 2.5h"), ["abbreviated_unit"]);
        assert_eq!(codes("It took 45s"), ["abbreviated_unit"]);
        assert!(codes("Music from the 1990s and 3D printing").is_empty());
        assert_eq!(codes("That was close (laughs)"), ["stage_direction"]);
        assert_eq!(
            codes("[say this slowly and with great warmth please] Hi"),
            ["long_cue"]
        );
        assert_eq!(codes("THIS IS DONE now"), ["capitals"]);
        assert_eq!(codes("Hi [warmly\n] there"), ["cue_line_break"]);
        assert_eq!(
            codes("[whispers] a secret [/whispers] Now, back."),
            ["span_end"]
        );
        assert!(codes("Now, back. [whispers] a secret [/whispers]").is_empty());
        let crossed = script_notes("[a] x [b] y [/a] z [/b]");
        let note = crossed
            .iter()
            .find(|note| note.code == "crossed_cues")
            .expect("crossed spans are noted");
        assert!(
            note.message.contains("`[/b]` at character 20"),
            "{}",
            note.message
        );
        assert!(codes("The CLI and API are DONE").is_empty());
    }

    #[test]
    fn wav_headers_give_the_duration() {
        let mut wav = Vec::new();
        let pcm = vec![0_u8; 48_000 * 2 + 24_000];
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&24_000_u32.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        let info = wav_info(&wav).expect("a WAV");
        assert_eq!(
            (info.sample_rate, info.channels, info.bits_per_sample),
            (24_000, 1, 16)
        );
        assert_eq!(info.duration_milliseconds, 2500);
        assert_eq!(format_duration(info.duration_milliseconds), "0:02.5");
        assert_eq!(format_duration(61_250), "1:01.2");
        assert!(wav_info(b"not a wav file").is_none());
        assert_eq!(sniff_audio_type(&wav), Some("audio/wav"));
        // Silence is already quiet: nothing to add.
        assert!(with_leading_silence(&wav, 400).is_none());
        let mut loud = wav.clone();
        for sample in loud[44..].as_chunks_mut::<2>().0 {
            *sample = 8000_i16.to_le_bytes();
        }
        let padded = with_leading_silence(&loud, 400).expect("speech that starts at once");
        let info = wav_info(&padded).expect("still a WAV");
        assert_eq!(info.duration_milliseconds, 2500 + 400);
        assert!(padded[44..44 + 19_200].iter().all(|byte| *byte == 0));
        assert_eq!(&padded[44 + 19_200..], &loud[44..]);
    }

    #[test]
    fn audio_types_come_from_headers_names_or_bytes() {
        assert_eq!(normalize_audio_type("audio/x-wav"), Some("audio/wav"));
        assert_eq!(
            normalize_audio_type("Audio/Webm; codecs=opus"),
            Some("audio/webm")
        );
        assert_eq!(normalize_audio_type("application/octet-stream"), None);
        assert_eq!(audio_type_for_name("/notes/voice.M4A"), Some("audio/x-m4a"));
        assert_eq!(audio_type_for_name("voice.txt"), None);
        assert_eq!(sniff_audio_type(b"OggS\0\x02"), Some("audio/ogg"));
        assert_eq!(sniff_audio_type(b"ID3\x04"), Some("audio/mpeg"));
        assert_eq!(sniff_audio_type(&[0xFF, 0xFB, 0x90]), Some("audio/mpeg"));
        assert_eq!(sniff_audio_type(&[0xFF, 0xF1, 0x50]), Some("audio/aac"));
        assert_eq!(
            sniff_audio_type(b"\0\0\0\x20ftypM4A \0\0"),
            Some("audio/x-m4a")
        );
        assert_eq!(sniff_audio_type(b"%PDF-1.7"), None);
        let signed =
            url::Url::parse("https://files.example/a.ogg?X-Amz-Signature=secret#t=1").expect("url");
        assert_eq!(display_url(&signed), "https://files.example/a.ogg");
    }

    #[tokio::test]
    async fn downloads_refuse_plain_http_and_never_carry_credentials() {
        let refused = fetch_audio("http://files.example/a.ogg", 1024)
            .await
            .expect_err("plain http");
        assert_eq!(refused.code, "insecure_audio_url");
        let refused = fetch_audio("https://user:pw@files.example/a.ogg", 1024)
            .await
            .expect_err("credentials");
        assert_eq!(refused.code, "insecure_audio_url");
    }

    #[test]
    fn only_public_addresses_count_as_public() {
        for private in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.1.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "64:ff9b::a00:1",
        ] {
            let address: std::net::IpAddr = private.parse().expect("an address");
            assert!(!is_public_address(address), "{private}");
        }
        for public in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700::1111",
            "64:ff9b::808:808",
        ] {
            let address: std::net::IpAddr = public.parse().expect("an address");
            assert!(is_public_address(address), "{public}");
        }
    }

    /// A sender's link may not make the reader's machine fetch from itself or a private
    /// network, directly, through a name or through a redirect.
    #[tokio::test]
    async fn downloads_stay_on_public_hosts_unless_the_service_is_local() {
        for link in [
            "https://127.0.0.1/a.ogg",
            "https://10.0.0.5/a.ogg",
            "https://[::1]/a.ogg",
            "https://169.254.169.254/latest/meta-data",
            "https://localhost/a.ogg",
            "https://files.localhost/a.ogg",
            "http://127.0.0.1:4541/a.ogg",
        ] {
            let refused = fetch_audio(link, 1024).await.expect_err(link);
            assert_eq!(refused.code, "insecure_audio_url", "{link}");
        }
        // Every redirect hop is held to the same rule (the policy checks each one), and a
        // name is only connected to at the public addresses it resolves to.
        let hop = url::Url::parse("http://127.0.0.1:47631/internal.wav").expect("a link");
        assert!(allowed_audio_url(&hop, AudioHosts::Public).is_err());
        assert!(allowed_audio_url(&hop, AudioHosts::AlsoLocal).is_ok());
        let name: reqwest::dns::Name = "localhost".parse().expect("a name");
        let resolved = reqwest::dns::Resolve::resolve(&PublicOnly, name).await;
        assert!(resolved.is_err(), "localhost has no public address");
        // In local development (a DM service on this machine) a local file still downloads.
        let local = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let port = local.local_addr().expect("an address").port();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut stream, _)) = local.accept().await {
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer).await;
                let wav = [b"RIFF".as_slice(), &[36, 0, 0, 0], b"WAVEfmt "].concat();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n",
                    wav.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&wav).await;
            }
        });
        let fetched = fetch_audio_from(
            &format!("http://127.0.0.1:{port}/note.wav"),
            1024 * 1024,
            AudioHosts::AlsoLocal,
        )
        .await
        .expect("a local file in local development");
        assert_eq!(fetched.content_type, "audio/wav");
        server.abort();
    }
}
