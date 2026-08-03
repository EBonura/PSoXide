//! Replay the player-skinning live capture (cortex_explosion_probe overlay
//! pages) through the console-exact GTE core, stage by stage. Feed it a text
//! file transcribed verbatim from the photographed overlay pages; it
//! recomputes each stage from the captured INPUTS and diffs against the
//! captured OUTPUTS. The FIRST stage where the hardware photo diverges from
//! the recomputation is the bug locus:
//!
//!   compose:  R0 =? clamp16(MVMVA(RT=VI, TR=0, V=col_j(M0)))  per column
//!   skin A:   VA =? MVMVA(RT=R0, TR=T0, V=pos)    (0x4A080012, sf=1 lm=0)
//!   skin B:   VB =? MVMVA(RT=R1, TR=T1, V=pos)
//!   lerp:     VL =? (VA*(256-W) + VB*W) >> 8      (CPU, engine formula)
//!
//! Capture file = the overlay lines, typed as photographed (page 2/3/4):
//!
//!   V 0229 F1F2 CFA5 J18/19 W18
//!   VA FFFFFED0 00000180 00000047
//!   VB FFFFFEBD 00000183 00000D30
//!   VL FFFFFECE 00000180 00000D45
//!   SXY 0083 009C FLG 00000000
//!   R00 0211 FFF6 0055
//!   R01 FFC9 FE3D 011F
//!   R02 0044 FEDC FE43
//!   T0 FFFFFF81 0000035F 000006FD
//!   R10..R12, T1 likewise
//!   VI0..VI2, M00..M02, P0, M10..M12, P1 likewise
//!
//! Usage: gte_skin_replay <capture.txt>

use psx_gte_core::Gte;

const MVMVA_RT_V0_TR_SF1: u32 = 0x4A08_0012;

#[derive(Default, Clone, Copy)]
struct Capture {
    vi: [[i16; 3]; 3],
    m0: [[i16; 3]; 3],
    m1: [[i16; 3]; 3],
    r0: [[i16; 3]; 3],
    r1: [[i16; 3]; 3],
    t0: [i32; 3],
    t1: [i32; 3],
    pos: [i16; 3],
    blend: i32,
    va: [i32; 3],
    vb: [i32; 3],
    vl: [i32; 3],
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: gte_skin_replay <capture.txt>");
    let text = std::fs::read_to_string(&path).expect("capture file readable");
    let c = parse(&text);

    let mut all_ok = true;

    // Stage 1: GTE compose (column-by-column MVMVA with VI as the rotation).
    let r0_calc = compose(&c.vi, &c.m0);
    all_ok &= report_mat("compose R0 = VI x M0", &r0_calc, &c.r0);
    let r1_calc = compose(&c.vi, &c.m1);
    all_ok &= report_mat("compose R1 = VI x M1", &r1_calc, &c.r1);

    // Stage 2: per-vertex skin MVMVA with the matrices the engine CTC2'd
    // (the CAPTURED R/T, not the recomputed ones, so a compose divergence
    // doesn't cascade into this stage's verdict).
    let va_calc = mvmva(&c.r0, &c.t0, &c.pos);
    all_ok &= report_vec("skin VA = R0*pos + T0", &va_calc, &c.va);
    let vb_calc = mvmva(&c.r1, &c.t1, &c.pos);
    all_ok &= report_vec("skin VB = R1*pos + T1", &vb_calc, &c.vb);

    // Stage 3: CPU lerp (engine formula), from the CAPTURED VA/VB.
    let t = c.blend;
    let inv = 256 - t;
    let vl_calc = [
        ((c.va[0] * inv) + (c.vb[0] * t)) >> 8,
        ((c.va[1] * inv) + (c.vb[1] * t)) >> 8,
        ((c.va[2] * inv) + (c.vb[2] * t)) >> 8,
    ];
    all_ok &= report_vec("lerp VL", &vl_calc, &c.vl);

    println!();
    if all_ok {
        println!("ALL STAGES CONSISTENT with the console-exact GTE core");
    } else {
        println!("DIVERGENT STAGE(S) FOUND -- the first mismatch above is the bug locus");
    }
}

/// One engine compose: RT=vi, TR=0, transform each column of `m`, clamp to
/// i16 (mirrors render3d::gte_compose_joint_rotation exactly).
fn compose(vi: &[[i16; 3]; 3], m: &[[i16; 3]; 3]) -> [[i16; 3]; 3] {
    let mut out = [[0i16; 3]; 3];
    for col in 0..3 {
        let v = [m[0][col], m[1][col], m[2][col]];
        let c = mvmva(vi, &[0, 0, 0], &v);
        out[0][col] = clamp_i16(c[0]);
        out[1][col] = clamp_i16(c[1]);
        out[2][col] = clamp_i16(c[2]);
    }
    out
}

fn clamp_i16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// MVMVA(mx=RT, vx=V0, cv=TR, sf=1, lm=0) through psx_gte_core, with the
/// exact register write path scene::transform_vertex uses.
fn mvmva(rt: &[[i16; 3]; 3], tr: &[i32; 3], v: &[i16; 3]) -> [i32; 3] {
    let mut gte = Gte::new();
    let pack = |a: i16, b: i16| (a as u16 as u32) | ((b as u16 as u32) << 16);
    gte.write_control(0, pack(rt[0][0], rt[0][1]));
    gte.write_control(1, pack(rt[0][2], rt[1][0]));
    gte.write_control(2, pack(rt[1][1], rt[1][2]));
    gte.write_control(3, pack(rt[2][0], rt[2][1]));
    gte.write_control(4, rt[2][2] as i32 as u32);
    gte.write_control(5, tr[0] as u32);
    gte.write_control(6, tr[1] as u32);
    gte.write_control(7, tr[2] as u32);
    gte.write_data(0, pack(v[0], v[1]));
    gte.write_data(1, v[2] as i32 as u32);
    gte.execute(MVMVA_RT_V0_TR_SF1);
    [
        gte.read_data(25) as i32,
        gte.read_data(26) as i32,
        gte.read_data(27) as i32,
    ]
}

fn report_vec(name: &str, calc: &[i32; 3], cap: &[i32; 3]) -> bool {
    let ok = calc == cap;
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        println!("   calc {:08X} {:08X} {:08X}", calc[0], calc[1], calc[2]);
        println!("   capt {:08X} {:08X} {:08X}", cap[0], cap[1], cap[2]);
    }
    ok
}

fn report_mat(name: &str, calc: &[[i16; 3]; 3], cap: &[[i16; 3]; 3]) -> bool {
    let ok = calc == cap;
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        for row in 0..3 {
            println!(
                "   row{row} calc {:04X} {:04X} {:04X}  capt {:04X} {:04X} {:04X}",
                calc[row][0] as u16,
                calc[row][1] as u16,
                calc[row][2] as u16,
                cap[row][0] as u16,
                cap[row][1] as u16,
                cap[row][2] as u16
            );
        }
    }
    ok
}

fn parse(text: &str) -> Capture {
    let mut c = Capture::default();
    for line in text.lines() {
        let mut tok = line.split_whitespace();
        let Some(label) = tok.next() else { continue };
        let rest: Vec<&str> = tok.collect();
        let h16 = |s: &str| {
            i16::from_str_radix(s, 16)
                .ok()
                .or_else(|| u16::from_str_radix(s, 16).ok().map(|v| v as i16))
        };
        let h32 = |s: &str| u32::from_str_radix(s, 16).ok().map(|v| v as i32);
        let row16 = |out: &mut [i16; 3], rest: &[&str]| {
            for (i, slot) in out.iter_mut().enumerate() {
                if let Some(v) = rest.get(i).and_then(|s| h16(s)) {
                    *slot = v;
                }
            }
        };
        let row32 = |out: &mut [i32; 3], rest: &[&str]| {
            for (i, slot) in out.iter_mut().enumerate() {
                if let Some(v) = rest.get(i).and_then(|s| h32(s)) {
                    *slot = v;
                }
            }
        };
        match label {
            "VI0" => row16(&mut c.vi[0], &rest),
            "VI1" => row16(&mut c.vi[1], &rest),
            "VI2" => row16(&mut c.vi[2], &rest),
            "M00" => row16(&mut c.m0[0], &rest),
            "M01" => row16(&mut c.m0[1], &rest),
            "M02" => row16(&mut c.m0[2], &rest),
            "M10" => row16(&mut c.m1[0], &rest),
            "M11" => row16(&mut c.m1[1], &rest),
            "M12" => row16(&mut c.m1[2], &rest),
            "R00" => row16(&mut c.r0[0], &rest),
            "R01" => row16(&mut c.r0[1], &rest),
            "R02" => row16(&mut c.r0[2], &rest),
            "R10" => row16(&mut c.r1[0], &rest),
            "R11" => row16(&mut c.r1[1], &rest),
            "R12" => row16(&mut c.r1[2], &rest),
            "T0" => row32(&mut c.t0, &rest),
            "T1" => row32(&mut c.t1, &rest),
            "VA" => row32(&mut c.va, &rest),
            "VB" => row32(&mut c.vb, &rest),
            "VL" => row32(&mut c.vl, &rest),
            "V" => {
                row16(&mut c.pos, &rest);
                for t in &rest {
                    if let Some(w) = t.strip_prefix('W') {
                        c.blend = w.parse().unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }
    c
}
