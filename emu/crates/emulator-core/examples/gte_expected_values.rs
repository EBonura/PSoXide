//! Compute the exact GTE outputs the hardware-tests conformance battery
//! must expect, using the SAME software GTE the emulator runs
//! (`psx_gte_core::Gte`). Replays the scene-captured MVMVA/RTPT/NCLIP ops
//! (inputs from `probe_gameplay_gte_ops`) and a set of corner cases, then
//! prints bakeable hex so the disc tests and the psx-gte-core host mirror
//! carry verified expecteds instead of guessed ones.
//!
//! Usage: cargo run -p emulator-core --example gte_expected_values --release

use psx_gte_core::Gte;

fn tri(a: u32, b: u32, c: u32) -> u32 {
    a ^ b.rotate_left(11) ^ c.rotate_left(22)
}

/// Rotation (RT) + translation (TR) + projection (OFX/OFY/H) shared across
/// the captured gameplay frame. TR is nonzero for world geometry (MVMVA /
/// RTPT), unlike the RTPS samples whose TR was zero.
fn seed_xform(g: &mut Gte) {
    g.write_control(31, 0);
    g.write_control(0, 0x0000_0f19); // R11,R12
    g.write_control(1, 0x016e_fab4); // R13,R21
    g.write_control(2, 0x0411_f098); // R22,R23
    g.write_control(3, 0xfbb1_fae7); // R31,R32
    g.write_control(4, 0xffff_f177); // R33
    g.write_control(5, 0xffff_eabc); // TRX
    g.write_control(6, 0xffff_fdb9); // TRY
    g.write_control(7, 0x0000_35be); // TRZ
    g.write_control(24, 0x00a0_0000); // OFX
    g.write_control(25, 0x0078_0000); // OFY
    g.write_control(26, 0x0000_0140); // H
}

fn mvmva(vxy0: u32, vz0: u32) -> Gte {
    let mut g = Gte::new();
    seed_xform(&mut g);
    g.write_data(0, vxy0);
    g.write_data(1, vz0);
    g.execute(0x4a08_0012); // MVMVA mx=RT vx=V0 cv=TR sf=1 lm=0
    g
}

fn rtpt(v: [u32; 6]) -> Gte {
    let mut g = Gte::new();
    seed_xform(&mut g);
    for (i, &w) in v.iter().enumerate() {
        g.write_data(i as u8, w);
    }
    g.execute(0x4a08_0030); // RTPT
    g
}

fn nclip(sxy0: u32, sxy1: u32, sxy2: u32) -> u32 {
    let mut g = Gte::new();
    g.write_control(31, 0);
    g.write_data(12, sxy0);
    g.write_data(13, sxy1);
    g.write_data(14, sxy2);
    g.execute(0x4a00_0006); // NCLIP
    g.read_data(24) // MAC0
}

fn lzcr(value: u32) -> u32 {
    let mut g = Gte::new();
    g.write_data(30, value); // LZCS
    g.read_data(31) // LZCR
}

fn main() {
    println!("// ---- scene MVMVA (RT*V0 + TR, sf=1) : MAC1/2/3 + digest ----");
    for (tag, vxy0, vz0) in [
        ('A', 0x2040_0340u32, 0x0000_09c0u32),
        ('B', 0x2040_0340, 0x0000_16c0),
        ('C', 0x0b00_0340, 0x0000_23c0),
        ('D', 0x2040_09c0, 0x0000_09c0),
    ] {
        let g = mvmva(vxy0, vz0);
        let (m1, m2, m3) = (g.read_data(25), g.read_data(26), g.read_data(27));
        println!(
            "MVMVA {tag}: in(0x{vxy0:08x},0x{vz0:08x}) MAC=0x{m1:08x},0x{m2:08x},0x{m3:08x} digest=0x{:08x} flag=0x{:08x}",
            tri(m1, m2, m3),
            g.read_control(31)
        );
    }

    println!("\n// ---- scene RTPT : SXY0/1/2 + digest + FLAG + SZ3 ----");
    for (tag, v) in [
        (
            'A',
            [
                0x0480_2d80u32,
                0x0000_2700,
                0x0480_2080,
                0x0000_2d80,
                0x0480_1a00,
                0x0000_2d80,
            ],
        ),
        (
            'B',
            [
                0x0480_2700,
                0x0000_2d80,
                0x0480_2d80,
                0x0000_2d80,
                0x0480_2080,
                0x0000_3400,
            ],
        ),
        (
            'C',
            [
                0x0480_1a00,
                0x0000_3400,
                0x0480_2700,
                0x0000_3400,
                0x0480_2d80,
                0x0000_3400,
            ],
        ),
        (
            'D',
            [
                0x0680_3400,
                0x0000_2d80,
                0x0480_3400,
                0x0000_3400,
                0x0680_3a80,
                0x0000_2700,
            ],
        ),
        (
            'E',
            [
                0x10c0_0000,
                0x0000_0d00,
                0x10c0_0680,
                0x0000_0d00,
                0x1dc0_0680,
                0x0000_0d00,
            ],
        ),
        (
            'F',
            [
                0x1dc0_0000,
                0x0000_0d00,
                0x10c0_0680,
                0x0000_1380,
                0x10c0_0000,
                0x0000_1380,
            ],
        ),
    ] {
        let g = rtpt(v);
        let (s0, s1, s2) = (g.read_data(12), g.read_data(13), g.read_data(14));
        println!(
            "RTPT {tag}: SXY=0x{s0:08x},0x{s1:08x},0x{s2:08x} digest=0x{:08x} flag=0x{:08x} sz3=0x{:08x}",
            tri(s0, s1, s2),
            g.read_control(31),
            g.read_data(19)
        );
    }

    println!("\n// ---- scene NCLIP (real backface tests) : MAC0 ----");
    for (tag, s0, s1, s2) in [
        ('A', 0x006e_0095u32, 0xffe2_0094u32, 0xffde_00dcu32),
        ('B', 0x0073_00d5, 0xffde_00dc, 0xffd8_0130),
        ('C', 0x0079_011f, 0xffd8_0130, 0xffd2_0194),
    ] {
        println!(
            "NCLIP {tag}: in 0x{s0:08x},0x{s1:08x},0x{s2:08x} MAC0=0x{:08x}",
            nclip(s0, s1, s2)
        );
    }

    println!("\n// ---- LZCS/LZCR leading-bit count ----");
    for v in [
        0x00ff_ffffu32,
        0xffff_0000,
        0x0000_0001,
        0x7fff_ffff,
        0x8000_0000,
    ] {
        println!("LZCS 0x{v:08x} -> LZCR={}", lzcr(v));
    }

    println!("\n// ---- MVMVA buggy FC mode (cv=2) ----");
    {
        let mut g = Gte::new();
        seed_xform(&mut g);
        g.write_control(21, 0x0000_1000); // FCX
        g.write_control(22, 0x0000_2000); // FCY
        g.write_control(23, 0x0000_3000); // FCZ
        g.write_data(0, 0x2040_0340);
        g.write_data(1, 0x0000_09c0);
        g.execute(0x4a08_4012); // MVMVA mx=RT vx=V0 cv=FC sf=1 lm=0 (bugged)
        let (m1, m2, m3) = (g.read_data(25), g.read_data(26), g.read_data(27));
        println!(
            "MVMVA-FC: MAC1=0x{m1:08x} MAC2=0x{m2:08x} MAC3=0x{m3:08x} digest=0x{:08x} flag=0x{:08x}",
            tri(m1, m2, m3),
            g.read_control(31)
        );
    }

    println!("\n// ---- SQR (IR^2, sf=1) ----");
    {
        let mut g = Gte::new();
        g.write_control(31, 0);
        g.write_data(9, 0x0000_1234); // IR1
        g.write_data(10, 0x0000_f8ee); // IR2 (negative i16)
        g.write_data(11, 0x0000_0567); // IR3
        g.execute(0x4a08_0028); // SQR
        let (m1, m2, m3) = (g.read_data(25), g.read_data(26), g.read_data(27));
        println!(
            "SQR: MAC=0x{m1:08x},0x{m2:08x},0x{m3:08x} digest=0x{:08x}",
            tri(m1, m2, m3)
        );
    }

    println!("\n// ---- OP (cross product, sf=1) ----");
    {
        let mut g = Gte::new();
        g.write_control(31, 0);
        g.write_control(0, 0x0000_1000); // R11 (D1)
        g.write_control(2, 0x0000_2000); // R22 (D2)
        g.write_control(4, 0x0000_3000); // R33 (D3)
        g.write_data(9, 0x0000_0400); // IR1
        g.write_data(10, 0x0000_0500); // IR2
        g.write_data(11, 0x0000_0600); // IR3
        g.execute(0x4a08_000c); // OP
        let (m1, m2, m3) = (g.read_data(25), g.read_data(26), g.read_data(27));
        println!(
            "OP: MAC1=0x{m1:08x} MAC2=0x{m2:08x} MAC3=0x{m3:08x} digest=0x{:08x}",
            tri(m1, m2, m3)
        );
    }

    println!("\n// ---- CANDIDATE FIXES vs hardware (FC photo=0x9D9B64C, OP photo=0xBFF0043, high-7 nibbles) ----");
    {
        // RT matrix decoded from c0..c4 (1.3.12 i16 cells), V0 from the FC test.
        let rt: [[i64; 3]; 3] = [
            [0x0f19, 0x0000, 0xfab4u16 as i16 as i64], // R11,R12,R13
            [0x016e, 0xf098u16 as i16 as i64, 0x0411], // R21,R22,R23
            [
                0xfae7u16 as i16 as i64,
                0xfbb1u16 as i16 as i64,
                0xf177u16 as i16 as i64,
            ], // R31,R32,R33
        ];
        let v = [0x0340i64, 0x2040, 0x09c0]; // Vx,Vy,Vz from VXY0=0x20400340, VZ0=0x09c0
                                             // psx-spx FC bug: drop translation AND first matrix column -> cols 1,2 only.
        let mac: [i32; 3] = [
            ((rt[0][1] * v[1] + rt[0][2] * v[2]) >> 12) as i32,
            ((rt[1][1] * v[1] + rt[1][2] * v[2]) >> 12) as i32,
            ((rt[2][1] * v[1] + rt[2][2] * v[2]) >> 12) as i32,
        ];
        println!(
            "MVMVA-FC candidate (drop col0+TR): MAC=0x{:08x},0x{:08x},0x{:08x} digest=0x{:08x}",
            mac[0],
            mac[1],
            mac[2],
            tri(mac[0] as u32, mac[1] as u32, mac[2] as u32)
        );
    }
    {
        // OP with IR feedback: each component reuses the just-written IR.
        let (d1, d2, d3) = (0x1000i64, 0x2000i64, 0x3000i64);
        let (mut ir2, ir3) = (0x500i64, 0x600i64);
        let mac1 = ((ir3 * d2 - ir2 * d3) >> 12) as i32;
        let ir1 = mac1 as i64;
        let mac2 = ((ir1 * d3 - ir3 * d1) >> 12) as i32;
        ir2 = mac2 as i64;
        let mac3 = ((ir2 * d1 - ir1 * d2) >> 12) as i32;
        let _ = (ir2, ir3);
        println!(
            "OP candidate (IR feedback):        MAC=0x{:08x},0x{:08x},0x{:08x} digest=0x{:08x}",
            mac1,
            mac2,
            mac3,
            tri(mac1 as u32, mac2 as u32, mac3 as u32)
        );
    }

    println!("\n// ---- latency-probe vertices A/B (must differ in SXY2/SZ3/IR1/IR0) ----");
    {
        let proj = |vxy0: u32, vz0: u32| {
            let mut g = Gte::new();
            g.write_control(31, 0);
            g.write_control(0, 0x0000_1000); // identity R11,R12
            g.write_control(1, 0x0000_0000); // R13,R21
            g.write_control(2, 0x0000_1000); // R22,R23
            g.write_control(3, 0x0000_0000); // R31,R32
            g.write_control(4, 0x0000_1000); // R33
            g.write_control(5, 0);
            g.write_control(6, 0);
            g.write_control(7, 0x0000_1000); // TRZ
            g.write_control(24, 0x00a0_0000); // OFX 160
            g.write_control(25, 0x0078_0000); // OFY 120
            g.write_control(26, 0x0000_0100); // H 256
            g.write_control(27, 0x0000_0100); // DQA
            g.write_control(28, 0);
            g.write_data(0, vxy0);
            g.write_data(1, vz0);
            g.execute(0x4a08_0001); // RTPS sf=1
            (
                g.read_data(14), // SXY2
                g.read_data(19), // SZ3
                g.read_data(9),  // IR1
                g.read_data(8),  // IR0
                g.read_control(31),
            )
        };
        let a = proj(0x0080_0100, 0x0000_0064); // (256,128,100)
        let b = proj(0xffc0_ff38, 0xffff_ff38); // (-200,-64,-200)
        println!(
            "A SXY2=0x{:08x} SZ3=0x{:08x} IR1=0x{:08x} IR0=0x{:08x} flag=0x{:08x}",
            a.0, a.1, a.2, a.3, a.4
        );
        println!(
            "B SXY2=0x{:08x} SZ3=0x{:08x} IR1=0x{:08x} IR0=0x{:08x} flag=0x{:08x}",
            b.0, b.1, b.2, b.3, b.4
        );
        println!(
            "distinct: SXY2={} SZ3={} IR1={} IR0={}",
            a.0 != b.0,
            a.1 != b.1,
            a.2 != b.2,
            a.3 != b.3
        );
    }

    println!("\n// ---- AVSZ3 (Z average -> OTZ) ----");
    {
        let mut g = Gte::new();
        g.write_control(31, 0);
        g.write_control(29, 0x0000_0155); // ZSF3 (~1/3)
        g.write_data(17, 0x0000_1000); // SZ1
        g.write_data(18, 0x0000_2000); // SZ2
        g.write_data(19, 0x0000_3000); // SZ3
        g.execute(0x4a00_002d); // AVSZ3
        println!(
            "AVSZ3: OTZ=0x{:08x} MAC0=0x{:08x}",
            g.read_data(7),
            g.read_data(24)
        );
    }
}
