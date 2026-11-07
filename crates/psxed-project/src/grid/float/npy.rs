//! Reader for NumPy `.npy` files holding one little-endian `f32` array.
//!
//! The format is a magic string, a version, a header length, an ASCII
//! dictionary naming the element type, the memory order and the shape, and
//! the raw elements. Only what the Citadel conversion writes is accepted:
//! `<f4`, C order, any number of dimensions.

/// Magic bytes every `.npy` file starts with.
const MAGIC: &[u8] = b"\x93NUMPY";

/// A parsed array.
#[derive(Clone, Debug, PartialEq)]
pub struct NpyF32 {
    /// Length of each dimension, outermost first.
    pub shape: Vec<usize>,
    /// Elements in C order.
    pub data: Vec<f32>,
}

/// Parse the bytes of a `.npy` file.
///
/// # Errors
///
/// Returns a message naming the first thing wrong: a missing magic, an
/// unsupported version, element type or memory order, a malformed header, or
/// a body that does not hold the shape's element count.
pub fn parse_f32(bytes: &[u8]) -> Result<NpyF32, String> {
    if !bytes.starts_with(MAGIC) || bytes.len() < 10 {
        return Err("not a .npy file (no magic)".to_string());
    }
    let (header_start, header_len) = match bytes[6] {
        1 => (10, usize::from(u16::from_le_bytes([bytes[8], bytes[9]]))),
        2 | 3 => {
            let raw = bytes
                .get(8..12)
                .ok_or_else(|| "truncated .npy header".to_string())?;
            let len = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            (12, len as usize)
        }
        other => return Err(format!("unsupported .npy version {other}")),
    };
    let header = bytes
        .get(header_start..header_start + header_len)
        .ok_or_else(|| "truncated .npy header".to_string())?;
    let header =
        std::str::from_utf8(header).map_err(|_| "the .npy header is not text".to_string())?;
    let descr = field(header, "descr")?;
    if !(descr.starts_with("'<f4'") || descr.starts_with("\"<f4\"")) {
        return Err(format!(
            "only little-endian f32 arrays are read, got {descr}"
        ));
    }
    if !field(header, "fortran_order")?.starts_with("False") {
        return Err("a Fortran-order array is not read".to_string());
    }
    let shape = shape(header)?;
    let count: usize = shape.iter().product();
    let body = &bytes[header_start + header_len..];
    if body.len() != count * 4 {
        return Err(format!(
            "the shape {shape:?} needs {} bytes, the file holds {}",
            count * 4,
            body.len()
        ));
    }
    let data = body
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(NpyF32 { shape, data })
}

/// The text after `'key':` in the header dictionary.
fn field<'a>(header: &'a str, key: &str) -> Result<&'a str, String> {
    let needle = format!("'{key}':");
    let at = header
        .find(&needle)
        .ok_or_else(|| format!("the .npy header has no {key}"))?;
    Ok(header[at + needle.len()..].trim_start())
}

/// The tuple after `'shape':`.
fn shape(header: &str) -> Result<Vec<usize>, String> {
    let rest = field(header, "shape")?;
    let open = rest
        .strip_prefix('(')
        .ok_or_else(|| "the .npy shape is not a tuple".to_string())?;
    let close = open
        .find(')')
        .ok_or_else(|| "the .npy shape is not closed".to_string())?;
    open[..close]
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<usize>()
                .map_err(|_| format!("bad .npy dimension {part:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes of a version 1 file holding `data` with `shape`.
    pub(crate) fn encode(shape: &[usize], data: &[f32]) -> Vec<u8> {
        let tuple = match shape {
            [one] => format!("({one},)"),
            many => format!(
                "({})",
                many.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': {tuple}, }}");
        // Pad so magic + version + length + header ends on a 64 byte line.
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut out = MAGIC.to_vec();
        out.extend([1, 0]);
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        for v in data {
            out.extend(v.to_le_bytes());
        }
        out
    }

    #[test]
    fn an_array_round_trips_with_its_shape() {
        let data: Vec<f32> = (0..24).map(|v| v as f32 * 0.5).collect();
        let parsed = parse_f32(&encode(&[2, 3, 4], &data)).unwrap();
        assert_eq!(parsed.shape, vec![2, 3, 4]);
        assert_eq!(parsed.data, data);
        let flat = parse_f32(&encode(&[3], &[1.0, 2.0, 3.0])).unwrap();
        assert_eq!(flat.shape, vec![3]);
    }

    #[test]
    fn nan_survives() {
        let parsed = parse_f32(&encode(&[2], &[f32::NAN, 1.0])).unwrap();
        assert!(parsed.data[0].is_nan() && parsed.data[1] == 1.0);
    }

    #[test]
    fn a_wrong_file_is_named() {
        assert!(parse_f32(b"hello world, not an array").is_err());
        let mut bytes = encode(&[2], &[1.0, 2.0]);
        bytes.truncate(bytes.len() - 1);
        assert!(parse_f32(&bytes).unwrap_err().contains("needs 8 bytes"));
        let mut doubles = encode(&[1], &[1.0]);
        let at = doubles.windows(3).position(|w| w == b"<f4").unwrap();
        doubles[at + 2] = b'8';
        assert!(parse_f32(&doubles).unwrap_err().contains("f32"));
    }
}
