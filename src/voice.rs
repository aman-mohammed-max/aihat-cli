//! Speaking instead of typing: record from the microphone with a recorder
//! the computer already has (PipeWire, PulseAudio, ALSA, SoX or ffmpeg),
//! then turn the speech into text, either on this computer with Phonon-2
//! (`pip install fermion-research` gives the `phonon` command; it runs on the
//! CPU, no network needed) or with any OpenAI-compatible
//! `/audio/transcriptions` endpoint: `phonon serve`, your provider, or one in
//! settings.json.

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

/// Which way speech becomes text: "phonon" (offline), "endpoint", or "auto"
/// (phonon when it is installed, else the endpoint).
pub fn engine(config: &VoiceConfig) -> &'static str {
    match config.engine.as_str() {
        "phonon" | "offline" | "local" => "phonon",
        "endpoint" | "online" | "api" => "endpoint",
        _ if on_path("phonon") => "phonon",
        _ => "endpoint",
    }
}

/// Turn a recording (or any audio file) into text.
pub fn transcribe(config: &VoiceConfig, db: &Database, secrets: &Secrets, file: &Path) -> Result<String> {
    let text = match engine(config) {
        "phonon" => transcribe_offline(file)?,
        _ => transcribe_online(config, db, secrets, file)?,
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("No speech was heard in the recording.");
    }
    Ok(text)
}

/// Phonon-2 on this computer: `phonon transcribe FILE`.
fn transcribe_offline(file: &Path) -> Result<String> {
    if !on_path("phonon") {
        bail!("Offline speech needs Phonon-2: pip install fermion-research (then `phonon` is on your PATH).");
    }
    let output = Command::new("phonon").arg("transcribe").arg(file).stdin(Stdio::null()).output().context("couldn't run phonon")?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("phonon couldn't transcribe it: {}", why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// An OpenAI-compatible endpoint: voice.url (like `phonon serve` at
/// http://127.0.0.1:8010/v1), or the default provider when it is empty.
fn transcribe_online(config: &VoiceConfig, db: &Database, secrets: &Secrets, file: &Path) -> Result<String> {
    let audio = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let model = if config.model.trim().is_empty() { "whisper-1".to_string() } else { config.model.trim().to_string() };
    if config.url.trim().is_empty() {
        let provider = db.default_provider()?.context("Add a provider first, or set voice.url to a speech endpoint (phonon serve gives one).")?;
        return crate::api::transcribe(&provider.base_url, Some((&provider, secrets)), &model, audio);
    }
    crate::api::transcribe(config.url.trim(), None, &model, audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_are_named_plainly() {
        let mut config = VoiceConfig::default();
        config.engine = "offline".into();
        assert_eq!(engine(&config), "phonon");
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
