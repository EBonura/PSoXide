//! Frozen old numerical renderer versus the shared owner and both view
//! policies.

mod oracle_common;

use oracle_common::{cargo, here, read, root, Scratch};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const UNIT_TEST_PRELUDE: &str = "use super::*;
    use crate::new::{project_soft,close_inv_q12,set_projection_h,projection_h,visible_clip,guard_clip,on_visible_boundary,quad_outside_vertical};
    const OFY:i32=120;
    fn ofy()->i32{120}
    fn view_plane_distance(v:&SVert,p:ViewPlane)->i32 {super::view_plane_distance(&FullView::new(),v,p)}
";

#[test]
fn frozen_renderer_matches_the_shared_owner_for_both_games() {
    let here = here();
    let root = root();
    let provenance: Value =
        serde_json::from_str(&read(&here.join("oracles/RENDER-PROVENANCE.json"))).unwrap();
    let original = read(&here.join("oracles/legacy-render.rs"));
    assert_eq!(sha256(&original), provenance["hl_sha256"].as_str().unwrap());
    for game in ["hl", "cs"] {
        let mut old = original.clone();
        if game == "cs" {
            let mut lines: Vec<String> = old.split_inclusive('\n').map(String::from).collect();
            for delta in provenance["cs_line_delta"].as_array().unwrap().iter().rev() {
                let start = delta["start"].as_u64().unwrap() as usize;
                let end = delta["end"].as_u64().unwrap() as usize;
                let replacement: Vec<String> = delta["replacement"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line.as_str().unwrap().to_string())
                    .collect();
                lines.splice(start..end, replacement);
            }
            old = lines.concat();
        }
        assert_eq!(
            sha256(&old),
            provenance[format!("{game}_sha256")].as_str().unwrap()
        );
        let scratch = Scratch::new("render-oracle-");
        scratch.write("src/old.rs", &old);
        let adapter = read(&here.join(format!("support/{game}_render_adapter.rs")));
        let unit_tests = old
            .split_once("#[cfg(test)]")
            .expect("legacy renderer has unit tests")
            .1
            .replace("use super::*;", UNIT_TEST_PRELUDE)
            .replace(
                "perspective_screen_midpoint(a, b)",
                "perspective_screen_midpoint(&a, &b)",
            )
            .replace(
                "perspective_screen_midpoint(b, a)",
                "perspective_screen_midpoint(&b, &a)",
            );
        scratch.write("src/new.rs", &adapter);
        scratch.write(
            "src/shared.rs",
            &format!(
                "{}\n#[cfg(test)]{unit_tests}",
                read(&here.parent().unwrap().join("src/render.rs"))
            ),
        );
        let (select_view, view_rects) = if game == "cs" {
            (
                "old::set_view_rect(x,y,w,h);new::set_view_rect(x,y,w,h);",
                "[(0,0,320,240),(0,0,320,120),(0,120,320,120),(0,0,160,240),(160,0,160,240),(32,24,256,192),(0,0,320,240)]",
            )
        } else {
            ("assert_eq!((x,y,w,h),(0,0,320,240));", "[(0,0,320,240)]")
        };
        scratch.write(
            "src/main.rs",
            &read(&here.join("support/render_harness.rs"))
                .replace("SELECT_VIEW", select_view)
                .replace("VIEW_RECTS", view_rects),
        );
        scratch.write(
            "Cargo.toml",
            &format!(
                "[package]\nname=\"render-oracle\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n[dependencies]\npsx-engine={{path=\"{}\"}}\n",
                root.join("engine/crates/psx-engine").display()
            ),
        );
        for lane in [
            &["run", "--release", "--quiet"][..],
            &["test", "--release", "--quiet", "--", "--test-threads=1"][..],
        ] {
            println!("{game} {}", cargo(&scratch.0, lane));
        }
    }
}
