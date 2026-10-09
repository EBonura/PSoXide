// SPDX-License-Identifier: GPL-2.0-or-later
//! The text an options row shows for a stepper value.

/// Text for one options row value, held inline.
///
/// Every label is upper-case ASCII, so it draws with any font that has
/// capitals and digits. The longest, `BRIGHTER 5` and `RIGHT 127`, fit with
/// room to spare; [`Label::CAPACITY`] is large enough for any `i8` step.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Label {
    bytes: [u8; Label::CAPACITY],
    len: u8,
}

impl Label {
    /// Bytes a label can hold: the longest word plus a space and a
    /// three-digit step.
    pub const CAPACITY: usize = 12;

    /// A label that reads `word` alone, such as `DEFAULT` or `CENTRE`.
    pub const fn word(word: &[u8]) -> Self {
        let mut bytes = [b' '; Self::CAPACITY];
        let len = if word.len() < Self::CAPACITY {
            word.len()
        } else {
            Self::CAPACITY
        };
        let mut at = 0;
        while at < len {
            bytes[at] = word[at];
            at += 1;
        }
        Self {
            bytes,
            len: len as u8,
        }
    }

    /// A label that reads `word`, a space and the decimal digits of `steps`,
    /// such as `DARKER 3`.
    pub const fn toward(word: &[u8], steps: u8) -> Self {
        let mut label = Self::word(word);
        let mut at = label.len as usize;
        // Cut the word if the digits would not fit; no shipped word is near.
        let digits = if steps >= 100 {
            3
        } else if steps >= 10 {
            2
        } else {
            1
        };
        if at + 1 + digits > Self::CAPACITY {
            at = Self::CAPACITY - 1 - digits;
        }
        label.bytes[at] = b' ';
        at += 1;
        let mut place = digits;
        let mut rest = steps;
        while place > 0 {
            place -= 1;
            label.bytes[at + place] = b'0' + rest % 10;
            rest /= 10;
        }
        label.len = (at + digits) as u8;
        label
    }

    /// The label's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// The label as text. A label is always ASCII, so this never comes back
    /// empty for a built label.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    /// Length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the label has no text. A built label never is.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies the label into `buf` and returns its length, for a row table
    /// whose value callback fills a `[u8; 12]` and returns the byte count.
    pub fn copy_into(&self, buf: &mut [u8; Self::CAPACITY]) -> usize {
        *buf = self.bytes;
        self.len as usize
    }
}

#[cfg(test)]
mod tests {
    use super::Label;

    #[test]
    fn word_and_toward_build_the_row_text() {
        assert_eq!(Label::word(b"DEFAULT").as_str(), "DEFAULT");
        assert_eq!(Label::toward(b"DARKER", 3).as_str(), "DARKER 3");
        assert_eq!(Label::toward(b"RIGHT", 16).as_str(), "RIGHT 16");
        assert_eq!(Label::toward(b"LEFT", 0).as_str(), "LEFT 0");
    }

    #[test]
    fn every_step_of_an_i8_fits() {
        for steps in 0..=u8::MAX {
            let label = Label::toward(b"BRIGHTER", steps);
            let mut want = [0u8; 16];
            let text = b"BRIGHTER ";
            want[..text.len()].copy_from_slice(text);
            let mut at = text.len();
            let mut digits = [0u8; 3];
            let mut n = 0;
            let mut rest = steps;
            loop {
                digits[n] = b'0' + rest % 10;
                n += 1;
                rest /= 10;
                if rest == 0 {
                    break;
                }
            }
            while n > 0 {
                n -= 1;
                want[at] = digits[n];
                at += 1;
            }
            assert_eq!(label.as_bytes(), &want[..at], "steps {steps}");
        }
    }

    #[test]
    fn copy_into_matches_the_bytes() {
        let label = Label::toward(b"UP", 7);
        let mut buf = [0u8; Label::CAPACITY];
        let len = label.copy_into(&mut buf);
        assert_eq!(&buf[..len], b"UP 7");
        assert_eq!(len, label.len());
        assert!(!label.is_empty());
    }
}
