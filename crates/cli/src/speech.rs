//! `dm voices`, `dm speech`, `dm messages listen|transcribe`, and the voice check before a
//! spoken message is queued. These call the DM service directly instead of through the relay:
//! they move audio (the relay keeps JSON results) or need their answer now.
use crate::Outcome;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use silicon_dm_client::{
    Client, SpeechAudio, VoiceCatalog,
    runtime::{
        LocalRuntime,
        store::{self, Config, SessionError},
    },
    speech,
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn coded(code: &'static str, message: String, hint: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SessionError {
        code,
        message,
        hint: hint.into(),
    })
}

/// The DM service a profile talks to, signed in or not; else `DM_API_URL`; else production.
pub fn api_url(config: &Config, profile: &str) -> String {
    config
        .profiles
        .get(&store::session_key(profile))
        .map(|stored| stored.base_url.clone())
        .or_else(|| {
            std::env::var("DM_API_URL")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| store::DEFAULT_API_URL.to_owned())
}

/// The voice catalog; a service without the voices route predates speech.
async fn catalog(base: &str) -> Result<VoiceCatalog> {
    match Client::new(base)?.voices().await {
        Ok(catalog) => Ok(catalog),
        Err(silicon_dm_client::Error::Api {
            status: 404, code, ..
        }) if code == "not_found" || code == "http_error" => Err(coded(
            "speech_not_supported",
            format!(
                "The DM service at {base} has no voices: spoken messages, listening and transcription need DM 0.17 or later (it answered 404 for GET /api/v1/voices)."
            ),
            "Point DM_API_URL (or dm login --api-url) at a DM 0.17 service. Text messages work as before.",
        )),
        Err(error) => Err(error.into()),
    }
}

fn unknown_voice(key: &str, base: &str, catalog: &VoiceCatalog) -> anyhow::Error {
    coded(
        "voice_not_found",
        format!(
            "There is no voice `{key}` at the DM service at {base}; its voices are {}.",
            catalog.keys().join(", ")
        ),
        "dm voices list describes each one; leave out --voice to speak in your own voice.",
    )
}

/// A voice key as typed (`Brittney ` is `brittney`).
pub fn voice_key(voice: &str) -> String {
    voice.trim().to_lowercase()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedCatalog {
    api_url: String,
    fetched_at: u64,
    keys: Vec<String>,
}
/// How long a spoken send trusts the voice list it fetched; a key not in it fetches again.
const CATALOG_SECONDS: u64 = 3600;

/// Before a spoken message is queued: the service speaks (DM 0.17 or later, with speech switched
/// on) and the voice it names exists, so a typo fails here instead of later in the queue, and a
/// service without speech does not hold the message in endless retries (its 503 is retryable).
/// The voice list of a service that speaks is kept for an hour in the home (`voices.json`). When
/// the service cannot be reached the message is queued anyway, as every send is; the relay sends
/// it when it can.
pub async fn check_spoken_send(
    local: &LocalRuntime,
    base: &str,
    voice: Option<&str>,
) -> Result<()> {
    let path = local.store().directory().join("voices.json");
    let cached = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CachedCatalog>(&bytes).ok())
        .filter(|cached| {
            cached.api_url == base
                && store::now().saturating_sub(cached.fetched_at) < CATALOG_SECONDS
        });
    if let Some(cached) = &cached
        && voice.is_none_or(|voice| cached.keys.iter().any(|key| key == voice))
    {
        return Ok(());
    }
    let catalog = match tokio::time::timeout(std::time::Duration::from_secs(5), catalog(base)).await
    {
        Ok(Ok(catalog)) => catalog,
        Ok(Err(error))
            if error
                .downcast_ref::<SessionError>()
                .is_some_and(|reason| reason.code == "speech_not_supported") =>
        {
            return Err(error);
        }
        Ok(Err(_)) | Err(_) => {
            if voice.is_some() {
                eprintln!(
                    "dm could not check that voice with the DM service at {base} now; the message is queued and the relay sends it when the service answers."
                );
            }
            return Ok(());
        }
    };
    if catalog.enabled == Some(false) {
        return Err(coded(
            "speech_not_configured",
            format!(
                "The DM service at {base} has speech switched off (no speech provider is configured), so a spoken message cannot be sent there; nothing was queued."
            ),
            "Send it as text (--text), or ask the service's operators to set DM_DEEPGRAM_API_KEY.",
        ));
    }
    let record = CachedCatalog {
        api_url: base.to_owned(),
        fetched_at: store::now(),
        keys: catalog.keys().into_iter().map(str::to_owned).collect(),
    };
    // A cache that cannot be written only costs the next send one more request.
    let _ = write_private(&path, &serde_json::to_vec(&record)?);
    match voice {
        Some(voice) if catalog.get(voice).is_none() => Err(unknown_voice(voice, base, &catalog)),
        _ => Ok(()),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = store::secure_open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Notes on a script, one line each on standard error (they never block a send).
pub fn print_notes(script: &str) {
    for note in speech::script_notes(script) {
        eprintln!("Speech note ({}): {}", note.code, note.message);
    }
}

fn notes_json(script: &str) -> Value {
    json!(
        speech::script_notes(script)
            .into_iter()
            .map(|note| json!({"code": note.code, "message": note.message}))
            .collect::<Vec<_>>()
    )
}

/// `dm voices list`: the catalog, and the signed-in account's voice when there is one.
pub async fn list(local: &LocalRuntime, profile: &str, config: &Config) -> Result<Value> {
    let base = api_url(config, profile);
    let catalog = catalog(&base).await?;
    let mut value = serde_json::to_value(&catalog)?;
    let account = store::profile(config, profile)
        .ok()
        .and_then(|stored| stored.account.as_ref().map(|account| account.uuid.clone()));
    match account {
        Some(uuid) => {
            let uuid = uuid.as_str();
            match local
                .with_client(profile, |client| async move { client.voice(uuid).await })
                .await
            {
                Ok(mine) => {
                    for item in value["items"].as_array_mut().into_iter().flatten() {
                        if item["key"] == mine.voice.key.as_str() {
                            item["yours"] = json!(true);
                        }
                    }
                    value["you"] = serde_json::to_value(&mine)?;
                }
                Err(error) => {
                    value["you"] = Value::Null;
                    value["note"] = json!(format!("Your own voice could not be read: {error:#}"));
                }
            }
        }
        None => {
            value["you"] = Value::Null;
            value["note"] =
                json!("Signed out: sign in to see which voice is yours (dm login status --json).");
        }
    }
    value["next"] = json!("dm voices preview KEY; dm voices set KEY");
    Ok(value)
}

/// `--out`, `--force` and `--json` of a command that writes a WAV.
pub struct WavOutput {
    pub out: Option<PathBuf>,
    pub force: bool,
    pub compact: bool,
}

/// Where a WAV goes: standard output, or a file written atomically that is never replaced
/// without `--force`.
enum Target {
    Stdout,
    File {
        path: PathBuf,
        force: bool,
    },
    /// A device or a named pipe (`/dev/null`, a FIFO): written into, never replaced.
    Special(PathBuf),
}
impl Target {
    /// Checks the destination before anything is rendered, so a refusal costs nothing.
    fn new(path: &Path, force: bool) -> Result<Self> {
        if path == Path::new("-") {
            return Ok(Self::Stdout);
        }
        let path = std::path::absolute(path)
            .with_context(|| format!("cannot resolve {}", path.display()))?;
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if metadata.is_dir() {
                return Err(coded(
                    "output_is_directory",
                    format!("{} is a directory; nothing was written.", path.display()),
                    "Name a file with --out, e.g. --out speech.wav.",
                ));
            }
            // Renaming over /dev/null or a pipe would replace it; such outputs are streams.
            if !metadata.is_file()
                && !metadata.file_type().is_symlink()
                && std::fs::metadata(&path).is_ok_and(|target| !target.is_dir())
            {
                return Ok(Self::Special(path));
            }
            if !force {
                return Err(exists(&path));
            }
        }
        match path.parent() {
            Some(directory) if directory.is_dir() => {}
            _ => {
                return Err(coded(
                    "output_directory_missing",
                    format!(
                        "The directory of {} does not exist; nothing was written.",
                        path.display()
                    ),
                    "Create the directory first, or choose another --out.",
                ));
            }
        }
        Ok(Self::File { path, force })
    }
    fn is_stdout(&self) -> bool {
        matches!(self, Self::Stdout)
    }
    /// Writes the bytes: through a private temporary file in the same directory, then renamed
    /// (or linked, which fails rather than replace a file that appeared meanwhile).
    fn write(&self, bytes: &[u8]) -> Result<Option<PathBuf>> {
        let (path, force) = match self {
            Self::Stdout => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(bytes)?;
                stdout.flush()?;
                return Ok(None);
            }
            Self::Special(path) => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|mut stream| stream.write_all(bytes))
                    .with_context(|| format!("cannot write {}", path.display()))?;
                return Ok(Some(path.clone()));
            }
            Self::File { path, force } => (path, *force),
        };
        let directory = path.parent().context("the output has no directory")?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "speech.wav".into());
        let temporary = directory.join(format!(".{name}.{}.part", uuid::Uuid::new_v4().simple()));
        let written = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .with_context(|| format!("cannot write in {}", directory.display()))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        let placed = if force {
            std::fs::rename(&temporary, path).map_err(anyhow::Error::from)
        } else {
            match std::fs::hard_link(&temporary, path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&temporary);
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(exists(path));
                }
                // A filesystem without hard links: check, then rename.
                Err(_) if !path.exists() => {
                    std::fs::rename(&temporary, path).map_err(anyhow::Error::from)
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(exists(path));
                }
            }
        };
        if let Err(error) = placed {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.context(format!("cannot write {}", path.display())));
        }
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(directory) {
            let _ = directory.sync_all();
        }
        Ok(Some(path.clone()))
    }
}

fn exists(path: &Path) -> anyhow::Error {
    coded(
        "file_exists",
        format!("{} already exists; nothing was written.", path.display()),
        "Pass --force to replace it, or choose another file with --out.",
    )
}

/// What was written, for the JSON summary.
fn written(path: Option<&Path>, audio: &SpeechAudio) -> Value {
    let duration = audio.duration_milliseconds;
    let mut summary = json!({
        "path": path.map(|path| path.display().to_string()),
        "bytes": audio.wav.len(),
        "content_type": audio.content_type,
        "duration_milliseconds": duration,
        "duration": duration.map(speech::format_duration),
        "voice": audio.voice,
        "cache": audio.cache,
    });
    if let Some(path) = path.filter(|path| path.is_file()) {
        summary["next"] = json!(format!(
            "Play it with any audio player (macOS: afplay '{}').",
            path.display()
        ));
    }
    summary
}

/// The summary goes to standard output, or to standard error when the WAV went there.
fn finish(summary: Value, to_stdout: bool, compact: bool) -> Outcome {
    if !to_stdout {
        return Outcome::ok(summary);
    }
    let text = if compact {
        serde_json::to_string(&summary)
    } else {
        serde_json::to_string_pretty(&summary)
    };
    eprintln!("{}", text.unwrap_or_default());
    Outcome::printed()
}

/// `dm voices preview`.
pub async fn preview(
    local: &LocalRuntime,
    profile: &str,
    config: &Config,
    voice: &str,
    script: Option<String>,
    output: WavOutput,
) -> Result<Outcome> {
    let key = voice_key(voice);
    let base = api_url(config, profile);
    let catalog = catalog(&base).await?;
    let found = catalog
        .get(&key)
        .ok_or_else(|| unknown_voice(&key, &base, &catalog))?;
    let script = script.unwrap_or_else(|| {
        format!(
            "Hi, I'm {}. This is how I'll sound when you listen to my messages.",
            found.name
        )
    });
    speech::validate_script(&script)?;
    let target = Target::new(
        &output
            .out
            .unwrap_or_else(|| PathBuf::from(format!("dm-voice-{}.wav", found.key))),
        output.force,
    )?;
    let (script_ref, key_ref) = (script.as_str(), found.key.as_str());
    let audio = local
        .with_client(profile, |client| async move {
            client.render_speech(script_ref, Some(key_ref)).await
        })
        .await?;
    let path = target.write(&audio.wav)?;
    let mut summary = written(path.as_deref(), &audio);
    summary["voice_name"] = json!(found.name);
    summary["script"] = json!(script);
    Ok(finish(summary, target.is_stdout(), output.compact))
}

/// FNV-1a: a short stable name for a rendered script, without another dependency.
fn short_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", hash >> 32)
}

/// `dm speech render`.
pub async fn render(
    local: &LocalRuntime,
    profile: &str,
    config: &Config,
    script: String,
    voice: Option<String>,
    output: WavOutput,
) -> Result<Outcome> {
    speech::validate_script(&script)?;
    let voice = voice.as_deref().map(voice_key);
    if let Some(key) = &voice {
        let base = api_url(config, profile);
        let catalog = catalog(&base).await?;
        if catalog.get(key).is_none() {
            return Err(unknown_voice(key, &base, &catalog));
        }
    }
    let name = format!(
        "dm-speech-{}.wav",
        short_hash(&format!("{}|{script}", voice.as_deref().unwrap_or("own")))
    );
    let target = Target::new(
        &output.out.unwrap_or_else(|| PathBuf::from(name)),
        output.force,
    )?;
    let (script_ref, voice_ref) = (script.as_str(), voice.as_deref());
    let audio = local
        .with_client(profile, |client| async move {
            client.render_speech(script_ref, voice_ref).await
        })
        .await?;
    let path = target.write(&audio.wav)?;
    let mut summary = written(path.as_deref(), &audio);
    let words = speech::spoken_words(&script);
    summary["script"] = json!({
        "characters": script.chars().count(),
        "spoken_words": words,
        "spoken_characters": words.chars().count(),
    });
    summary["notes"] = notes_json(&script);
    summary["next"] = json!(match path {
        Some(path) => format!(
            "dm speech transcribe '{}' (check the words); dm messages send CONVERSATION --speak SCRIPT",
            path.display()
        ),
        None => "dm messages send CONVERSATION --speak SCRIPT".to_owned(),
    });
    Ok(finish(summary, target.is_stdout(), output.compact))
}

fn too_large(shown: &str) -> anyhow::Error {
    coded(
        "audio_too_large",
        format!(
            "{shown} is larger than {} MiB, the most DM transcribes in one request.",
            speech::DEFAULT_TRANSCRIBE_MAX_BYTES / (1024 * 1024)
        ),
        "Transcribe a shorter recording; split it with any audio editor (ffmpeg -i in.m4a -f segment -segment_time 600 part%02d.m4a).",
    )
}

/// Reads at most the transcription limit from a file or standard input.
fn read_audio(file: &Path) -> Result<(Vec<u8>, String)> {
    let limit = speech::DEFAULT_TRANSCRIBE_MAX_BYTES;
    let (mut reader, shown): (Box<dyn Read>, String) = if file == Path::new("-") {
        (
            Box::new(std::io::stdin().lock()),
            "standard input".to_owned(),
        )
    } else {
        let shown = file.display().to_string();
        let metadata = std::fs::metadata(file).map_err(|error| {
            coded(
                "audio_unreadable",
                format!("{shown} cannot be read: {error}."),
                "Check the path; '-' reads standard input.",
            )
        })?;
        if metadata.is_dir() {
            return Err(coded(
                "audio_unreadable",
                format!("{shown} is a directory, not an audio file."),
                "Name the audio file to transcribe.",
            ));
        }
        if metadata.len() > limit {
            return Err(too_large(&shown));
        }
        (Box::new(std::fs::File::open(file)?), shown)
    };
    let mut bytes = Vec::new();
    reader.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(too_large(&shown));
    }
    if bytes.is_empty() {
        return Err(coded(
            "audio_unreadable",
            format!("{shown} is empty."),
            "Record or download the audio first.",
        ));
    }
    Ok((bytes, shown))
}

/// The transcription type: `--content-type`, else the first bytes, else the file name.
fn audio_type(bytes: &[u8], file: &Path, declared: Option<&str>) -> Result<&'static str> {
    if let Some(declared) = declared {
        return speech::normalize_audio_type(declared).ok_or_else(|| {
            coded(
                "unsupported_audio_type",
                format!(
                    "`{declared}` is not a type DM transcribes; it takes {}.",
                    speech::TRANSCRIPTION_TYPES.join(", ")
                ),
                "Convert the audio (for example ffmpeg -i in.xxx out.wav) or name its real type.",
            )
        });
    }
    speech::sniff_audio_type(bytes)
        .or_else(|| speech::audio_type_for_name(&file.to_string_lossy()))
        .ok_or_else(|| {
            coded(
                "unsupported_audio_type",
                format!(
                    "dm cannot tell what audio {} is: its first bytes and name match none of {}.",
                    if file == Path::new("-") {
                        "standard input".to_owned()
                    } else {
                        file.display().to_string()
                    },
                    speech::TRANSCRIPTION_TYPES.join(", ")
                ),
                "Pass --content-type, or convert it (ffmpeg -i in.xxx out.wav).",
            )
        })
}

/// Transcribes audio. A WAV that starts speaking at once (DM's own speech does) gets silence in
/// front first, or the transcription can lose its first word; the duration reported is still
/// the audio's own.
async fn transcribe_audio(
    local: &LocalRuntime,
    profile: &str,
    bytes: &[u8],
    kind: &'static str,
    language: Option<&str>,
) -> Result<Value> {
    let lead = speech::TRANSCRIPTION_LEAD_MILLISECONDS;
    let padded = (kind == "audio/wav")
        .then(|| speech::with_leading_silence(bytes, lead))
        .flatten();
    let upload: &[u8] = padded.as_deref().unwrap_or(bytes);
    let mut transcription = local
        .with_client(profile, |client| async move {
            client.transcribe(upload.to_vec(), kind, language).await
        })
        .await?;
    if padded.is_some() {
        transcription.duration_milliseconds = transcription
            .duration_milliseconds
            .map(|duration| duration.saturating_sub(lead));
    }
    let mut value = serde_json::to_value(&transcription)?;
    if transcription.transcript.trim().is_empty() {
        value["note"] = json!("No speech was recognised in the audio.");
    }
    if padded.is_some() {
        value["leading_silence_added_ms"] = json!(lead);
    }
    Ok(value)
}

/// `dm speech transcribe`.
pub async fn transcribe_file(
    local: &LocalRuntime,
    profile: &str,
    file: &Path,
    language: Option<String>,
    content_type: Option<String>,
) -> Result<Value> {
    let (bytes, shown) = read_audio(file)?;
    let kind = audio_type(&bytes, file, content_type.as_deref())?;
    let mut value = transcribe_audio(local, profile, &bytes, kind, language.as_deref()).await?;
    value["file"] = json!(shown);
    value["content_type"] = json!(kind);
    value["bytes"] = json!(bytes.len());
    Ok(value)
}

/// `c:ada::si:scout` becomes `c-ada-si-scout`, for default file names.
fn file_part(text: &str) -> String {
    let mut part = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            part.push(character);
        } else if !part.ends_with('-') {
            part.push('-');
        }
    }
    part.trim_matches('-').to_owned()
}

/// `dm messages listen`.
pub async fn listen(
    local: &LocalRuntime,
    profile: &str,
    conversation: &str,
    message: &str,
    for_account: Option<&str>,
    output: WavOutput,
) -> Result<Outcome> {
    let default = PathBuf::from(format!(
        "dm-{}-{}.wav",
        file_part(conversation),
        file_part(message)
    ));
    let target = Target::new(output.out.as_deref().unwrap_or(&default), output.force)?;
    let audio = local
        .with_client(profile, |client| async move {
            client
                .message_speech(conversation, message, for_account)
                .await
        })
        .await?;
    let path = target.write(&audio.wav)?;
    let mut summary = written(path.as_deref(), &audio);
    summary["conversation_id"] = json!(conversation);
    summary["message_id"] = json!(message);
    Ok(finish(summary, target.is_stdout(), output.compact))
}

/// Where a message's audio may be downloaded from: public hosts only, unless this profile's
/// DM service runs on this machine (local development, whose files are on this machine too).
/// The links are the sender's, and must not make dm read what only this machine can reach.
fn audio_hosts(profile: &str) -> Result<speech::AudioHosts> {
    let config = crate::store::load()?;
    let base_url = silicon_dm_client::runtime::store::profile(&config, profile)?
        .base_url
        .clone();
    let local = url::Url::parse(&base_url)
        .ok()
        .is_some_and(|url| silicon_dm_client::is_loopback(&url));
    Ok(if local {
        speech::AudioHosts::AlsoLocal
    } else {
        speech::AudioHosts::Public
    })
}

/// `dm messages transcribe`: the voice note, else the first audio attachment, else the speech.
pub async fn transcribe_message(
    local: &LocalRuntime,
    profile: &str,
    conversation: &str,
    message: &str,
    language: Option<String>,
) -> Result<Value> {
    let found = local
        .with_client(profile, |client| async move {
            client.message(conversation, message).await
        })
        .await?;
    if found.deleted_at.is_some() {
        return Err(coded(
            "message_deleted",
            format!(
                "Message {message} in {conversation} was deleted; there is nothing to transcribe."
            ),
            "dm messages list shows the conversation's other messages.",
        ));
    }
    let audio_link = found
        .voice
        .as_ref()
        .map(|voice| ("voice", voice.url.clone()))
        .or_else(|| {
            found
                .attachment_details
                .iter()
                .find(|detail| {
                    detail
                        .content_type
                        .as_deref()
                        .and_then(speech::normalize_audio_type)
                        .is_some()
                        || speech::audio_type_for_name(&detail.url).is_some()
                })
                .map(|detail| ("attachment", detail.url.clone()))
        })
        .or_else(|| {
            // Services before DM 0.17 list links only: recognise audio by its file name.
            found
                .content
                .attachments
                .iter()
                .find(|attachment| {
                    speech::audio_type_for_name(attachment.permanent_url.path()).is_some()
                })
                .map(|attachment| ("attachment", attachment.permanent_url.to_string()))
        });
    let (source, url, bytes, kind) = match audio_link {
        Some((source, url)) => {
            let fetched = speech::fetch_audio_from(
                &url,
                speech::DEFAULT_TRANSCRIBE_MAX_BYTES,
                audio_hosts(profile)?,
            )
            .await?;
            (
                source,
                Some(speech::display_url(&fetched.url)),
                fetched.bytes,
                fetched.content_type,
            )
        }
        None if found.speech.is_some() => {
            let audio = local
                .with_client(profile, |client| async move {
                    client.message_speech(conversation, message, None).await
                })
                .await?;
            ("speech", None, audio.wav, "audio/wav")
        }
        None => {
            return Err(coded(
                "no_audio",
                format!(
                    "Message {message} in {conversation} has no voice note, audio attachment or speech to transcribe."
                ),
                format!(
                    "dm messages show {conversation} {message} lists its attachments; dm speech transcribe FILE transcribes a recording you have."
                ),
            ));
        }
    };
    let mut value = transcribe_audio(local, profile, &bytes, kind, language.as_deref()).await?;
    value["conversation_id"] = json!(conversation);
    value["message_id"] = json!(message);
    value["source"] = json!(source);
    value["url"] = json!(url);
    value["content_type"] = json!(kind);
    value["bytes"] = json!(bytes.len());
    value["voice_transcript"] = json!(found.content.voice_transcript);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;

    #[test]
    fn default_names_are_safe_and_stable() {
        assert_eq!(file_part("c:ada::si:scout"), "c-ada-si-scout");
        assert_eq!(file_part("g:product-design"), "g-product-design");
        assert_eq!(file_part("00a"), "00a");
        assert_eq!(short_hash("mark|hello"), short_hash("mark|hello"));
        assert_ne!(short_hash("mark|hello"), short_hash("hope|hello"));
        assert_eq!(voice_key(" Brittney "), "brittney");
    }

    #[test]
    fn wav_files_are_written_whole_and_never_replaced_without_force() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("speech.wav");
        let first = Target::new(&path, false)?;
        assert_eq!(first.write(b"RIFF-one")?, Some(std::path::absolute(&path)?));
        assert_eq!(std::fs::read(&path)?, b"RIFF-one");
        let refused = Target::new(&path, false).err().context("refused")?;
        assert_eq!(
            refused.downcast_ref::<SessionError>().map(|e| e.code),
            Some("file_exists")
        );
        // A file that appears between the check and the write is not replaced either.
        let racing = Target::new(&directory.path().join("late.wav"), false)?;
        std::fs::write(directory.path().join("late.wav"), b"theirs")?;
        assert!(racing.write(b"ours").is_err());
        assert_eq!(std::fs::read(directory.path().join("late.wav"))?, b"theirs");
        Target::new(&path, true)?.write(b"RIFF-two")?;
        assert_eq!(std::fs::read(&path)?, b"RIFF-two");
        let leftovers: Vec<_> = std::fs::read_dir(directory.path())?
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary files stay behind");
        let missing = Target::new(&directory.path().join("no/such/dir.wav"), false)
            .err()
            .context("missing directory")?;
        assert_eq!(
            missing.downcast_ref::<SessionError>().map(|e| e.code),
            Some("output_directory_missing")
        );
        // A device is written into, never replaced, with or without --force.
        #[cfg(unix)]
        for force in [false, true] {
            let null = Target::new(Path::new("/dev/null"), force)?;
            assert!(matches!(null, Target::Special(_)));
            null.write(b"discarded")?;
            assert!(std::fs::metadata("/dev/null")?.file_type().is_char_device());
        }
        let folder = Target::new(directory.path(), true)
            .err()
            .context("a directory")?;
        assert_eq!(
            folder.downcast_ref::<SessionError>().map(|e| e.code),
            Some("output_is_directory")
        );
        Ok(())
    }

    #[test]
    fn audio_types_come_from_the_flag_the_bytes_or_the_name() -> Result<()> {
        let wav = b"RIFF\0\0\0\0WAVEfmt ";
        assert_eq!(audio_type(wav, Path::new("x.mp3"), None)?, "audio/wav");
        assert_eq!(
            audio_type(b"????", Path::new("x.m4a"), None)?,
            "audio/x-m4a"
        );
        assert_eq!(
            audio_type(b"????", Path::new("-"), Some("audio/webm; codecs=opus"))?,
            "audio/webm"
        );
        for (bytes, name, declared) in [
            (&b"????"[..], "notes.txt", None),
            (&b"????"[..], "x.wav", Some("text/plain")),
        ] {
            let error = audio_type(bytes, Path::new(name), declared)
                .err()
                .context("unsupported")?;
            assert_eq!(
                error.downcast_ref::<SessionError>().map(|e| e.code),
                Some("unsupported_audio_type")
            );
        }
        Ok(())
    }
}
