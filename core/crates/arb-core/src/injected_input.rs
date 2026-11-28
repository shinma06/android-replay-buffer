//! Derives requested shell inputs from authenticated adbd service log records.
use crate::bundle::{LogRecord, RecordBucket};
use crate::time::TimeAlignment;
use serde::{Deserialize, Serialize};
use serde_json::Number;

/// Origin of an observed input.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InputEventSource {
    /// Kernel input device.
    Getevent,
    /// Service request received by adbd.
    AdbdCommand,
    /// Instrumented external input producer.
    ExternalInjected,
}

/// Observation point of the associated timestamp.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InjectedTimeCertainty {
    /// adbd received a shell request.
    CommandReceived,
    /// A producer observed dispatch.
    Dispatched,
    /// Timestamp of the input itself.
    Exact,
}

/// Parsed arguments. Only coordinate-bearing variants carry display pixels.
///
/// Coordinates keep Android's `Float.parseFloat` domain; integral arguments stay integers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[non_exhaustive]
pub enum InputCommand {
    /// Single touch.
    Tap {
        /// Horizontal display pixel.
        x_display: Number,
        /// Vertical display pixel.
        y_display: Number,
    },
    /// Swipe between two display points.
    Swipe(Stroke),
    /// Drag between two display points.
    Draganddrop(Stroke),
    /// One or more key names or codes.
    Keyevent {
        /// Long press requested.
        longpress: bool,
        /// Key names or numbers in request order.
        keycodes: Vec<String>,
    },
    /// Argument to the Android text command.
    Text {
        /// Text argument before Android interprets escapes.
        text: String,
    },
    /// A single motion sample.
    Motionevent {
        /// Motion action name.
        action: String,
        /// Horizontal display pixel.
        x_display: Number,
        /// Vertical display pixel.
        y_display: Number,
    },
    /// Relative trackball motion.
    Roll {
        /// Horizontal displacement.
        dx: Number,
        /// Vertical displacement.
        dy: Number,
    },
}

/// Display coordinates and requested duration of a stroke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stroke {
    /// Start X.
    pub x_display: Number,
    /// Start Y.
    pub y_display: Number,
    /// End X.
    pub end_x_display: Number,
    /// End Y.
    pub end_y_display: Number,
    /// Explicit duration, if supplied.
    pub duration_ms: Option<u32>,
}

impl InputCommand {
    /// Whether the request describes a whole physical-style gesture.
    pub const fn is_gesture(&self) -> bool {
        matches!(
            self,
            Self::Tap { .. } | Self::Swipe(_) | Self::Draganddrop(_)
        )
    }

    /// Starting position in display pixels (roll is relative).
    pub fn position(&self) -> Option<(f64, f64)> {
        let (x, y) = match self {
            Self::Tap {
                x_display,
                y_display,
            }
            | Self::Motionevent {
                x_display,
                y_display,
                ..
            } => (x_display, y_display),
            Self::Swipe(s) | Self::Draganddrop(s) => (&s.x_display, &s.y_display),
            _ => return None,
        };
        x.as_f64().zip(y.as_f64())
    }
}

/// Input evidence tied to its source log and epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectedInput {
    /// Producer of this observation.
    pub source: InputEventSource,
    /// True for framework injection requests.
    pub injected: bool,
    /// Explicit Android display selection.
    pub display_id: Option<i32>,
    /// Requested operation.
    #[serde(flatten)]
    pub command: InputCommand,
    /// Epoch of the anchored log record.
    pub boot_id: Option<String>,
    /// Aligned request time if anchored.
    pub boottime_ns: Option<i64>,
    /// Meaning of the recorded time.
    pub time_certainty: InjectedTimeCertainty,
    /// API-dependent nominal timing allowance, unrelated to shell completion.
    pub latency_bound_ms: u32,
    /// Sequence in the original log.
    pub source_log_seq: u64,
}

/// Parsed requests and recognized input commands with unsupported arguments.
#[derive(Debug, Default)]
pub struct InjectedCommandExtraction {
    /// Successfully parsed commands in log and shell order.
    pub inputs: Vec<InjectedInput>,
    /// Input commands rejected by the grammar.
    pub unparsed_count: usize,
}

/// Only the adbd tag from root or shell is eligible.
pub fn trusted_adbd(tag: Option<&str>, uid: Option<u32>) -> bool {
    matches!(uid, Some(0 | 2000)) && tag == Some("adbd")
}

/// Reads service requests without executing or expanding shell expressions.
pub fn extract_injected_commands(
    logs: &[LogRecord],
    api_level: Option<u32>,
) -> InjectedCommandExtraction {
    let mut result = InjectedCommandExtraction::default();
    for log in logs {
        result.unparsed_count += visit_commands(
            log.tag.as_deref(),
            log.uid,
            &log.message,
            |display_id, command| {
                let (boot_id, boottime_ns) = match (&log.alignment, log.bucket) {
                    (
                        TimeAlignment::Anchored {
                            boot_id,
                            boottime_ns,
                            ..
                        },
                        RecordBucket::Main,
                    ) => (Some(boot_id.clone()), Some(*boottime_ns)),
                    _ => (None, None),
                };
                result.inputs.push(InjectedInput {
                    source: InputEventSource::AdbdCommand,
                    injected: true,
                    command,
                    display_id,
                    boot_id,
                    boottime_ns,
                    time_certainty: InjectedTimeCertainty::CommandReceived,
                    latency_bound_ms: if api_level.unwrap_or(0) >= 30 {
                        50
                    } else {
                        500
                    },
                    source_log_seq: log.seq,
                });
            },
        );
    }
    result
}

/// Visits parsed requests from one log, returning the number of unsupported input commands.
pub fn visit_commands(
    tag: Option<&str>,
    uid: Option<u32>,
    message: &str,
    mut visit: impl FnMut(Option<i32>, InputCommand),
) -> usize {
    if !trusted_adbd(tag, uid) {
        return 0;
    }
    let Some((service, command)) = message
        .strip_prefix("adbd service requested '")
        .and_then(|m| m.strip_suffix('\''))
        .and_then(|m| m.split_once(':'))
    else {
        return 0;
    };
    if service.split(',').next() != Some("shell") {
        return 0;
    }
    let mut bad = 0;
    let mut original_directory = true;
    let mut original_path = true;
    lex(command, |words, valid| {
        let mut args = words.iter().map(String::as_str);
        let executable = match args.next() {
            Some("cd") => {
                original_directory = false;
                return;
            }
            Some("/system/bin/input") => "input",
            Some("/system/bin/cmd") => "cmd",
            Some("input") if original_path => "input",
            Some("cmd") if original_path => "cmd",
            Some("./system/bin/input" | "system/bin/input") if original_directory => "input",
            Some("./system/bin/cmd" | "system/bin/cmd") if original_directory => "cmd",
            first => {
                // A changed PATH may resolve bare names to a local executable.
                if words.iter().any(|w| w.starts_with("PATH="))
                    || (first == Some("unset") && words.iter().any(|w| w == "PATH"))
                {
                    original_path = false;
                }
                return;
            }
        };
        if executable == "cmd" && args.next() != Some("input") {
            return;
        }
        if valid && let Some((display, command)) = parse(&args.collect::<Vec<_>>()) {
            visit(display, command);
        } else {
            bad += 1;
        }
    });
    bad
}

// Accept a literal shell subset. Stop at unsupported shell constructs: a semicolon inside
// a comment, substitution or nested shell must never introduce a synthetic command.
fn lex(line: &str, mut emit: impl FnMut(&[String], bool)) {
    let mut chars = line.chars().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut valid = true;
    while let Some(c) = chars.next() {
        match c {
            c if Some(c) == quote => quote = None,
            '\\' if quote != Some('\'') => {
                let Some(next) = chars.next() else {
                    valid = false;
                    break;
                };
                if quote == Some('"') && !matches!(next, '"' | '\\' | '$' | '`') {
                    word.push('\\');
                }
                word.push(next);
                started = true;
            }
            '$' | '`' if quote != Some('\'') => {
                valid = false;
                break;
            }
            _ if quote.is_some() => word.push(c),
            '\'' | '"' => {
                quote = Some(c);
                started = true;
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '#' if !started => break,
            ';' | '&' if c == ';' || chars.next_if_eq(&'&').is_some() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
                emit(&words, true);
                words.clear();
            }
            '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | '~' => {
                valid = false;
                break;
            }
            _ if c.is_control() => {
                valid = false;
                break;
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    emit(&words, valid && quote.is_none());
}

fn parse(mut args: &[&str]) -> Option<(Option<i32>, InputCommand)> {
    let mut display = None;
    if let ["-d", id, rest @ ..] = args {
        display = Some(id.parse().ok()?);
        args = rest;
    }
    if let [
        "keyboard" | "mouse" | "touchscreen" | "touchpad" | "trackball" | "stylus" | "dpad"
        | "gamepad" | "touchnavigation" | "joystick",
        rest @ ..,
    ] = args
    {
        args = rest;
    }
    if display.is_none()
        && let ["-d", id, rest @ ..] = args
    {
        display = Some(id.parse().ok()?);
        args = rest;
    }
    let command = match args {
        ["tap", x, y] => InputCommand::Tap {
            x_display: coordinate(x)?,
            y_display: coordinate(y)?,
        },
        [
            op @ ("swipe" | "draganddrop"),
            x,
            y,
            end_x,
            end_y,
            duration @ ..,
        ] => {
            let duration_ms = match duration {
                [] => None,
                [ms] => Some(ms.parse().ok()?),
                _ => return None,
            };
            let stroke = Stroke {
                x_display: coordinate(x)?,
                y_display: coordinate(y)?,
                end_x_display: coordinate(end_x)?,
                end_y_display: coordinate(end_y)?,
                duration_ms,
            };
            if *op == "swipe" {
                InputCommand::Swipe(stroke)
            } else {
                InputCommand::Draganddrop(stroke)
            }
        }
        ["keyevent", keys @ ..] => {
            let rest = keys.strip_prefix(&["--longpress"]);
            let keycodes = rest.unwrap_or(keys);
            if keycodes.is_empty()
                || keycodes.iter().any(|k| {
                    k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
            {
                return None;
            }
            InputCommand::Keyevent {
                longpress: rest.is_some(),
                keycodes: keycodes.iter().map(|k| (*k).to_owned()).collect(),
            }
        }
        ["text", s] => InputCommand::Text {
            text: (*s).to_owned(),
        },
        ["motionevent", a, x, y]
            if matches!(
                a.to_ascii_uppercase().as_str(),
                "DOWN" | "UP" | "MOVE" | "CANCEL"
            ) =>
        {
            InputCommand::Motionevent {
                action: a.to_ascii_uppercase(),
                x_display: coordinate(x)?,
                y_display: coordinate(y)?,
            }
        }
        ["roll", x, y] => InputCommand::Roll {
            dx: coordinate(x)?,
            dy: coordinate(y)?,
        },
        _ => return None,
    };
    Some((display, command))
}

// Android reads coordinates with Float.parseFloat; reject what it would turn into NaN or infinity.
fn coordinate(arg: &str) -> Option<Number> {
    if let Ok(n) = arg.parse::<i64>() {
        return Some(n.into());
    }
    arg.parse::<f32>().ok().filter(|v| v.is_finite())?;
    Number::from_f64(arg.parse().ok()?)
}
