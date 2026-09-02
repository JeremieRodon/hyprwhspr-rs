use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hyprwhspr-rs",
    version,
    about = "Native speech-to-text voice dictation for Hyprland"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Run in test mode (Enter to toggle recording)
    #[arg(long)]
    pub test: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Install integration components (waybar, systemd, elephant)
    Install(InstallArgs),
    /// Control the running recorder daemon
    Record(RecordArgs),
}

#[derive(clap::Args)]
pub struct InstallArgs {
    /// Install Waybar module and CSS
    #[arg(long)]
    pub waybar: bool,

    /// Install systemd user service
    #[arg(long)]
    pub service: bool,

    /// Install Elephant menu for Walker
    #[arg(long)]
    pub elephant: bool,

    /// Install all components (non-interactive)
    #[arg(long)]
    pub all: bool,

    /// Overwrite existing files without prompting
    #[arg(long, short)]
    pub force: bool,
}

impl InstallArgs {
    /// Returns true if any specific component flag was set
    pub fn has_specific_flags(&self) -> bool {
        self.waybar || self.service || self.elephant || self.all
    }
}

#[derive(clap::Args)]
pub struct RecordArgs {
    #[command(subcommand)]
    pub action: RecordAction,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct LangArgs {
    /// Spoken language code passed to whisper (e.g. fr, en, auto).
    /// Overrides the config default for this single recording.
    #[arg(long)]
    pub lang: Option<String>,

    /// Translate the transcription into English (whisper can only translate into English).
    #[arg(long)]
    pub translate: Option<bool>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum RecordAction {
    /// Start recording if idle
    Start(LangArgs),
    /// Stop recording if active
    Stop,
    /// Toggle between idle and recording
    Toggle(LangArgs),
    /// Print current recorder state
    Status,
}
