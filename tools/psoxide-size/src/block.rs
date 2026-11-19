//! Find a versioned state block in a PS-X EXE.
//!
//! A game that lets a gate read its state from emulator RAM places a statically
//! initialised block (a four-byte magic, a version word, its own length, zero
//! padding) in `.data`. The gate needs the block's address in the exact
//! executable that gets packed, so this searches the declared image for the
//! one block that matches and adds its offset to the load address.

const HEADER_LEN: usize = 0x800;

/// What identifies a block: magic, version word, total length, and the name the
/// address is reported under.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Spec {
    /// The four magic bytes at the start of the block.
    pub magic: [u8; 4],
    /// The version word that follows the magic.
    pub version: u32,
    /// The block's total length in bytes, also stored in its third word.
    pub len: usize,
    /// The name the address is reported under.
    pub name: String,
}

impl Spec {
    /// Parse `MAGIC:VERSION:LEN:NAME`, for example `GATE:1:256:GAME_GATE_STATE`.
    ///
    /// # Errors
    /// The text is not four fields, the magic is not four bytes, or a number does not parse.
    pub fn parse(text: &str) -> Result<Spec, String> {
        let bad = || format!("block spec `{text}` is not MAGIC:VERSION:LEN:NAME");
        let mut parts = text.splitn(4, ':');
        let magic = parts.next().ok_or_else(bad)?.as_bytes();
        let magic: [u8; 4] = magic
            .try_into()
            .map_err(|_| format!("block spec `{text}`: the magic is exactly four bytes"))?;
        let version = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let len = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let name = parts.next().ok_or_else(bad)?.to_string();
        if len < 12 || name.is_empty() {
            return Err(bad());
        }
        Ok(Spec {
            magic,
            version,
            len,
            name,
        })
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or("short PS-X EXE header")?;
    Ok(u32::from_le_bytes(
        slice.try_into().map_err(|_| "short word")?,
    ))
}

/// The address of the one initialised block `spec` describes in `exe`.
///
/// # Errors
/// `exe` is not a PS-X EXE with its whole payload, or the block is missing or ambiguous.
pub fn locate(exe: &[u8], spec: &Spec) -> Result<u32, String> {
    if exe.get(..8) != Some(b"PS-X EXE") {
        return Err("input is not a PS-X EXE".into());
    }
    let load = word(exe, 0x18)?;
    let image_len = word(exe, 0x1c)? as usize;
    let image = exe
        .get(HEADER_LEN..HEADER_LEN + image_len)
        .ok_or("PS-X EXE payload is shorter than its header says")?;
    let mut matches = Vec::new();
    for (offset, block) in image.windows(spec.len).enumerate() {
        if block[..4] == spec.magic
            && word(block, 4)? == spec.version
            && word(block, 8)? == spec.len as u32
            && block[12..].iter().all(|&byte| byte == 0)
        {
            matches.push(offset);
        }
    }
    if matches.len() != 1 {
        return Err(format!(
            "expected one initialized {} block, found {}",
            spec.name,
            matches.len()
        ));
    }
    load.checked_add(matches[0] as u32)
        .ok_or_else(|| "gate block address overflowed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: usize = 80;

    fn spec() -> Spec {
        Spec::parse("TEST:2:80:TEST_GATE_STATE").unwrap()
    }

    fn sample(block_offsets: &[usize], declared_len: usize) -> Vec<u8> {
        let mut bytes = vec![0; HEADER_LEN + 0x300];
        bytes[..8].copy_from_slice(b"PS-X EXE");
        bytes[0x18..0x1c].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        bytes[0x1c..0x20].copy_from_slice(&(declared_len as u32).to_le_bytes());
        for &offset in block_offsets {
            let block = &mut bytes[HEADER_LEN + offset..HEADER_LEN + offset + LEN];
            block[..4].copy_from_slice(b"TEST");
            block[4..8].copy_from_slice(&2u32.to_le_bytes());
            block[8..12].copy_from_slice(&(LEN as u32).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn locates_the_block_at_the_load_address() {
        assert_eq!(locate(&sample(&[0x40], 0x300), &spec()), Ok(0x8001_0040));
    }

    #[test]
    fn rejects_an_ambiguous_initialiser() {
        assert_eq!(
            locate(&sample(&[0x40, 0x180], 0x300), &spec()),
            Err("expected one initialized TEST_GATE_STATE block, found 2".into())
        );
    }

    #[test]
    fn rejects_a_wrong_version_or_length() {
        let mut wrong_version = sample(&[0x40], 0x300);
        wrong_version[HEADER_LEN + 0x44] = 3;
        let mut wrong_length = sample(&[0x40], 0x300);
        wrong_length[HEADER_LEN + 0x48] = 16;
        for bytes in [wrong_version, wrong_length] {
            assert_eq!(
                locate(&bytes, &spec()),
                Err("expected one initialized TEST_GATE_STATE block, found 0".into())
            );
        }
    }

    #[test]
    fn rejects_a_truncated_payload_and_a_foreign_file() {
        let mut bytes = sample(&[0x40], 0x300);
        bytes.truncate(HEADER_LEN + 0x200);
        assert_eq!(
            locate(&bytes, &spec()),
            Err("PS-X EXE payload is shorter than its header says".into())
        );
        assert_eq!(
            locate(b"not a PS-X EXE", &spec()),
            Err("input is not a PS-X EXE".into())
        );
    }

    #[test]
    fn spec_text_is_checked() {
        assert!(Spec::parse("TOOLONG:1:80:X").is_err());
        assert!(Spec::parse("TEST:1:80").is_err());
        assert!(Spec::parse("TEST:x:80:N").is_err());
        assert_eq!(spec().len, 80);
    }
}
