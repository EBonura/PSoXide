//! YUV4MPEG2 ("Y4M"), the decoded-picture form the cook reads.
//!
//! A Y4M stream is a one-line text header and then frames, each a `FRAME`
//! line followed by the raw planes. The cook takes 8-bit 4:2:0 only (the
//! `C420`, `C420jpeg`, `C420mpeg2` and `C420paldv` colour spaces), which is
//! what a video decoder hands out for nearly every source, and reads the
//! `XCOLORRANGE=FULL|LIMITED` extension for the sample range (limited when
//! absent, as most video is). The header fields used:
//!
//! ```text
//! YUV4MPEG2 W<width> H<height> F<num>:<den> [C<colour space>] [XCOLORRANGE=...]
//! ```
//!
//! Interlace, aspect and the other fields are read past and ignored.

use std::io::BufRead;

/// Sample range of the luma and chroma planes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Range {
    /// Luma 16..=235, chroma 16..=240 (video, "TV" range).
    Limited,
    /// Luma and chroma 0..=255 (JPEG, "PC" range).
    Full,
}

/// The stream header.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Header {
    /// Picture width in pixels.
    pub width: usize,
    /// Picture height in pixels.
    pub height: usize,
    /// Frame rate as numerator and denominator.
    pub fps: (u32, u32),
    /// Sample range.
    pub range: Range,
}

impl Header {
    /// Bytes of one chroma plane.
    fn chroma_bytes(&self) -> usize {
        self.width.div_ceil(2) * self.height.div_ceil(2)
    }

    /// Bytes of one frame's planes.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.width * self.height + 2 * self.chroma_bytes()
    }
}

/// One frame's planes, 8-bit, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Frame {
    /// Luma, `width * height` bytes.
    pub y: Vec<u8>,
    /// Blue difference, `ceil(width / 2) * ceil(height / 2)` bytes.
    pub cb: Vec<u8>,
    /// Red difference, the size of [`Frame::cb`].
    pub cr: Vec<u8>,
}

/// Reads frames from a Y4M stream.
pub struct Reader<R> {
    input: R,
    header: Header,
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn read_line<R: BufRead>(input: &mut R) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    if input.read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(invalid("a Y4M line ends without a newline".into()));
    }
    line.pop();
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| invalid("a Y4M line is not text".into()))
}

fn parse_header(line: &str) -> Result<Header, String> {
    let mut tokens = line.split(' ');
    if tokens.next() != Some("YUV4MPEG2") {
        return Err("not a YUV4MPEG2 stream".into());
    }
    let (mut width, mut height, mut fps) = (None, None, None);
    let mut range = Range::Limited;
    for token in tokens.filter(|t| !t.is_empty()) {
        let (tag, value) = token.split_at(1);
        match tag {
            "W" => width = value.parse().ok(),
            "H" => height = value.parse().ok(),
            "F" => {
                fps = value
                    .split_once(':')
                    .and_then(|(n, d)| Some((n.parse().ok()?, d.parse().ok()?)))
                    .filter(|&(n, d): &(u32, u32)| n > 0 && d > 0);
            }
            "C" if !matches!(value, "420" | "420jpeg" | "420mpeg2" | "420paldv") => {
                return Err(format!(
                    "colour space C{value}: the cook reads 8-bit 4:2:0 (C420*)"
                ));
            }
            "X" => match value {
                "COLORRANGE=FULL" => range = Range::Full,
                "COLORRANGE=LIMITED" => range = Range::Limited,
                _ => {}
            },
            _ => {}
        }
    }
    match (width, height, fps) {
        (Some(width), Some(height), Some(fps)) if width > 0 && height > 0 => Ok(Header {
            width,
            height,
            fps,
            range,
        }),
        _ => Err("the Y4M header lacks a width, height or frame rate".into()),
    }
}

impl<R: BufRead> Reader<R> {
    /// Reads the stream header.
    ///
    /// # Errors
    ///
    /// The input is not a 4:2:0 Y4M stream, or reading it failed.
    pub fn new(mut input: R) -> std::io::Result<Self> {
        let line = read_line(&mut input)?.ok_or_else(|| invalid("empty Y4M stream".into()))?;
        let header = parse_header(&line).map_err(invalid)?;
        Ok(Reader { input, header })
    }

    /// The stream header.
    pub fn header(&self) -> Header {
        self.header
    }

    /// The next frame, or `None` at the end of the stream.
    ///
    /// # Errors
    ///
    /// A frame is cut short or its marker line is not `FRAME`.
    pub fn next_frame(&mut self) -> std::io::Result<Option<Frame>> {
        let Some(line) = read_line(&mut self.input)? else {
            return Ok(None);
        };
        if line != "FRAME" && !line.starts_with("FRAME ") {
            return Err(invalid(format!("expected a FRAME line, found {line:?}")));
        }
        let luma = self.header.width * self.header.height;
        let chroma = self.header.chroma_bytes();
        let mut read = |n: usize| -> std::io::Result<Vec<u8>> {
            let mut plane = vec![0u8; n];
            self.input.read_exact(&mut plane)?;
            Ok(plane)
        };
        Ok(Some(Frame {
            y: read(luma)?,
            cb: read(chroma)?,
            cr: read(chroma)?,
        }))
    }
}

/// Writes a Y4M header, for tests and for tools that hand the cook pictures.
#[must_use]
pub fn header_line(width: usize, height: usize, fps: (u32, u32), range: Range) -> String {
    let range = match range {
        Range::Limited => "LIMITED",
        Range::Full => "FULL",
    };
    format!(
        "YUV4MPEG2 W{width} H{height} F{}:{} Ip A1:1 C420jpeg XCOLORRANGE={range}\n",
        fps.0, fps.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_header_and_two_frames() {
        let mut data = header_line(4, 2, (15, 1), Range::Full).into_bytes();
        for fill in [10u8, 20] {
            data.extend_from_slice(b"FRAME\n");
            data.extend(std::iter::repeat_n(fill, 8 + 2 + 2));
        }
        let mut r = Reader::new(&data[..]).unwrap();
        let h = r.header();
        assert_eq!(
            (h.width, h.height, h.fps, h.range),
            (4, 2, (15, 1), Range::Full)
        );
        assert_eq!(h.frame_bytes(), 12);
        assert_eq!(r.next_frame().unwrap().unwrap().y, vec![10; 8]);
        assert_eq!(r.next_frame().unwrap().unwrap().cr, vec![20; 2]);
        assert_eq!(r.next_frame().unwrap(), None);
    }

    #[test]
    fn limited_range_is_the_default_and_444_is_refused() {
        let h = parse_header("YUV4MPEG2 W640 H480 F30000:1001 Ip").unwrap();
        assert_eq!((h.range, h.fps), (Range::Limited, (30000, 1001)));
        assert!(parse_header("YUV4MPEG2 W640 H480 F15:1 C444").is_err());
        assert!(parse_header("YUV4MPEG2 W640 F15:1").is_err());
        assert!(parse_header("RIFF").is_err());
    }

    #[test]
    fn a_short_frame_is_an_error() {
        let mut data = header_line(4, 2, (15, 1), Range::Full).into_bytes();
        data.extend_from_slice(b"FRAME\n\0\0\0");
        let mut r = Reader::new(&data[..]).unwrap();
        assert!(r.next_frame().is_err());
    }
}
