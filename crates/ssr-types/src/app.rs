//! Intent and snapshot: the whole contract between the screen and Rust.
//!
//! The screen sends only intents and draws only snapshots; it keeps no state of
//! its own, so reopening the window draws the same picture (ADR 0001). The
//! `supervisor` state machine consumes the intents and produces the snapshots
//! (ADR 0013).

use std::path::PathBuf;

use crate::layout::{DirectionId, DirectionView};
use crate::ports::{DeviceId, DriverState, OutputLayout, Platform};
use crate::position::Speaker;
use crate::status::EngineStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Ko,
    En,
}

/// One line on the pipe from a guarded app (ADR 0033).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Heartbeat {
    pub running: bool,
    pub output_periods: u64,
    pub capture_reads: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Intent {
    SetLanguage(Option<Language>),
    BeginSetup,
    InstallDriver,
    /// Looks at the PC again after the user did something outside the app
    /// (a reboot, test signing, a repair, a device plugged in at the output
    /// step), or leaves the faulted state.
    Recheck,
    ChooseOutput(DeviceId),
    /// Switches the output between headphones and speakers (ADR 0034).
    /// Headphones need an imported BRIR set.
    SetProfileKind(ProfileKind),
    /// Keeps the BRIR set for headphones; a running headphone output takes
    /// it live, while speakers in use stay as they are.
    ImportBrir(PathBuf),
    /// The layout speakers use while no correction is imported.
    SetSpeakerLayout(SpeakerLayout),
    /// Imports a room correction for the speakers: the CamillaDSP
    /// configuration SECS 2 or SECS 2R exports (ADR 0034). It decides the
    /// speakers, the device channels and all their processing.
    ImportCorrection(PathBuf),
    /// Goes back to the stored speaker layout without correction.
    ClearCorrection,
    /// The gain of the output in use; each kind keeps its own.
    SetGain(f32),
    TestDirection(DirectionId),
    /// Chooses an endpoint's upmixer (ADR 0035). `FirstParty` is refused
    /// until the first-party upmixer exists (ADR 0020).
    SetUpmixer(UpmixerSetting),
    /// Chooses the VST upmixer plugin of an endpoint: a `.vst3` bundle or a
    /// VST 2 `.dll`. The supervisor scans it before keeping it.
    ChoosePlugin(PluginChoice),
    /// Opens the plugin's editor window of an endpoint (its host process's
    /// own top-level window).
    OpenPluginEditor(u16),
    /// Whether an output kind applies upmixing (ADR 0035); on by default.
    SetUpmixApply(UpmixApply),
    Enable,
    Disable,
    StartDiagnosticRecording,
    Quit,
}

/// The first-run wizard's steps, in the order the checks run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    Welcome,
    /// The PC cannot run this build: an unsupported OS build or architecture
    /// (the error says which).
    Unsupported,
    /// The driver package needs test signing and the PC does not have it on
    /// (Secure Boot state in the error).
    NotTestSigning,
    /// The driver is not installed or Windows reports a problem code for it.
    Driver,
    RebootRequired,
    ChooseOutput,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Phase {
    Setup(SetupStep),
    Ready,
    Running,
    Faulted,
    Quit,
}

/// The two outputs a profile offers. One is in use at a time and each keeps
/// its own settings, so switching back finds them as they were (ADR 0034).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// Binaural over headphones through the imported BRIR set.
    Headphones,
    /// The listener's speakers, each with an optional room-correction FIR.
    Speakers,
}

/// The speaker layouts the app offers: standard layouts with a Windows
/// channel mask (ADR 0034). Any other layout takes a `profile.toml`
/// (ADR 0030). The serialized form is the layout's name.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub enum SpeakerLayout {
    #[default]
    #[serde(rename = "2.0")]
    Stereo,
    #[serde(rename = "2.1")]
    Stereo21,
    #[serde(rename = "4.0")]
    Quad,
    #[serde(rename = "5.1")]
    Surround51,
    #[serde(rename = "7.1")]
    Surround71,
    #[serde(rename = "5.1.2")]
    Surround512,
    #[serde(rename = "5.1.4")]
    Surround514,
    #[serde(rename = "7.1.2")]
    Surround712,
    #[serde(rename = "7.1.4")]
    Surround714,
}

impl SpeakerLayout {
    /// In the order the screen lists them.
    pub const ALL: [Self; 9] = [
        Self::Stereo,
        Self::Stereo21,
        Self::Quad,
        Self::Surround51,
        Self::Surround71,
        Self::Surround512,
        Self::Surround514,
        Self::Surround712,
        Self::Surround714,
    ];

    /// The layout's name, which is also its serialized form.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stereo => "2.0",
            Self::Stereo21 => "2.1",
            Self::Quad => "4.0",
            Self::Surround51 => "5.1",
            Self::Surround71 => "7.1",
            Self::Surround512 => "5.1.2",
            Self::Surround514 => "5.1.4",
            Self::Surround712 => "7.1.2",
            Self::Surround714 => "7.1.4",
        }
    }

    /// The speakers in device-channel order. [`Speaker::Lfe`] is the
    /// subwoofer. The order and the mask bits are those of
    /// [`crate::channel_layout_preset`]; where a layout has no side pair, its
    /// surrounds on the back bits are named `Ls`/`Rs`, so they sit at the
    /// nominal 110 degrees of a 5.1 room instead of the 150 degrees of a 7.1
    /// back pair. `Ls`/`Rs` carry no Windows bit, so the mask order check of
    /// a speaker profile leaves them alone.
    pub const fn speakers(self) -> &'static [Speaker] {
        use Speaker::*;
        match self {
            Self::Stereo => &[L, R],
            Self::Stereo21 => &[L, R, Lfe],
            Self::Quad => &[L, R, Ls, Rs],
            Self::Surround51 => &[L, R, C, Lfe, Ls, Rs],
            Self::Surround71 => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss],
            Self::Surround512 => &[L, R, C, Lfe, Ls, Rs, Tfl, Tfr],
            Self::Surround514 => &[L, R, C, Lfe, Ls, Rs, Tfl, Tfr, Trl, Trr],
            Self::Surround712 => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr],
            Self::Surround714 => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr, Trl, Trr],
        }
    }

    /// The device format the layout opens: one channel per speaker, with the
    /// standard Windows channel mask.
    pub const fn output_layout(self) -> OutputLayout {
        let channel_mask = match self {
            Self::Stereo => 0x3,
            Self::Stereo21 => 0xb,
            Self::Quad => 0x33,
            Self::Surround51 => 0x3f,
            Self::Surround71 => 0x63f,
            Self::Surround512 => 0x503f,
            Self::Surround514 => 0x2d03f,
            Self::Surround712 => 0x563f,
            Self::Surround714 => 0x2d63f,
        };
        OutputLayout {
            channels: self.speakers().len() as u16,
            channel_mask,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct ProfileView {
    /// The output in use: the one Enable starts, or the running engine plays.
    pub kind: ProfileKind,
    /// The output in use by name: the BRIR set, the speaker layout (`"7.1.4"`),
    /// the imported correction's title, or the name `profile.toml` gives.
    pub name: String,
    /// The gain of the output in use.
    pub gain_db: f32,
    /// The directions of the output in use: the BRIR set's, or the speakers'
    /// (the subwoofer is not a direction).
    pub directions: Vec<DirectionView>,
    /// The state directory's profile.toml decides the output; the kind,
    /// import, layout and correction intents are refused.
    pub from_file: bool,
    /// Headphone settings, kept while speakers are in use.
    pub headphones: HeadphonesView,
    /// Speaker settings, kept while headphones are in use.
    pub speakers: SpeakersView,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct HeadphonesView {
    /// Imported BRIR set (folder, hesuvi.wav, or hrir.wav).
    pub brir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct SpeakersView {
    /// The stored app layout, used while no correction is imported; `None`
    /// when a speaker `profile.toml` lays the speakers out its own way.
    pub layout: Option<SpeakerLayout>,
    /// The imported correction, which then decides the speakers.
    pub correction: Option<CorrectionView>,
    /// Every device channel that plays, in device order: the correction's
    /// outputs when one is imported, else the layout's speakers.
    pub channels: Vec<SpeakerChannelView>,
}

/// An imported SECS 2 / SECS 2R correction (ADR 0034).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct CorrectionView {
    /// The CamillaDSP configuration file.
    pub path: String,
    /// The configuration's `title` (`"SECS 2 mixed (8.0 ms)"`).
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct SpeakerChannelView {
    /// The speaker on this channel (`"L"`, `"Tfl"`), `"LFE"` for the layout's
    /// subwoofer, or the correction's output name (`"Lb"`, `"SW2"`).
    pub name: String,
    /// Zero-based device channel.
    pub channel: u16,
    pub subwoofer: bool,
}

/// The upmixers an endpoint's sound may go through (ADR 0035).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UpmixerChoice {
    /// The first-party upmixer (ADR 0020). Not built yet; it becomes the
    /// default once it is.
    FirstParty,
    /// A VST 2 or VST 3 upmixer plugin in its own host process (ADR 0021).
    Plugin,
    /// No upmixing.
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub enum PluginFormat {
    #[serde(rename = "vst2")]
    Vst2,
    #[serde(rename = "vst3")]
    Vst3,
}

/// [`Intent::SetUpmixer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct UpmixerSetting {
    /// Endpoint number: 0 main, 1 music, 2 PASS (ADR 0035, the 2026-10-16
    /// correction).
    pub endpoint: u16,
    pub choice: UpmixerChoice,
}

/// [`Intent::ChoosePlugin`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct PluginChoice {
    pub endpoint: u16,
    pub path: PathBuf,
}

/// [`Intent::SetUpmixApply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct UpmixApply {
    pub kind: ProfileKind,
    pub on: bool,
}

/// Why an endpoint's sound plays as it came while its upmixer runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UpmixBypass {
    /// The source playing now is native surround (more than two channels
    /// besides LFE, or objects); it is never upmixed.
    NativeSurround,
    /// The output kind in use does not apply upmixing.
    KindOff,
    /// The source's sample rate changed; the upmixer is being set up for it.
    RateChange,
}

/// A chosen upmixer plugin.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct PluginView {
    /// The `.vst3` bundle or `.dll`.
    pub path: String,
    pub format: PluginFormat,
    /// The plugin's own name and vendor, as it reports them.
    pub name: String,
    pub vendor: String,
}

/// What an endpoint's upmixer is doing.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum UpmixState {
    /// Not running: upmixing is off for this endpoint, the output kind in use
    /// does not apply it, or the engine is off.
    Stopped,
    /// The plugin host is starting or loading the plugin.
    Starting,
    Running,
    /// The plugin missed blocks or is restarting; the upmixer's input plays,
    /// delayed the same (ADR 0021, ADR 0035).
    Fallback,
    /// Off for this session; the sentence says why.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct EndpointUpmixView {
    /// Endpoint number: 0 main, 1 music, 2 PASS.
    pub endpoint: u16,
    /// The saved choice; `None` follows the default (ADR 0035).
    pub choice: Option<UpmixerChoice>,
    /// What the choice resolves to now, the default included.
    pub effective: UpmixerChoice,
    pub plugin: Option<PluginView>,
    pub state: UpmixState,
    /// Set while the upmixer runs but the sound playing now goes around it.
    pub bypass: Option<UpmixBypass>,
    /// The upmix path's added latency while it runs: two blocks, the
    /// plugin's own latency and the 2 ms concealment (ADR 0035).
    pub latency_ms: Option<f32>,
    /// The plugin's input speakers by name (`"L"`, `"R"`, ...); the stereo is
    /// placed onto them first.
    pub inputs: Vec<String>,
    /// The plugin's output speakers by name (`"L"`, `"C"`, `"LFE"`, ...).
    pub outputs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct UpmixView {
    /// The main, music and PASS endpoints, in endpoint order.
    pub endpoints: Vec<EndpointUpmixView>,
    /// Whether headphones apply upmixing (on by default).
    pub headphones: bool,
    /// Whether speakers apply upmixing (on by default).
    pub speakers: bool,
    /// The first-party upmixer exists in this build. False until ADR 0020's
    /// conditions are met; the screen then shows it as not yet built.
    pub first_party_available: bool,
}

/// An output the user may choose, or has chosen.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct OutputChoice {
    pub id: DeviceId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct Snapshot {
    /// The resolved language. An OS-following setting is resolved before the
    /// snapshot crosses the UI contract.
    pub language: Language,
    /// The saved choice behind `language`: `None` follows the Windows display
    /// language. The settings screen's selector shows this, not the result.
    pub language_choice: Option<Language>,
    pub phase: Phase,
    pub profile: ProfileView,
    pub upmix: UpmixView,
    pub engine: Option<EngineStatus>,
    /// Outputs the user may choose: active, and never our own endpoints.
    pub outputs: Vec<OutputChoice>,
    pub chosen_output: Option<OutputChoice>,
    pub driver: Option<DriverState>,
    pub platform: Option<Platform>,
    /// The three default roles point at our main endpoint.
    pub roles_taken: bool,
    /// The last error, cleared by the next intent that succeeds.
    pub error: Option<UserError>,
    /// Facts the screen shows as is (a previous default device that
    /// disappeared, ...).
    pub notes: Vec<String>,
}

/// An error the screen shows as is: the sentence and the next action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[error("{message}")]
pub struct UserError {
    pub message: String,
    pub next: NextAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    Retry,
    OpenSettings,
    /// Restart Windows, then recheck.
    Reboot,
    None,
}

#[cfg(test)]
mod tests {
    use super::SpeakerLayout;
    use crate::position::Speaker;

    /// The Windows speaker a channel of the layout stands for: `Ls`/`Rs`
    /// ride the back bits where the layout has no side pair.
    fn windows_speaker(speaker: Speaker) -> Speaker {
        match speaker {
            Speaker::Ls => Speaker::Lrs,
            Speaker::Rs => Speaker::Rrs,
            other => other,
        }
    }

    #[test]
    fn every_layout_opens_its_speakers_in_mask_order() {
        for layout in SpeakerLayout::ALL {
            let output = layout.output_layout();
            let speakers = layout.speakers();
            assert_eq!(
                usize::from(output.channels),
                speakers.len(),
                "{}",
                layout.name()
            );
            assert_eq!(output.channel_mask.count_ones() as usize, speakers.len());
            // Channel n is the mask's n-th set bit (WAVEFORMATEXTENSIBLE).
            let mut bits = (0..32)
                .map(|shift| 1_u32 << shift)
                .filter(|bit| output.channel_mask & bit != 0);
            for &speaker in speakers {
                let bit = bits.next().expect("one bit per speaker");
                assert_eq!(Speaker::from_wave_bit(bit), Some(windows_speaker(speaker)));
            }
            assert!(speakers.iter().filter(|&&s| s == Speaker::Lfe).count() <= 1);
        }
    }

    #[test]
    fn layouts_match_the_shared_presets_and_serialize_as_their_names() {
        for layout in SpeakerLayout::ALL {
            let json = serde_json::to_string(&layout).unwrap();
            assert_eq!(json, format!("\"{}\"", layout.name()));
            assert_eq!(
                serde_json::from_str::<SpeakerLayout>(&json).unwrap(),
                layout
            );
            if let Some(preset) = crate::channel_layout_preset(layout.name()) {
                let ours: Vec<_> = layout
                    .speakers()
                    .iter()
                    .map(|&s| windows_speaker(s))
                    .collect();
                assert_eq!(ours, preset, "{}", layout.name());
            }
        }
        assert_eq!(SpeakerLayout::default(), SpeakerLayout::Stereo);
    }
}
