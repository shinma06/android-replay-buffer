//! Display form of host paths (#586-E).
//!
//! Windows canonicalization returns extended-length (verbatim) paths such as `\\?\C:\data\b1` or
//! `\\?\UNC\server\share\b1`. The catalog keeps those canonical paths for comparisons; only the
//! `path` a person or agent reads is shortened here. The rules are plain string rules, so every
//! OS runs the same function and the same tests.
//!
//! A prefix is removed only when the shorter Win32 form names exactly the same file: a verbatim
//! path skips Win32 normalization, so a path that normalization would change (`.`/`..`, trailing
//! dots or spaces, `/`, reserved device names, empty components, invalid characters) or that is
//! too long for the classic `MAX_PATH` limit keeps its prefix.
use std::borrow::Cow;
use std::path::Path;

/// Longest display path without the extended-length prefix (`MAX_PATH` minus the terminator).
pub const MAX_PLAIN_PATH_CHARS: usize = 259;

const VERBATIM: &str = r"\\?\";

/// The display text of `path`: [`display_path_str`] applied to its lossy UTF-8 rendering.
#[must_use]
pub fn display_path(path: &Path) -> String {
    display_path_str(&path.to_string_lossy()).into_owned()
}

/// Removes a Windows extended-length prefix when the plain form names the same file.
///
/// - `\\?\C:\dir\file` becomes `C:\dir\file`.
/// - `\\?\UNC\server\share\dir` becomes `\\server\share\dir`.
/// - Anything else, including `\\?\Volume{…}\`, `\\.\` device paths, POSIX paths and any path
///   that fails a safety check, is returned unchanged.
#[must_use]
pub fn display_path_str(text: &str) -> Cow<'_, str> {
    let Some(rest) = text.strip_prefix(VERBATIM) else {
        return Cow::Borrowed(text);
    };
    let plain = if let Some(unc) = strip_unc(rest) {
        unc_form(unc)
    } else {
        drive_form(rest).map(Cow::Borrowed)
    };
    match plain {
        Some(plain) if plain.encode_utf16().count() <= MAX_PLAIN_PATH_CHARS => plain,
        _ => Cow::Borrowed(text),
    }
}

/// The text after `UNC\` (case-insensitive), if the verbatim path is a UNC one.
fn strip_unc(rest: &str) -> Option<&str> {
    let head = rest.get(..4)?;
    head.eq_ignore_ascii_case(r"UNC\").then(|| &rest[4..])
}

/// `C:\…` stays as it is when the drive letter, root and every component are plain.
fn drive_form(rest: &str) -> Option<&str> {
    let bytes = rest.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    let tail = &rest[3..];
    (tail.is_empty() || plain_components(tail)).then_some(rest)
}

/// `server\share[\…]` becomes `\\server\share[\…]` when every component is plain.
fn unc_form(unc: &str) -> Option<Cow<'_, str>> {
    let mut parts = unc.splitn(3, '\\');
    let server = parts.next()?;
    let share = parts.next()?;
    if !plain_component(server) || !plain_component(share) {
        return None;
    }
    if let Some(tail) = parts.next()
        && !(tail.is_empty() || plain_components(tail))
    {
        return None;
    }
    Some(Cow::Owned(format!(r"\\{unc}")))
}

/// Every `\`-separated component is plain. A single trailing `\` is allowed (a directory).
fn plain_components(tail: &str) -> bool {
    let tail = tail.strip_suffix('\\').unwrap_or(tail);
    !tail.is_empty() && tail.split('\\').all(plain_component)
}

/// A component Win32 normalization leaves untouched and that is not a device name.
fn plain_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.ends_with(['.', ' '])
        && !component
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
        && !reserved_device_name(component)
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9` (also with superscript digits) and
/// `CONIN$`/`CONOUT$`, with or without an extension, compared case-insensitively.
fn reserved_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let stem = stem.trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let digit: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && matches!(digit.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
}

#[cfg(test)]
mod tests {
    use super::{MAX_PLAIN_PATH_CHARS, display_path, display_path_str};
    use proptest::prelude::*;
    use std::path::Path;

    #[test]
    fn drive_and_unc_prefixes_are_removed() {
        for (input, expected) in [
            (
                r"\\?\C:\Users\me\arb\bundles\b1",
                r"C:\Users\me\arb\bundles\b1",
            ),
            (r"\\?\d:\data\", r"d:\data\"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\arb\b1", r"\\server\share\arb\b1"),
            (r"\\?\unc\server\share", r"\\server\share"),
            (r"\\?\C:\日本語\記録", r"C:\日本語\記録"),
            (r"\\?\C:\a.b\file.txt", r"C:\a.b\file.txt"),
            (r"\\?\C:\console\com10\lpt", r"C:\console\com10\lpt"),
        ] {
            assert_eq!(display_path_str(input), expected, "{input}");
        }
    }

    #[test]
    fn unsafe_or_foreign_paths_are_unchanged() {
        let long = format!(r"\\?\C:\{}", "a".repeat(MAX_PLAIN_PATH_CHARS));
        for input in [
            "/home/me/.local/share/arb/bundles/b1",
            r"C:\already\plain",
            r"\\server\share\plain",
            r"\\.\COM1",
            r"\\?\Volume{0b5c2c7e-0000-0000-0000-100000000000}\dir",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
            r"\\?\C:",
            r"\\?\C:relative",
            r"\\?\C:\a\..\b",
            r"\\?\C:\a\.\b",
            r"\\?\C:\trailing.",
            r"\\?\C:\trailing \x",
            r"\\?\C:\a\\b",
            r"\\?\C:\a/b",
            r"\\?\C:\a\b?c",
            r"\\?\C:\a\b:stream",
            r"\\?\C:\dir\CON",
            r"\\?\C:\dir\nul.txt",
            r"\\?\C:\dir\Com1.log",
            r"\\?\C:\dir\LPT²",
            r"\\?\C:\dir\conout$",
            r"\\?\C:\dir\aux .txt",
            r"\\?\UNC\server",
            r"\\?\UNC\\share\x",
            r"\\?\UNC\server\\x",
            r"\\?\UNC\server\share\..\x",
            r"\\?\",
            r"\\?\UNC\",
            long.as_str(),
        ] {
            assert_eq!(display_path_str(input), input, "{input}");
        }
    }

    #[test]
    fn the_length_limit_counts_utf16_units() {
        let fits = format!(r"\\?\C:\{}", "a".repeat(MAX_PLAIN_PATH_CHARS - 3));
        assert_eq!(display_path_str(&fits).len(), MAX_PLAIN_PATH_CHARS);
        let over = format!(r"\\?\C:\{}", "a".repeat(MAX_PLAIN_PATH_CHARS - 2));
        assert_eq!(display_path_str(&over), over.as_str());
        // A supplementary-plane character is two UTF-16 units.
        let wide = format!(r"\\?\C:\{}", "😀".repeat(129));
        assert_eq!(display_path_str(&wide), wide.as_str());
    }

    #[test]
    fn path_wrapper_matches_the_string_rules() {
        assert_eq!(display_path(Path::new("/tmp/b1")), "/tmp/b1");
        assert_eq!(display_path(Path::new(r"\\?\C:\x")), r"C:\x");
    }

    fn component() -> impl Strategy<Value = String> {
        "[A-Za-z0-9 ._$¹-]{1,12}"
    }

    proptest! {
        /// The result is either the input or the input without the prefix, never anything else,
        /// and a stripped drive path re-prefixed is the original text.
        #[test]
        fn output_is_the_input_or_its_exact_plain_form(
            drive in "[A-Za-z]",
            parts in proptest::collection::vec(component(), 0..8),
            unc in any::<bool>(),
        ) {
            let joined = parts.join("\\");
            let input = if unc {
                format!(r"\\?\UNC\{joined}")
            } else {
                format!(r"\\?\{drive}:\{joined}")
            };
            let out = display_path_str(&input);
            if out != input.as_str() {
                let restored = if unc {
                    format!(r"\\?\UNC\{}", out.strip_prefix(r"\\").unwrap())
                } else {
                    format!(r"\\?\{out}")
                };
                prop_assert_eq!(restored, input.clone());
                prop_assert!(!out.starts_with(r"\\?\"));
                prop_assert!(out.encode_utf16().count() <= MAX_PLAIN_PATH_CHARS);
                for part in &parts {
                    prop_assert!(part != "." && part != "..");
                    prop_assert!(!part.ends_with(['.', ' ']));
                }
                if unc {
                    prop_assert!(parts.len() >= 2);
                }
            }
            // Idempotent: the display form of a display form is itself.
            prop_assert_eq!(display_path_str(&out), out.clone());
        }

        /// Text without the verbatim prefix is always returned untouched.
        #[test]
        fn non_verbatim_text_is_unchanged(text in "\\PC{0,64}") {
            prop_assume!(!text.starts_with(r"\\?\"));
            prop_assert_eq!(display_path_str(&text), text.as_str());
        }
    }
}
