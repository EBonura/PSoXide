//! G3: the check a disc builder runs on an executable before it writes it into an image.
//!
//! The receipt `psoxide-pgo apply` writes into the PSX-EXE header says the architecture gate
//! passed that exact image. An executable without one may be pressed only when
//! `sdk/profile.toml` lists its program under `no-receipt` (a program whose build driver does
//! not run `apply` yet; the list ends with W9).
//!
//! Identifying a program that has no receipt: the executable carries no name, so the caller
//! passes the one it knows (`mkisopsx` uses the executable's file stem). A determined bypass is
//! to rename the executable to a listed name; or to forge a receipt, which anyone who can run
//! `psx_iso::receipt::write` can. This stops a program built the wrong way by accident, and any
//! change to a gated image after the gate ran; it does not stop sabotage by someone with the
//! SDK in hand.

use crate::profile::Profile;
use psx_iso::receipt::{self, Kind, ReceiptError};

/// What the press may do with an executable.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// A matching shipping receipt.
    Receipt(String),
    /// A matching diag receipt and the caller allowed diag images.
    Diag(String),
    /// No receipt, and the profile lists the program as not yet on `apply`.
    Listed(String),
}

/// Judge `exe` (a PSX-EXE) called `label` against the SDK's profile and the program's own file
/// (`None` when the recipe passes none).
///
/// # Errors
/// The text of the refusal.
pub fn check(
    exe: &[u8],
    label: &str,
    allow_diag: bool,
    profile: &Profile,
    own: Option<&Profile>,
) -> Result<Verdict, String> {
    // A `no-receipt` entry of the SDK's profile names the program; one in the program's own file
    // belongs to the program whose recipe passed the file.
    let listed = profile
        .exceptions
        .iter()
        .find(|e| e.kind == "no-receipt" && e.program == label)
        .or_else(|| own.and_then(|own| own.no_receipt(label)));
    match receipt::verify(exe) {
        Ok(found) => {
            if let Some(e) = listed {
                return Err(format!(
                    "psx-gate: {label}: a no-receipt entry ({}) lists this program but \
                     this executable has a receipt: delete the entry",
                    e.remove
                ));
            }
            match found.kind {
                Kind::Diag if allow_diag => Ok(Verdict::Diag(found.program().to_string())),
                Kind::Diag => Err(format!(
                    "psx-gate: {label}: built with a diagnostic feature of an SDK crate \
                     (psx-rt/host-tty and the other [diag] entries of sdk/profile.toml); STANDARD.md \
                     rule 2. A diagnostic image does not go on a disc: rebuild without it, or pass \
                     --diag for a development disc"
                )),
                _ => Ok(Verdict::Receipt(found.program().to_string())),
            }
        }
        Err(ReceiptError::Missing) => match listed {
            Some(_) => Ok(Verdict::Listed(label.to_string())),
            None => Err(format!(
                "psx-gate: {label}: no gate receipt, so this executable did not come through \
                 `psoxide-pgo apply` (the one build step that runs the architecture gate); \
                 STANDARD.md rules 2 and 3. Build it with `psoxide-pgo apply`"
            )),
        },
        Err(ReceiptError::Mismatch) => Err(format!(
            "psx-gate: {label}: the gate receipt does not match this executable: it was changed \
             after `psoxide-pgo apply` ran. Rebuild it with `psoxide-pgo apply`"
        )),
        Err(_) => Err(format!("psx-gate: {label}: not a PSX-EXE")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn exe() -> Vec<u8> {
        let mut v = vec![0u8; 2048 + 2048];
        v[..8].copy_from_slice(b"PS-X EXE");
        v[0x1c..0x20].copy_from_slice(&2048u32.to_le_bytes());
        v
    }

    fn profile(listed: &[&str]) -> Profile {
        let mut text = String::new();
        for name in listed {
            text += &format!(
                "[[exception]]\nprogram = \"{name}\"\nkind = \"no-receipt\"\nitem = \"-\"\nremove = \"W9\"\n"
            );
        }
        Profile::parse(&text, Path::new("p")).unwrap()
    }

    #[test]
    fn passing_and_failing_fixtures() {
        let mut gated = exe();
        receipt::write(&mut gated, "game", Kind::Shipping).unwrap();
        assert_eq!(
            check(&gated, "game", false, &profile(&[]), None),
            Ok(Verdict::Receipt("game".into()))
        );
        // Changed after the gate.
        let mut patched = gated.clone();
        patched[3000] ^= 1;
        assert!(check(&patched, "game", false, &profile(&[]), None)
            .unwrap_err()
            .contains("changed"));
        // Never gated.
        let plain = exe();
        assert!(check(&plain, "game", false, &profile(&[]), None)
            .unwrap_err()
            .contains("no gate receipt"));
        assert_eq!(
            check(&plain, "game", false, &profile(&["game"]), None),
            Ok(Verdict::Listed("game".into()))
        );
        // A listed program that now has a receipt: the entry is stale.
        assert!(check(&gated, "game", false, &profile(&["game"]), None)
            .unwrap_err()
            .contains("delete the entry"));
        // Listed in the program's own file instead of the SDK's.
        let own = Profile::parse_program_file(
            "[[exception]]\nkind = \"no-receipt\"\nitem = \"-\"\nremove = \"W9\"\n",
            Path::new("psx-gate.toml"),
        )
        .unwrap();
        assert_eq!(
            check(&plain, "game", false, &profile(&[]), Some(&own)),
            Ok(Verdict::Listed("game".into()))
        );
        assert!(check(&gated, "game", false, &profile(&[]), Some(&own))
            .unwrap_err()
            .contains("delete the entry"));
        // Diag.
        let mut tap = exe();
        receipt::write(&mut tap, "game", Kind::Diag).unwrap();
        assert!(check(&tap, "game", false, &profile(&[]), None)
            .unwrap_err()
            .contains("diagnostic"));
        assert_eq!(
            check(&tap, "game", true, &profile(&[]), None),
            Ok(Verdict::Diag("game".into()))
        );
    }
}
