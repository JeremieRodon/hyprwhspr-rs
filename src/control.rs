use anyhow::{anyhow, bail, Context, Result};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderState {
    Inactive,
    Recording,
    Processing,
}

impl RecorderState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Recording => "recording",
            Self::Processing => "processing",
        }
    }
}

impl fmt::Display for RecorderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RecorderState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "inactive" => Ok(Self::Inactive),
            "recording" => Ok(Self::Recording),
            "processing" => Ok(Self::Processing),
            other => Err(anyhow!("Unknown recorder state: {other}")),
        }
    }
}

/// Per-recording language/translation overrides carried by Start and Toggle commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscribeOptions {
    /// BCP-47 / whisper language code (e.g. "fr", "en", "auto").
    /// `None` means use the configured default.
    pub lang: Option<String>,
    /// When true, add `--translate` so whisper outputs English regardless of input language.
    pub translate: Option<bool>,
}

impl fmt::Display for TranscribeOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(lang) = &self.lang {
            write!(f, " lang={lang}")?;
        }
        if let Some(translate) = self.translate {
            if translate {
                f.write_str(" translate")?;
            } else {
                f.write_str(" no_translate")?;
            }
        }
        Ok(())
    }
}

impl FromStr for TranscribeOptions {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut opts = Self::default();
        for tok in value.split_whitespace() {
            if let Some(lang) = tok.strip_prefix("lang=") {
                opts.lang = Some(lang.to_string());
            } else if tok == "translate" {
                opts.translate = Some(true);
            } else if tok == "no_translate" {
                opts.translate = Some(false);
            }
        }
        Ok(opts)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordCommand {
    Start(TranscribeOptions),
    Stop,
    Toggle(TranscribeOptions),
    Status,
}

impl RecordCommand {
    pub fn start_default() -> Self {
        Self::Start(TranscribeOptions::default())
    }

    pub fn toggle_default() -> Self {
        Self::Toggle(TranscribeOptions::default())
    }
}

impl fmt::Display for RecordCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(opts) => write!(f, "start{opts}"),
            Self::Stop => f.write_str("stop"),
            Self::Toggle(opts) => write!(f, "toggle{opts}"),
            Self::Status => f.write_str("status"),
        }
    }
}

impl FromStr for RecordCommand {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (verb, opts) = match value.split_once(" ") {
            Some((verb, opts)) => (verb, opts),
            None => (value, ""),
        };
        match verb {
            "start" => Ok(Self::Start(opts.parse()?)),
            "stop" => Ok(Self::Stop),
            "toggle" => Ok(Self::Toggle(opts.parse()?)),
            "status" => Ok(Self::Status),
            other => Err(anyhow!("Unknown record command: {other}")),
        }
    }
}

pub type ControlReply = std::result::Result<RecorderState, String>;

#[derive(Debug)]
pub struct ControlRequest {
    pub command: RecordCommand,
    pub reply_tx: oneshot::Sender<ControlReply>,
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::env;
    use std::fs;
    use std::io::ErrorKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::task::JoinHandle;

    pub struct ControlServer {
        path: PathBuf,
        task: JoinHandle<()>,
    }

    impl ControlServer {
        pub fn spawn(tx: mpsc::Sender<ControlRequest>) -> Result<Option<Self>> {
            let path = control_socket_path()?;
            prepare_socket_path(&path)?;
            let listener = UnixListener::bind(&path)
                .with_context(|| format!("Failed to bind control socket at {}", path.display()))?;

            let task = tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let request_tx = tx.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_client(stream, request_tx).await {
                                    tracing::warn!("Control client error: {err:#}");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::error!("Control socket accept failed: {err}");
                            break;
                        }
                    }
                }
            });

            Ok(Some(Self { path, task }))
        }
    }

    impl Drop for ControlServer {
        fn drop(&mut self) {
            self.task.abort();
            if let Err(err) = fs::remove_file(&self.path) {
                if err.kind() != ErrorKind::NotFound {
                    tracing::warn!(
                        "Failed to remove control socket {}: {}",
                        self.path.display(),
                        err
                    );
                }
            }
        }
    }

    pub async fn send_record_command(command: RecordCommand) -> Result<RecorderState> {
        let path = control_socket_path()?;
        let mut stream = UnixStream::connect(&path).await.with_context(|| {
            format!(
                "Failed to connect to hyprwhspr-rs control socket at {}. Start hyprwhspr-rs first.",
                path.display()
            )
        })?;

        stream
            .write_all(command.to_string().as_bytes())
            .await
            .context("Failed to send control command")?;
        stream
            .shutdown()
            .await
            .context("Failed to finish control command write")?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .context("Failed to read control response")?;

        parse_response(&String::from_utf8_lossy(&response))
    }

    async fn handle_client(mut stream: UnixStream, tx: mpsc::Sender<ControlRequest>) -> Result<()> {
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .await
            .context("Failed to read control request")?;

        let command = RecordCommand::from_str(String::from_utf8_lossy(&request).trim())?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(ControlRequest { command, reply_tx })
            .await
            .context("Control channel closed")?;

        let response = match reply_rx.await {
            Ok(Ok(state)) => format!("ok {state}\n"),
            Ok(Err(message)) => format!("err {message}\n"),
            Err(_) => "err control handler dropped response\n".to_string(),
        };

        stream
            .write_all(response.as_bytes())
            .await
            .context("Failed to write control response")?;
        stream
            .shutdown()
            .await
            .context("Failed to finish control response write")?;

        Ok(())
    }

    fn control_socket_path() -> Result<PathBuf> {
        let base = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir());
        let dir = base.join("hyprwhspr-rs");
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create control directory {}", dir.display()))?;
        Ok(dir.join("control.sock"))
    }

    fn prepare_socket_path(path: &PathBuf) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => bail!(
                "Another hyprwhspr-rs instance is already serving control commands at {}",
                path.display()
            ),
            Err(_) => {
                fs::remove_file(path).with_context(|| {
                    format!("Failed to remove stale control socket {}", path.display())
                })?;
            }
        }

        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub struct ControlServer;

    impl ControlServer {
        pub fn spawn(_tx: mpsc::Sender<ControlRequest>) -> Result<Option<Self>> {
            Ok(None)
        }
    }

    pub async fn send_record_command(_command: RecordCommand) -> Result<RecorderState> {
        bail!("`hyprwhspr-rs record` is currently only supported on Linux")
    }
}

pub use platform::{send_record_command, ControlServer};

fn parse_response(raw: &str) -> Result<RecorderState> {
    let body = raw.trim();
    if let Some(state) = body.strip_prefix("ok ") {
        return RecorderState::from_str(state);
    }
    if let Some(message) = body.strip_prefix("err ") {
        bail!(message.to_string());
    }
    bail!("Malformed control response: {body}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_strings() {
        assert_eq!(
            RecorderState::from_str("recording").unwrap(),
            RecorderState::Recording
        );
        assert_eq!(
            RecorderState::from_str("processing").unwrap(),
            RecorderState::Processing
        );
    }

    #[test]
    fn parses_command_strings() {
        assert_eq!(
            RecordCommand::from_str("start").unwrap(),
            RecordCommand::Start(TranscribeOptions::default())
        );
        assert_eq!(
            RecordCommand::from_str("stop").unwrap(),
            RecordCommand::Stop
        );
        assert_eq!(
            RecordCommand::from_str("toggle").unwrap(),
            RecordCommand::Toggle(TranscribeOptions::default())
        );
        assert_eq!(
            RecordCommand::from_str("status").unwrap(),
            RecordCommand::Status
        );
    }

    #[test]
    fn roundtrips_record_options() {
        let cmd = RecordCommand::Toggle(TranscribeOptions {
            lang: Some("fr".to_string()),
            translate: Some(true),
        });
        let encoded = cmd.to_string();
        assert_eq!(encoded, "toggle lang=fr translate");
        let decoded = RecordCommand::from_str(&encoded).unwrap();
        assert_eq!(decoded, cmd);
    }

    #[test]
    fn roundtrips_lang_only() {
        let cmd = RecordCommand::Start(TranscribeOptions {
            lang: Some("auto".to_string()),
            translate: Some(false),
        });
        let decoded = RecordCommand::from_str(&cmd.to_string()).unwrap();
        assert_eq!(decoded, cmd);
    }

    #[test]
    fn roundtrips_translate_only() {
        let cmd = RecordCommand::Toggle(TranscribeOptions {
            lang: Some("en".to_string()),
            translate: Some(true),
        });
        let decoded = RecordCommand::from_str(&cmd.to_string()).unwrap();
        assert_eq!(decoded, cmd);
    }

    #[test]
    fn rejects_unknown_response() {
        assert!(parse_response("wat").is_err());
        assert!(parse_response("err nope").is_err());
    }
}
