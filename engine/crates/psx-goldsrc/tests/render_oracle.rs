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
    // Reference plane helpers the legacy unit tests call. They live here, not in
    // the shipped renderer, because only these tests use them.
    fn view_plane_distance(v:&SVert,plane:ViewPlane)->i32 {
        let view=FullView::new();
        let h=view.projection_h();
        match plane {
            ViewPlane::Near => v.z - NEAR_Z,
            ViewPlane::Left => h * v.x + (view.ofx() - view.vx0()) * v.z,
            ViewPlane::Right => (view.vx1() - view.ofx()) * v.z - h * v.x,
            ViewPlane::Top => h * v.y + (view.ofy() - view.vy0()) * v.z,
            ViewPlane::Bottom => (view.vy1() - view.ofy()) * v.z - h * v.y,
        }
    }
    // The crossing of an edge with a view plane as the generic clip computes it:
    // the lane kernel must reproduce it bit for bit from either end of the edge.
    fn lerp_view_plane(a:&SVert,b:&SVert,mut da:i32,mut db:i32,plane:ViewPlane)->SVert {
        let (a,b)=if (b.x,b.y,b.z)<(a.x,a.y,a.z) {
            core::mem::swap(&mut da,&mut db);
            (b,a)
        } else {
            (a,b)
        };
        let t=t_q12(da,da-db);
        let mut v=SVert {
            x:mix(a.x,b.x,t),
            y:mix(a.y,b.y,t),
            z:mix(a.z,b.z,t),
            rgb:(mix(a.rgb.0,b.rgb.0,t),mix(a.rgb.1,b.rgb.1,t),mix(a.rgb.2,b.rgb.2,t)),
            uv:(mix(a.uv.0,b.uv.0,t),mix(a.uv.1,b.uv.1,t)),
        };
        if matches!(plane,ViewPlane::Near) {
            v.z=NEAR_Z;
        }
        v
    }
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
