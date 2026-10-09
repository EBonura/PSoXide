//! Run frozen HL/CS HSFX against shared source with actual SDK register
//! lowering.
//!
//! Only MMIO read/write, device init, and ADPCM transfer endpoints are replaced
//! by recorders. The current SDK Voice/OneShot implementations compute every
//! register value and preserve write order. No retail assets or emulator are
//! required.
//!
//! Map-loop voice allocation is out of scope: the shared runtime deliberately
//! replaced the legacy round-robin with free-voice-first, quietest-eviction and
//! listener re-levelling (73ff8eaa, fe95bd46), so it no longer matches the
//! frozen originals. The hsfx.rs host tests pin that behaviour instead.

mod oracle_common;

use oracle_common::{cargo, here, read, root, skip_block, Scratch};
use regex::Regex;

/// Replace the body of `fn <name>(...)` (the text between its braces).
fn replace_body(source: &str, name: &str, body: &str) -> String {
    let signature = Regex::new(&format!(
        r"(?m)^(?:pub )?(?:unsafe )?fn {name}\([^\n]*\)[^{{]*\{{"
    ))
    .expect("signature regex");
    let m = signature
        .find(source)
        .unwrap_or_else(|| panic!("no fn {name}"));
    let end = skip_block(source, m.end());
    format!("{}{}{}", &source[..m.end()], body, &source[end - 1..])
}

fn hardware_imports(s: &str) -> String {
    s.replace("psx_spu::", "crate::spu::")
        .replace("psx_sfx::", "crate::sfx::")
}

const SNAPSHOT_IMPL: &str = r#"
impl<const N:usize,const H:u8,const S:u8> Hsfx<N,H,S> {
 pub fn snapshot(&self)->Vec<u64> {
  let mut v=Vec::new();
  for n in self.addrs {v.push(n as u64)} for n in self.rates {v.push(n as u64)}
  v.extend([self.count as u64,self.next_voice as u64,self.dialogue_base as u64]);
  for n in self.voice_addrs {v.push(n as u64)} for n in self.voice_rates {v.push(n as u64)}
  v.push(self.voice_count as u64);for n in self.ear {v.push(n as u64)} v
 }
}
"#;

const OLD_TAIL: &str = r#"
pub unsafe fn reset() { ADDRS=[0;MAX_SFX]; RATES=[0;MAX_SFX]; COUNT=0; NEXT_VOICE=0; DIALOGUE_BASE=0; VOICE_ADDRS=[0;MAX_VOICES];VOICE_RATES=[0;MAX_VOICES];VOICE_COUNT=0;MAP_LOOP_OWNER=[MAP_LOOP_OWNER_NONE;MAP_LOOP_VOICE_COUNT];NEXT_MAP_LOOP=0;EAR=[0;3]; }
pub unsafe fn snapshot()->Vec<u64>{let mut v=Vec::new();for n in ADDRS {v.push(n as u64)}for n in RATES{v.push(n as u64)}v.extend([COUNT as u64,NEXT_VOICE as u64,DIALOGUE_BASE as u64]);for n in VOICE_ADDRS{v.push(n as u64)}for n in VOICE_RATES{v.push(n as u64)}v.push(VOICE_COUNT as u64);for n in EAR{v.push(n as u64)}v}
"#;

#[test]
fn frozen_hsfx_matches_shared_source() {
    let here = here();
    let root = root();
    let scratch = Scratch::new("hsfx-oracle-");
    let deps = ["psx-asset", "psx-io"]
        .iter()
        .map(|name| {
            format!(
                "{name} = {{ path = \"{}/sdk/crates/{name}\" }}",
                root.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    // psx-spu names its register map through psx-hw, which lives in the
    // repository's shared crates directory.
    let deps = format!(
        "{deps}\npsx-hw = {{ path = \"{}/crates/psx-hw\" }}",
        root.display()
    );
    scratch.write(
        "Cargo.toml",
        &format!(
            "[package]\nname=\"hsfx-oracle\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n[dependencies]\n{deps}\n"
        ),
    );
    let inner_attributes = Regex::new(r"(?m)^#!.*\n").expect("static regex");
    let spu_path = root.join("sdk/crates/psx-spu/src/lib.rs");
    let spu = inner_attributes
        .replace_all(&read(&spu_path), "")
        .into_owned();
    let mut spu = spu.replace(
        "pub mod tones;",
        &format!(
            "#[path=\"{}/tones.rs\"] pub mod tones;",
            spu_path.parent().unwrap().display()
        ),
    );
    spu = replace_body(
        &spu,
        "write_reg16",
        "\ncrate::record(crate::Op::Write(addr,value));\n",
    );
    spu = replace_body(&spu, "read_reg16", "\nlet _ = addr; 0\n");
    spu = replace_body(&spu, "init", "\ncrate::record(crate::Op::Init);\n");
    spu = replace_body(
        &spu,
        "upload_adpcm",
        "\ncrate::record(crate::Op::Upload(dest.byte_offset(),bytes.to_vec()));\n",
    );
    // The shared source reaches the SPU through the `Spu` driver, whose reset
    // and upload bodies are these two private functions; the frozen originals
    // call the free functions patched above. Same recorders.
    spu = replace_body(&spu, "init_with", "\ncrate::record(crate::Op::Init);\n");
    spu = replace_body(
        &spu,
        "upload_adpcm_with",
        "\ncrate::record(crate::Op::Upload(dest.byte_offset(),bytes.to_vec()));\n",
    );
    // A driver without the reset, for the call that used to find the SPU
    // already initialised (the dialogue loader).
    spu += "\nimpl Spu { pub fn bare() -> Self { Self(unsafe { SpuDma::steal() }) } }\n";
    scratch.write("src/spu.rs", &spu);

    let sfx_path = root.join("sdk/crates/psx-sfx/src/lib.rs");
    let sfx = inner_attributes
        .replace_all(&read(&sfx_path), "")
        .into_owned();
    scratch.write("src/sfx.rs", &hardware_imports(&sfx));

    let shared_path = here.parent().unwrap().join("src/hsfx.rs");
    let shared = hardware_imports(&read(&shared_path)) + SNAPSHOT_IMPL;
    scratch.write("src/shared.rs", &shared);

    let function =
        Regex::new(r"(?ms)^pub unsafe fn (\w+)\((.*?)\)([^\{]*)\{").expect("static regex");
    let max_sfx = Regex::new(r"const MAX_SFX: usize = (\d+)").expect("static regex");
    for (short, game) in [("hl", "hl-psx"), ("cs", "cs-psx")] {
        let old = hardware_imports(&format!(
            "{}{}",
            read(&here.join(format!("oracles/{game}-ids.rs"))),
            read(&here.join("oracles/legacy-hsfx-runtime.rs"))
        )) + OLD_TAIL;
        scratch.write(&format!("src/old_{short}.rs"), &old);
        let n = &max_sfx.captures(&old).expect("MAX_SFX")[1];
        let ids: Vec<String> = ["CHARGER_HEALTH_LOOP", "CHARGER_HEV_LOOP"]
            .iter()
            .map(|key| {
                Regex::new(&format!(r"pub const {key}: u8 = (\d+)"))
                    .expect("id regex")
                    .captures(&old)
                    .unwrap_or_else(|| panic!("no {key}"))[1]
                    .to_string()
            })
            .collect();
        let mut adapter = format!(
            "static mut STATE:crate::shared::Hsfx<{n},{},{}>=crate::shared::Hsfx::new();\npub unsafe fn reset() {{STATE=crate::shared::Hsfx::new();}}\npub unsafe fn snapshot()->Vec<u64>{{STATE.snapshot()}}\n",
            ids[0], ids[1]
        );
        for m in function.captures_iter(&old) {
            let (name, formals, ret) = (&m[1], &m[2], &m[3]);
            if name == "reset" || name == "snapshot" {
                continue;
            }
            let names = formals
                .split(',')
                .filter(|x| !x.trim().is_empty())
                .map(|x| x.split(':').next().unwrap().trim())
                .collect::<Vec<_>>()
                .join(", ");
            // The shared bank takes the driver where the originals reached for
            // the free functions. `init_from_pack` used to reset the SPU first:
            // `Spu::new` does that, immediately before the call.
            adapter += &match name {
                "init_from_pack" => format!(
                    "pub unsafe fn {name}({formals}){ret}{{let mut spu=crate::spu::Spu::new(psx_io::periph::SpuDma::steal());STATE.{name}(&mut spu, {names})}}\n"
                ),
                "load_dialogue_pack" => format!(
                    "pub unsafe fn {name}({formals}){ret}{{let mut spu=crate::spu::Spu::bare();STATE.{name}(&mut spu, {names})}}\n"
                ),
                _ => format!("pub unsafe fn {name}({formals}){ret}{{STATE.{name}({names})}}\n"),
            };
        }
        scratch.write(&format!("src/new_{short}.rs"), &adapter);
    }
    scratch.write("src/main.rs", &read(&here.join("support/hsfx_harness.rs")));
    println!(
        "{}",
        cargo(
            &scratch.0,
            &[
                "run",
                "--release",
                "--quiet",
                "--manifest-path",
                "Cargo.toml"
            ]
        )
    );
}
