//! Machine-readable summary shared by saved and live input projections.
use super::injected_input::{InjectedTimeCertainty, InputEventSource};
use serde::{Deserialize, Serialize};

/// Extent of physical capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PhysicalStatus {
    /// Input data was observed.
    Recorded,
    /// No input data was observed.
    Absent,
    /// Physical capture was disabled.
    Disabled,
    /// No usable observation channel.
    Unavailable,
}

/// Counts in distinct physical units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalCapture {
    /// Physical stream status.
    pub status: PhysicalStatus,
    /// Kernel event source.
    pub source: InputEventSource,
    /// Raw rows, unknown for bundles without this metadata.
    pub input_events: Option<u64>,
    /// Recognized strokes.
    pub gestures: u64,
}

/// Counts accumulated without retaining text or key arguments.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandCounts {
    /// All recognized command invocations including unsupported arguments.
    pub commands: u64,
    /// Requests for complete taps, swipes and drags.
    pub gestures: u64,
    /// Unsupported invocations.
    pub unparsed_commands: u64,
}

/// Machine-readable input evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCapture {
    /// Kernel evidence.
    pub physical: PhysicalCapture,
    /// Shell request evidence.
    pub injected: InjectedCapture,
    /// Limits of the observation channels.
    pub caveats: Vec<String>,
}

/// Evidence from injection channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectedCapture {
    /// Whether any request was observed.
    pub status: String,
    /// Sources represented in the counts.
    pub sources: Vec<InputEventSource>,
    /// Counts of observed requests.
    #[serde(flatten)]
    pub counts: CommandCounts,
    /// Meaning of observed timestamps.
    pub time_certainty: Option<InjectedTimeCertainty>,
}

impl InputCapture {
    /// Builds a summary for the caller's observation interval.
    pub fn new(
        status: PhysicalStatus,
        input_events: Option<u64>,
        gestures: u64,
        counts: CommandCounts,
    ) -> Self {
        let observed = counts.commands > 0;
        Self {
            physical: PhysicalCapture {
                status,
                source: InputEventSource::Getevent,
                input_events,
                gestures,
            },
            injected: InjectedCapture {
                status: if observed {
                    "commands_only"
                } else {
                    "not_observable"
                }
                .into(),
                sources: if observed {
                    vec![InputEventSource::AdbdCommand]
                } else {
                    vec![]
                },
                counts,
                time_certainty: observed.then_some(InjectedTimeCertainty::CommandReceived),
            },
            caveats: vec![
                "interactive_shell_not_visible".into(),
                "non_adb_injectors_not_visible".into(),
            ],
        }
    }
}
impl Default for InputCapture {
    fn default() -> Self {
        Self::new(
            PhysicalStatus::Unavailable,
            None,
            0,
            CommandCounts::default(),
        )
    }
}
