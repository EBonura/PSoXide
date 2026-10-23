//! Retargeting a cue sheet at the library name.

use crate::Error;
use std::path::Path;

/// `text` with its first `FILE` line replaced by one that points at `<name>.bin`, quoted
/// (the name has spaces). Every other line is kept; line endings become `\n`.
pub(crate) fn retarget(text: &str, name: &str, cue: &Path) -> Result<String, Error> {
    let mut replaced = false;
    let mut output = String::with_capacity(text.len() + name.len());
    for line in text.lines() {
        if !replaced && line.trim_start().starts_with("FILE ") {
            output.push_str(&format!("FILE \"{name}.bin\" BINARY\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if replaced {
        Ok(output)
    } else {
        Err(Error::CueWithoutFile(cue.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_line_is_quoted_and_the_rest_kept() {
        let text = "FILE repo.bin BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n";
        assert_eq!(
            retarget(text, "Game Name (repo)", Path::new("x.cue")).unwrap(),
            "FILE \"Game Name (repo).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
        );
    }

    #[test]
    fn only_the_first_file_line_is_replaced() {
        let text = "FILE a.bin BINARY\n  TRACK 01 AUDIO\nFILE b.bin BINARY\n";
        let output = retarget(text, "N", Path::new("x.cue")).unwrap();
        assert_eq!(
            output,
            "FILE \"N.bin\" BINARY\n  TRACK 01 AUDIO\nFILE b.bin BINARY\n"
        );
    }

    #[test]
    fn a_cue_without_a_file_line_is_rejected() {
        let error = retarget("TRACK 01 MODE2/2352\n", "N", Path::new("x.cue")).unwrap_err();
        assert!(matches!(error, Error::CueWithoutFile(_)), "{error}");
    }
}
