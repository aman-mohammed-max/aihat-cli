//! Speaking instead of typing: record from the microphone with a recorder
//! the computer already has (PipeWire, PulseAudio, ALSA, SoX or ffmpeg),
//! then turn the speech into text, either on this computer with Phonon-2
//! (downloaded from Hugging Face and run by aihat's own Rust engine, see
//! stt.rs; no network once downloaded) or with any OpenAI-compatible
//! `/audio/transcriptions` endpoint: your provider, or one in settings.json.

use crate::config::VoiceConfig;
use crate::db::Database;
use crate::secrets::Secrets;
use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A recording in progress.
pub struct Recording {
    child: Child,
    pub path: PathBuf,
    pub started: Instant,
    /// The recorder's name, for messages.
    pub recorder: String,
}

fn on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let file = dir.join(program);
        file.is_file() || (cfg!(windows) && dir.join(format!("{program}.exe")).is_file())
    })
}

/// The recorder command for `file`: settings.json's `voice.record` (with
/// `{file}` in it), else the first one found here. 16 kHz mono WAV.
fn recorder(config: &VoiceConfig, file: &Path) -> Result<(String, Vec<String>)> {
    let file = file.to_string_lossy().into_owned();
    if !config.record.trim().is_empty() {
        let line = config.record.replace("{file}", &crate::mcp_stdio::command_line(&[file]));
        return Ok(if cfg!(windows) { ("cmd".into(), vec!["/C".into(), line]) } else { ("sh".into(), vec!["-c".into(), line]) });
    }
    let candidates: Vec<(&str, Vec<String>)> = vec![
        ("pw-record", vec!["--rate".into(), "16000".into(), "--channels".into(), "1".into(), "--format".into(), "s16".into(), file.clone()]),
        ("parecord", vec!["--rate=16000".into(), "--channels=1".into(), "--format=s16le".into(), "--file-format=wav".into(), file.clone()]),
        ("arecord", vec!["-q".into(), "-f".into(), "S16_LE".into(), "-r".into(), "16000".into(), "-c".into(), "1".into(), file.clone()]),
        ("rec", vec!["-q".into(), "-r".into(), "16000".into(), "-c".into(), "1".into(), file.clone()]),
        (
            "ffmpeg",
            if cfg!(target_os = "macos") {
                vec!["-loglevel".into(), "error".into(), "-f".into(), "avfoundation".into(), "-i".into(), ":0".into(), "-ar".into(), "16000".into(), "-ac".into(), "1".into(), "-y".into(), file.clone()]
            } else {
                vec!["-loglevel".into(), "error".into(), "-f".into(), "pulse".into(), "-i".into(), "default".into(), "-ar".into(), "16000".into(), "-ac".into(), "1".into(), "-y".into(), file.clone()]
            },
        ),
    ];
    candidates
        .into_iter()
        .find(|(program, _)| on_path(program))
        .map(|(program, args)| (program.to_string(), args))
        .ok_or_else(|| anyhow!("No sound recorder was found. Install one (PipeWire, PulseAudio, alsa-utils, SoX or ffmpeg), or set voice.record in settings.json to a command that records to {{file}}."))
}

/// Start recording from the microphone.
pub fn start(config: &VoiceConfig) -> Result<Recording> {
    let path = std::env::temp_dir().join(format!("aihat-voice-{}.wav", uuid::Uuid::new_v4()));
    let (program, args) = recorder(config, &path)?;
    let mut command = Command::new(&program);
    command.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command.spawn().with_context(|| format!("couldn't start {program}"))?;
    Ok(Recording { child, path, started: Instant::now(), recorder: program })
}

impl Drop for Recording {
    fn drop(&mut self) {
        // aihat quit while listening: the recorder goes too.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Recording {
    /// Stop and return the recorded file. The recorder is asked to finish
    /// (so the WAV is complete) before it is stopped hard.
    pub fn stop(mut self) -> Result<PathBuf> {
        if let Ok(Some(status)) = self.child.try_wait().map(|s| s.filter(|s| !s.success())) {
            let mut why = String::new();
            if let Some(mut err) = self.child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut err, &mut why);
            }
            bail!("{} stopped by itself ({status}){}", self.recorder, if why.trim().is_empty() { String::new() } else { format!(": {}", why.trim()) });
        }
        #[cfg(unix)]
        // SAFETY: signalling the recorder's own process group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size <= 44 {
            let _ = std::fs::remove_file(&self.path);
            bail!("Nothing was recorded. Check that the microphone is on (on a Chromebook: Settings › Linux › Allow Linux to access your microphone).");
        }
        Ok(self.path.clone())
    }

    /// Stop without keeping anything.
    pub fn cancel(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The Phonon-2 files on Hugging Face, and their published checksums.
const MODEL_URL: &str = "https://huggingface.co/FermionResearch/Phonon-2/resolve/main/phonon-2.bps.tar.zst";
const MODEL_SHA256: &str = "98125795b6dda72f5c6eee9ba33d19815df65dcb18b50a357bf9f73c9935309e";
const CONTAINER_SHA256: &str = "4b6bfa3a12cc3c4e0a54f2ab3ec4ca7a842b09e5c7ecfc8e7ca0ac6cc8c11468";
pub const MODEL_BYTES: u64 = 163_515_201;
/// The tokenizer is the base model's (parakeet-tdt-0.6b-v3).
const TOKENIZER_URL: &str = "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/main/tokenizer.json";

/// Where the downloaded model lives.
pub fn model_dir() -> PathBuf {
    let data = crate::config::Paths::resolve(None).map(|p| p.data_dir).unwrap_or_else(|_| std::env::temp_dir());
    data.join("models").join("phonon-2")
}

pub fn model_installed() -> bool {
    let dir = model_dir();
    dir.join("model.fermion").is_file() && dir.join("tokenizer.json").is_file()
}

/// Which way speech becomes text: "local" (Phonon-2 on this computer),
/// "endpoint", or "auto": local once the model is downloaded.
pub fn engine(config: &VoiceConfig) -> &'static str {
    match config.engine.as_str() {
        "local" | "phonon" | "offline" => "local",
        "endpoint" | "online" | "api" => "endpoint",
        _ if model_installed() => "local",
        _ => "endpoint",
    }
}

/// Turn a recording (or any WAV or FLAC file) into text.
pub fn transcribe(config: &VoiceConfig, db: &Database, secrets: &Secrets, file: &Path) -> Result<String> {
    let text = match engine(config) {
        "local" => transcribe_local(file)?,
        _ => transcribe_online(config, db, secrets, file)?,
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("No speech was heard in the recording.");
    }
    Ok(text)
}

/// The model stays loaded after the first use, so later recordings start at once.
static LOADED: std::sync::Mutex<Option<std::sync::Arc<crate::stt::Phonon>>> = std::sync::Mutex::new(None);

fn transcribe_local(file: &Path) -> Result<String> {
    if !model_installed() {
        bail!("Phonon-2 isn't downloaded yet: /voice in the chat, or aihat voice download.");
    }
    let audio = crate::stt::read_audio(file)?;
    let model = {
        let mut slot = LOADED.lock().map_err(|_| anyhow!("the speech model is busy"))?;
        match slot.as_ref() {
            Some(model) => model.clone(),
            None => {
                let model = std::sync::Arc::new(crate::stt::Phonon::load(&model_dir()).context("loading Phonon-2")?);
                *slot = Some(model.clone());
                model
            }
        }
    };
    model.transcribe(&audio)
}

/// Load the model in the background (while the person is still talking),
/// so the text comes sooner.
pub fn preload() {
    if !model_installed() || LOADED.lock().map(|slot| slot.is_some()).unwrap_or(true) {
        return;
    }
    std::thread::spawn(|| {
        if let Ok(model) = crate::stt::Phonon::load(&model_dir())
            && let Ok(mut slot) = LOADED.lock()
            && slot.is_none()
        {
            *slot = Some(std::sync::Arc::new(model));
        }
    });
}

/// Free the loaded model (after deleting it, or to give memory back).
pub fn unload() {
    if let Ok(mut slot) = LOADED.lock() {
        *slot = None;
    }
}

/// An OpenAI-compatible endpoint: voice.url (like `phonon serve` at
/// http://127.0.0.1:8010/v1), or the default provider when it is empty.
fn transcribe_online(config: &VoiceConfig, db: &Database, secrets: &Secrets, file: &Path) -> Result<String> {
    let audio = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let model = if config.model.trim().is_empty() { "whisper-1".to_string() } else { config.model.trim().to_string() };
    if config.url.trim().is_empty() {
        let provider = db.default_provider()?.context("Add a provider first, or set voice.url to a speech endpoint.")?;
        return crate::api::transcribe(&provider.base_url, Some((&provider, secrets)), &model, audio);
    }
    crate::api::transcribe(config.url.trim(), None, &model, audio)
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Download Phonon-2 from Hugging Face (164 MB) and unpack it, all in Rust.
/// `progress` gets (bytes so far, total). `cancel` stops it.
pub fn download(progress: &mut dyn FnMut(u64, u64), cancel: &std::sync::atomic::AtomicBool) -> Result<PathBuf> {
    use std::io::{Read, Write};
    let dir = model_dir();
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join("phonon-2.bps.tar.zst.part");
    let client = crate::api::client(3_600_000)?;
    let mut response = client.get(MODEL_URL).send().context("Couldn't reach Hugging Face")?;
    if !response.status().is_success() {
        bail!("Hugging Face answered {} for the model", response.status());
    }
    let total = response.content_length().unwrap_or(MODEL_BYTES);
    let mut out = std::fs::File::create(&archive)?;
    let mut done = 0u64;
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&archive);
            bail!("Download stopped");
        }
        let read = response.read(&mut chunk).context("the download broke off")?;
        if read == 0 {
            break;
        }
        out.write_all(&chunk[..read])?;
        done += read as u64;
        progress(done, total);
    }
    out.flush()?;
    drop(out);
    if sha256_file(&archive)? != MODEL_SHA256 {
        let _ = std::fs::remove_file(&archive);
        bail!("The download doesn't match Phonon-2's published checksum, so it was not used.");
    }
    // .tar.zst: unpack model.fermion only.
    let decoder = ruzstd::decoding::StreamingDecoder::new(std::fs::File::open(&archive)?).map_err(|e| anyhow!("unpacking: {e}"))?;
    let target = dir.join("model.fermion");
    untar_one(decoder, "model.fermion", &target)?;
    let _ = std::fs::remove_file(&archive);
    if sha256_file(&target)? != CONTAINER_SHA256 {
        let _ = std::fs::remove_file(&target);
        bail!("The unpacked model doesn't match its checksum.");
    }
    let tokenizer = client.get(TOKENIZER_URL).send().context("Couldn't reach Hugging Face for the tokenizer")?;
    if !tokenizer.status().is_success() {
        bail!("Hugging Face answered {} for the tokenizer", tokenizer.status());
    }
    std::fs::write(dir.join("tokenizer.json"), tokenizer.bytes()?)?;
    Ok(dir)
}

/// Copy the file named `name` (in any folder) out of a tar stream.
fn untar_one(mut tar: impl std::io::Read, name: &str, target: &Path) -> Result<()> {
    let mut header = [0u8; 512];
    loop {
        if tar.read_exact(&mut header).is_err() || header.iter().all(|&b| b == 0) {
            bail!("{name} isn't in the download");
        }
        let text = |range: std::ops::Range<usize>| {
            let raw = &header[range];
            String::from_utf8_lossy(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())]).into_owned()
        };
        let size = u64::from_str_radix(text(124..136).trim(), 8).unwrap_or(0);
        let path = text(0..100);
        let padded = size.div_ceil(512) * 512;
        if path.rsplit('/').next() == Some(name) && matches!(header[156], b'0' | 0) {
            let mut out = std::fs::File::create(target)?;
            std::io::copy(&mut std::io::Read::take(&mut tar, size), &mut out)?;
            std::io::copy(&mut std::io::Read::take(&mut tar, padded - size), &mut std::io::sink())?;
            return Ok(());
        }
        std::io::copy(&mut std::io::Read::take(&mut tar, padded), &mut std::io::sink())?;
    }
}

/// Remove the downloaded model.
pub fn remove_model() -> Result<()> {
    unload();
    let dir = model_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_are_named_plainly() {
        let mut config = VoiceConfig::default();
        config.engine = "offline".into();
        assert_eq!(engine(&config), "local");
        config.engine = "online".into();
        assert_eq!(engine(&config), "endpoint");
    }

    #[cfg(unix)]
    #[test]
    fn a_custom_recorder_gets_the_file() {
        let config = VoiceConfig { record: "cp /dev/null {file}".into(), ..VoiceConfig::default() };
        let (program, args) = recorder(&config, Path::new("/tmp/a b.wav")).unwrap();
        assert_eq!(program, "sh");
        assert_eq!(args[1], "cp /dev/null '/tmp/a b.wav'");
    }
}
