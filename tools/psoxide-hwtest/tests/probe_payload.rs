//! The audio probe payloads carry their CRC three times; all must agree.

use psoxide_hwtest::audio_report::probe_binary;
use psoxide_hwtest::util::{base64_encode, crc32};

fn payload(body: &[u8], suffix: Option<u32>) -> String {
    let crc = crc32(body);
    let mut binary = body.to_vec();
    binary.extend(crc.to_le_bytes());
    format!(
        "PA1/{}/C:{:08X}",
        base64_encode(&binary),
        suffix.unwrap_or(crc)
    )
}

#[test]
fn a_consistent_payload_decodes() {
    let mut body = b"PA1B".to_vec();
    body.extend([0u8; 12]);
    let (binary, crc) = probe_binary(&payload(&body, None), "PA1", 20).unwrap();
    assert_eq!(&binary[..4], b"PA1B");
    assert_eq!(crc, crc32(&binary[..binary.len() - 4]));
}

#[test]
fn length_and_crc_disagreements_are_rejected() {
    let mut good = b"PA1B".to_vec();
    good.extend([0u8; 12]);
    assert!(probe_binary(&payload(&good, None), "PA1", 24).is_err());
    assert!(probe_binary(&payload(&good, Some(0)), "PA1", 20).is_err());
}
