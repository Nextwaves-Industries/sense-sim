//! Text that came from a device is hostile until cleaned.
//!
//! Station names, certificate subjects and error messages are all set by whoever
//! configured (or compromised) the device, and they end up in terminals, log lines,
//! the viewer and hook input. Cleaning here keeps a name from rewriting a log line,
//! reordering what an operator reads, or looking identical to a different name.

pub const MAX_DEVICE_TEXT_CHARS: usize = 256;

/// Removes control, bidirectional and invisible characters, then caps the length.
pub fn clean_device_text(raw: &str) -> String {
    raw.chars().filter(|c| !is_hidden_or_control(*c)).take(MAX_DEVICE_TEXT_CHARS).collect()
}

fn is_hidden_or_control(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Bidirectional marks, embeddings, overrides and isolates.
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            // Invisible characters that make two different names look the same.
            | '\u{00AD}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_untouched() {
        for name in ["PLC_Line3-CPU1516", "Förderband 7", "ライン1", "& calc.exe"] {
            assert_eq!(clean_device_text(name), name);
        }
    }

    #[test]
    fn line_breaks_and_escapes_cannot_forge_a_log_line() {
        assert_eq!(clean_device_text("ok\r\n2026-11-02 fake entry"), "ok2026-11-02 fake entry");
        assert_eq!(clean_device_text("\u{1b}[31mred\u{1b}[0m"), "[31mred[0m");
    }

    #[test]
    fn bidi_overrides_cannot_reorder_what_an_operator_reads() {
        assert_eq!(clean_device_text("safe\u{202E}exe.bat"), "safeexe.bat");
        assert_eq!(clean_device_text("\u{2066}a\u{2069}\u{200F}"), "a");
    }

    #[test]
    fn invisible_characters_cannot_disguise_one_name_as_another() {
        assert_eq!(clean_device_text("PLC\u{200B}1"), clean_device_text("PLC1"));
    }

    #[test]
    fn length_is_capped_in_characters_not_bytes() {
        assert_eq!(clean_device_text(&"a".repeat(1000)).chars().count(), MAX_DEVICE_TEXT_CHARS);
        assert_eq!(clean_device_text(&"ü".repeat(1000)).chars().count(), MAX_DEVICE_TEXT_CHARS);
    }
}
