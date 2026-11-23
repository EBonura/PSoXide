use super::*;
use crate::{build_world_pack, load_boot_exe_from_disc, Disc, EXE_HEADER_BYTES};
use psx_scene_abi::{
    compact_bytes, decode_header_checked, table_image, table_scene, SCENE_FLAG_PARTNER_CREDIT,
};

fn exe(t_addr: u32, pc: u32, sp: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; EXE_HEADER_BYTES];
    out[..8].copy_from_slice(b"PS-X EXE");
    out[0x10..0x14].copy_from_slice(&pc.to_le_bytes());
    out[0x18..0x1c].copy_from_slice(&t_addr.to_le_bytes());
    out[0x1c..0x20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    out[0x30..0x34].copy_from_slice(&sp.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn payload(sectors: usize, seed: u8) -> Vec<u8> {
    (0..sectors * SECTOR_BYTES)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect()
}

const BASE: u32 = 0x8001_0000;

fn small_exe() -> Vec<u8> {
    exe(BASE, BASE, 0, &payload(1, 0))
}

fn boot_plan() -> ScenePlan {
    ScenePlan {
        images: vec![ImageSpec::program(BOOT_EXE_NAME, None)],
        scenes: vec![SceneSpec::start("BOOT", BOOT_EXE_NAME)],
        core_image: None,
    }
}

fn press(
    boot_exe: Vec<u8>,
    plan: &ScenePlan,
    world: bool,
) -> Result<(Disc, SceneDisc), SceneDiscError> {
    let mut builder = IsoBuilder::new();
    let world = world.then(|| build_world_pack(&[(0, b"room".as_slice())]));
    let ui = world
        .as_ref()
        .map(|_| build_world_pack(&[(0, b"ui".as_slice())]));
    let pressed = add_scene_disc_files(&mut builder, boot_exe, world, ui, None, plan)?;
    Ok((Disc::from_bin(builder.build_bin()), pressed))
}

fn refuse(plan: &ScenePlan) -> SceneDiscError {
    press(small_exe(), plan, false).err().unwrap()
}

#[test]
fn the_table_sits_at_22_the_boot_exe_at_23_and_images_follow_it() {
    let boot_payload = payload(3, 1);
    let game_payload = payload(5, 2);
    let mut plan = boot_plan();
    plan.images.push(ImageSpec::program(
        "game.exe",
        Some(exe(BASE, BASE + 0x40, 0x801f_d700, &game_payload)),
    ));
    let mut game = SceneSpec::start("GAME", "GAME.EXE");
    game.partner_credit = 3;
    game.flags = SCENE_FLAG_PARTNER_CREDIT;
    plan.scenes.push(game);
    // A scene inside the boot exe: no image row of its own.
    plan.scenes
        .push(SceneSpec::at("MENU", BOOT_EXE_NAME, BASE + 0x100));
    let (disc, pressed) = press(
        exe(BASE, BASE + 0x10, 0x801f_d700, &boot_payload),
        &plan,
        true,
    )
    .unwrap();

    // The BIOS path is unchanged: SYSTEM.CNF boots PSX.EXE, now at 23.
    let booted = load_boot_exe_from_disc(&disc).expect("boots");
    assert_eq!(booted.exe.payload, boot_payload);
    assert_eq!(disc.find_file("SYSTEM.CNF").unwrap().lba, 21);
    // The BIOS takes the stack from SYSTEM.CNF: it starts at the core window's base.
    let cnf = String::from_utf8(disc.read_file("SYSTEM.CNF").unwrap()).unwrap();
    assert_eq!(cnf.as_bytes(), scene_system_cnf());
    assert_eq!(
        cnf.replace(&format!("{:08X}", psx_scene_abi::STACK_INIT), "801FFFF0")
            .into_bytes(),
        crate::default_system_cnf(),
        "only the STACK value differs from the default"
    );
    assert_eq!(disc.find_file(TABLE_FILE_NAME).unwrap().lba, TABLE_LBA);
    assert_eq!(disc.find_file(BOOT_EXE_NAME).unwrap().lba, BOOT_EXE_LBA);
    // The next image is contiguous with the boot exe: 23 + 1 header + 3.
    assert_eq!(disc.find_file("GAME.EXE").unwrap().lba, 27);
    assert_eq!(
        disc.find_file(WORLD_PACK_FILE_NAME).unwrap().lba,
        WORLD_PACK_DEFAULT_START_LBA
    );

    let table = disc.read_file(TABLE_FILE_NAME).unwrap();
    assert_eq!(table, pressed.table);
    let header = decode_header_checked(&table).expect("valid table");
    assert_eq!(
        (header.scene_count, header.image_count, header.boot_scene),
        (3, 2, 0)
    );

    let boot = table_image(&table, &header, 0).unwrap();
    assert_eq!((boot.lba, boot.size, boot.load_addr), (23, 3 * 2048, BASE));
    assert_eq!(
        (boot.kind, boot.class, boot.evicts),
        (Kind::Program, None, 0)
    );
    let game = table_image(&table, &header, 1).unwrap();
    assert_eq!(game, pressed.images[1]);
    assert_eq!((game.lba, game.size), (27, 5 * 2048));
    assert_eq!(
        game.checksum,
        psx_pack::WordFnv32::of(&words(&game_payload))
    );
    assert_eq!(
        (game.core_hash, game.member_base, game.cdda_delta),
        (0, 0, 0)
    );
    // The image row's LBA is where the program's header sector really is.
    assert_eq!(&disc.read_sector_user(game.lba).unwrap()[..8], b"PS-X EXE");

    let s0 = table_scene(&table, &header, 0).unwrap();
    assert_eq!(
        (s0.name_str(), s0.image, s0.entry),
        ("BOOT", 0, BASE + 0x10)
    );
    assert_eq!(s0.flags, SCENE_FLAG_START);
    assert_eq!(s0.min_block_version, 2);
    let s1 = table_scene(&table, &header, 1).unwrap();
    assert_eq!((s1.image, s1.entry, s1.partner_credit), (1, BASE + 0x40, 3));
    assert_eq!(s1.flags, SCENE_FLAG_START | SCENE_FLAG_PARTNER_CREDIT);
    let s2 = table_scene(&table, &header, 2).unwrap();
    assert_eq!((s2.image, s2.entry, s2.flags), (0, BASE + 0x100, 0));
}

#[test]
fn an_image_too_big_for_the_boot_area_and_every_one_after_it_go_after_the_packs() {
    let mut plan = boot_plan();
    plan.images.push(ImageSpec::program(
        "BIG.EXE",
        Some(exe(BASE, BASE, 0, &payload(900, 3))),
    ));
    plan.images[0].reserve = Some(Span {
        start: 0x8010_0000,
        end: 0x8011_0000,
    });
    plan.images.push(ImageSpec::overlay(
        "SMALL.OVL",
        vec![7u8; 100],
        BOOT_EXE_NAME,
        0x8010_0000,
        Class::Swap,
    ));
    plan.scenes.push(SceneSpec::start("BIG", "BIG.EXE"));
    let (disc, pressed) = press(exe(BASE, BASE, 0, &payload(200, 0)), &plan, true).unwrap();
    let ui = disc.find_file(UI_PACK_FILE_NAME).unwrap();
    let after_ui = ui.lba + sectors_of(ui.size as usize);
    // Table order holds even though SMALL.OVL alone would have fitted.
    assert_eq!(pressed.images[1].lba, after_ui);
    assert_eq!(pressed.images[2].lba, after_ui + 901);
    // An Overlay's LBA is its first data sector and it loads whole sectors.
    let ovl = pressed.images[2];
    assert_eq!(
        (ovl.kind, ovl.load_addr, ovl.size),
        (Kind::Overlay, 0x8010_0000, 2048)
    );
    let mut padded = vec![7u8; 100];
    padded.resize(2048, 0);
    assert_eq!(ovl.checksum, psx_pack::WordFnv32::of(&words(&padded)));
    assert_eq!(
        &disc.read_sector_user(ovl.lba).unwrap()[..2048],
        &padded[..]
    );
}

/// A program image the way psoxide-core.ld stores it: `data` bytes of text
/// and data, then the core copy, then zeros to whole sectors.
fn with_core(data: usize, core: &[u8]) -> Vec<u8> {
    let mut image: Vec<u8> = (0..data).map(|i| (i as u8) ^ 0x5a | 1).collect();
    image.extend_from_slice(core);
    image.resize(image.len().next_multiple_of(SECTOR_BYTES), 0);
    image
}

fn core_image() -> Vec<u8> {
    (0..256u32)
        .flat_map(|w| w.wrapping_mul(0x0101_0101).wrapping_add(1).to_le_bytes())
        .collect()
}

#[test]
fn programs_carry_the_core_hash_and_stop_before_the_stored_core() {
    let core = core_image();
    let hash = psx_pack::WordFnv32::of(&words(&core));
    // 5,000 bytes of program: the core copy starts in sector 2, so the
    // payload the core loads is three sectors, not the four stored.
    let mut image = with_core(5_000, &core);
    assert_eq!(image.len(), 3 * SECTOR_BYTES);
    image.resize(4 * SECTOR_BYTES, 0);
    assert_eq!(payload_before_core(&image, &core), Some(3 * SECTOR_BYTES));
    let mut plan = boot_plan();
    plan.images.push(ImageSpec::program(
        "GAME.EXE",
        Some(exe(BASE, BASE, 0, &image)),
    ));
    plan.scenes.push(SceneSpec::start("GAME", "GAME.EXE"));
    plan.core_image = Some(core.clone());
    let boot_exe = exe(BASE, BASE, 0, &with_core(100, &core));
    let (disc, pressed) = press(boot_exe.clone(), &plan, false).unwrap();
    assert_eq!(pressed.images[0].core_hash, hash);
    assert_eq!(pressed.images[0].size, SECTOR_BYTES as u32);
    let row = pressed.images[1];
    assert_eq!(row.core_hash, hash);
    assert_eq!(row.size, 3 * SECTOR_BYTES as u32);
    assert_eq!(
        disc.find_file("GAME.EXE").unwrap().size as usize,
        EXE_HEADER_BYTES + 4 * SECTOR_BYTES,
        "the file on the disc is still whole"
    );
    assert_eq!(
        row.checksum,
        psx_pack::WordFnv32::of(&words(&image[..3 * SECTOR_BYTES]))
    );
    // A core copy that starts on a sector boundary is left out whole.
    let exact = with_core(SECTOR_BYTES, &core);
    assert_eq!(payload_before_core(&exact, &core), Some(SECTOR_BYTES));

    // A program that does not carry the core, or carries it with something
    // after it, is refused...
    for file in [payload(2, 9), {
        let mut tail = with_core(100, &core);
        tail[2000] = 1;
        tail
    }] {
        plan.images[1].file = Some(exe(BASE, BASE, 0, &file));
        assert_eq!(
            press(boot_exe.clone(), &plan, false).err().unwrap(),
            SceneDiscError::CoreMissing("GAME.EXE".into())
        );
    }
    // ...unless the image names its own hash (a program of another pin), which
    // keeps its whole payload.
    plan.images[1].core_hash = Some(0x1234_5678);
    let (_, pressed) = press(boot_exe, &plan, false).unwrap();
    assert_eq!(pressed.images[1].core_hash, 0x1234_5678);
    assert_eq!(pressed.images[1].size, SECTOR_BYTES as u32);

    plan.core_image = Some(vec![1, 2, 3]);
    assert_eq!(
        press(small_exe(), &plan, false).err().unwrap(),
        SceneDiscError::CoreImage(3)
    );
}

#[test]
fn a_collection_program_row_carries_its_member_base() {
    // mkdisc's use: a member's program row, then the member's place on the collection disc.
    let file = exe(BASE, BASE + 8, 0x801f_df00, &payload(2, 4));
    let (mut row, pc) = program_image_row("PSX.EXE", &file, 23, None, None).unwrap();
    assert_eq!(pc, BASE + 8);
    assert_eq!((row.member_base, row.cdda_delta), (0, 0));
    row.member_base = 40_000;
    row.cdda_delta = 7;
    let scene = SceneRow {
        id: 0,
        image: 0,
        flags: SCENE_FLAG_START,
        entry: pc,
        partner_credit: 0,
        min_block_version: 2,
        name: SceneRow::name_bytes("MEMBER"),
    };
    let mut table = vec![0u8; SECTOR_BYTES];
    psx_scene_abi::encode_table(&[scene], &[row], 0, &mut table).unwrap();
    let header = decode_header_checked(&table).unwrap();
    let back = table_image(&table, &header, 0).unwrap();
    assert_eq!(
        (back.lba, back.member_base, back.cdda_delta),
        (23, 40_000, 7)
    );
}

#[test]
fn programs_must_load_between_the_kernel_and_the_core_window() {
    // Ends exactly at the switch stack (1 KB below the window): accepted.
    let top = psx_scene_abi::PAYLOAD_END - 2 * SECTOR_BYTES as u32;
    let mut plan = boot_plan();
    plan.images.push(ImageSpec::program(
        "TOP.EXE",
        Some(exe(top, top, 0, &payload(2, 0))),
    ));
    plan.scenes.push(SceneSpec::start("TOP", "TOP.EXE"));
    press(small_exe(), &plan, false).unwrap();
    // Into the switch stack, ending at the window, or below the kernel's 64 KiB: refused.
    for t_addr in [top + 4, CORE_WINDOW.start - 4096, 0x8000_f800] {
        let mut plan = plan.clone();
        plan.images[1].file = Some(exe(t_addr, t_addr, 0, &payload(2, 0)));
        assert_eq!(
            refuse(&plan),
            SceneDiscError::Bounds {
                name: "TOP.EXE".into(),
                start: t_addr,
                end: t_addr + 4096
            }
        );
    }
    // The boot exe is held to the same bounds.
    let high = CORE_WINDOW.start - SECTOR_BYTES as u32;
    assert!(matches!(
        press(exe(high, high, 0, &payload(2, 0)), &boot_plan(), false),
        Err(SceneDiscError::Bounds { .. })
    ));
}

/// The boot program (16 KB at BASE) with a reserve, two RESIDENT_SET overlays side by side and
/// two SWAP overlays, one over each of them.
fn overlay_plan() -> ScenePlan {
    let mut plan = boot_plan();
    plan.images[0].reserve = Some(Span {
        start: 0x8010_0000,
        end: 0x8012_0000,
    });
    let ovl = |name: &str, at: u32, sectors: usize, class| {
        ImageSpec::overlay(name, payload(sectors, 5), BOOT_EXE_NAME, at, class)
    };
    plan.images.extend([
        ovl("SET.OVL", 0x8010_0000, 2, Class::ResidentSet),
        ovl("SET2.OVL", 0x8010_1000, 1, Class::ResidentSet),
        ovl("SWAPA.OVL", 0x8010_0800, 1, Class::Swap),
        ovl("SWAPB.OVL", 0x8011_0000, 4, Class::Swap),
    ]);
    plan.scenes
        .push(SceneSpec::at("LEVEL", "SWAPA.OVL", 0x8010_0800));
    plan
}

#[test]
fn an_overlay_evicts_exactly_the_images_its_range_intersects() {
    let boot = exe(BASE, BASE, 0, &payload(8, 1));
    let (_, pressed) = press(boot.clone(), &overlay_plan(), false).unwrap();
    let rows = &pressed.images;
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].evicts, 0, "a Program carries no mask");
    // SET 0x80100000..0x80101000 meets SWAPA 0x80100800..0x80101000; SET2 0x80101000.. meets nothing.
    assert_eq!(rows[1].evicts, 1 << 3);
    assert_eq!(rows[2].evicts, 0);
    assert_eq!(rows[3].evicts, 1 << 1);
    assert_eq!(rows[4].evicts, 0);
    for row in &rows[1..] {
        assert_eq!((row.kind, row.owner, row.core_hash), (Kind::Overlay, 0, 0));
    }
    assert_eq!(rows[1].class, Some(Class::ResidentSet));
    assert_eq!(rows[3].class, Some(Class::Swap));
    // The overlay scene enters inside the overlay; START is not set.
    let level = pressed.scenes[1];
    assert_eq!((level.image, level.entry, level.flags), (3, 0x8010_0800, 0));

    // Two RESIDENT_SETs of one owner are co-resident: overlapping, refused, naming both.
    let mut plan = overlay_plan();
    plan.images[2].load_addr = 0x8010_0800;
    assert_eq!(
        press(boot.clone(), &plan, false).err().unwrap(),
        SceneDiscError::Overlap {
            a: "SET.OVL".into(),
            b: "SET2.OVL".into()
        }
    );
    // An overlay over its owner's own load range: refused (the reserve is moved over it so the
    // reserve check passes and the overlap check is what refuses).
    let mut plan = overlay_plan();
    plan.images[0].reserve = Some(Span {
        start: BASE,
        end: 0x8012_0000,
    });
    plan.images[4].load_addr = BASE + 0x1000;
    assert_eq!(
        press(boot.clone(), &plan, false).err().unwrap(),
        SceneDiscError::Overlap {
            a: BOOT_EXE_NAME.into(),
            b: "SWAPB.OVL".into()
        }
    );
    // Outside the reserve, at either end.
    for at in [0x800f_f800, 0x8011_fc00] {
        let mut plan = overlay_plan();
        plan.images[3].load_addr = at;
        assert!(matches!(
            press(boot.clone(), &plan, false),
            Err(SceneDiscError::OutsideReserve { .. })
        ));
    }
    // A reserve reaching past PAYLOAD_END, an owner without one, an owner that is an Overlay,
    // an unknown owner.
    let mut plan = overlay_plan();
    plan.images[0].reserve = Some(Span {
        start: 0x8010_0000,
        end: CORE_WINDOW.start,
    });
    assert!(matches!(
        press(boot.clone(), &plan, false),
        Err(SceneDiscError::Bounds { .. })
    ));
    let mut plan = overlay_plan();
    plan.images[0].reserve = None;
    assert_eq!(
        press(boot.clone(), &plan, false).err().unwrap(),
        SceneDiscError::Overlay("SET.OVL".into())
    );
    let mut plan = overlay_plan();
    plan.images[4].owner = Some("SET.OVL".into());
    assert_eq!(
        press(boot.clone(), &plan, false).err().unwrap(),
        SceneDiscError::Overlay("SWAPB.OVL".into())
    );
    let mut plan = overlay_plan();
    plan.images[4].owner = Some("NONE.EXE".into());
    assert_eq!(
        press(boot, &plan, false).err().unwrap(),
        SceneDiscError::Unknown("NONE.EXE".into())
    );
}

#[test]
fn every_program_has_one_start_scene_and_entries_lie_in_their_image() {
    let boot = || exe(BASE, BASE, 0, &payload(8, 1));
    let refuse = |plan: &ScenePlan| press(boot(), plan, false).err().unwrap();
    // No START scene for the boot program, or two.
    let mut plan = overlay_plan();
    plan.scenes[0] = SceneSpec::at("BOOT", BOOT_EXE_NAME, BASE);
    assert_eq!(
        refuse(&plan),
        SceneDiscError::StartScenes {
            image: BOOT_EXE_NAME.into(),
            count: 0
        }
    );
    let mut plan = overlay_plan();
    plan.scenes.push(SceneSpec::start("AGAIN", BOOT_EXE_NAME));
    assert_eq!(
        refuse(&plan),
        SceneDiscError::StartScenes {
            image: BOOT_EXE_NAME.into(),
            count: 2
        }
    );
    // An in-program entry past the program's load range, an overlay entry outside the overlay,
    // START on an overlay scene, an entry given for a START scene.
    for scene in [
        SceneSpec::at("X", BOOT_EXE_NAME, BASE + 8 * 2048),
        SceneSpec::at("X", "SWAPA.OVL", 0x8010_0000),
        SceneSpec::start("X", "SWAPA.OVL"),
        SceneSpec {
            entry: Some(BASE),
            ..SceneSpec::start("X", BOOT_EXE_NAME)
        },
    ] {
        let mut plan = overlay_plan();
        plan.scenes.push(scene);
        assert_eq!(refuse(&plan), SceneDiscError::Entry("X".into()));
    }
    // The last in-program byte is fine.
    let mut plan = overlay_plan();
    plan.scenes
        .push(SceneSpec::at("X", BOOT_EXE_NAME, BASE + 8 * 2048 - 4));
    press(boot(), &plan, false).unwrap();
    // A scene on an unknown image.
    let mut plan = overlay_plan();
    plan.scenes.push(SceneSpec::start("X", "NONE.EXE"));
    assert_eq!(refuse(&plan), SceneDiscError::Unknown("NONE.EXE".into()));
}

#[test]
fn each_program_gets_a_compact_copy_of_the_rows_it_reaches() {
    let boot = exe(BASE, BASE, 0, &payload(8, 1));
    let mut plan = overlay_plan();
    // A second program with a menu scene of its own; scenes: 0 BOOT, 1 LEVEL, 2 GAME, 3 GMENU.
    plan.images.push(ImageSpec::program(
        "GAME.EXE",
        Some(exe(BASE, BASE, 0, &payload(2, 2))),
    ));
    plan.scenes.push(SceneSpec::start("GAME", "GAME.EXE"));
    plan.scenes
        .push(SceneSpec::at("GMENU", "GAME.EXE", BASE + 4));
    // The boot program reaches every row (the default); the game reaches only BOOT.
    plan.images[5].reaches = Some(vec!["BOOT".into()]);
    let (_, pressed) = press(boot.clone(), &plan, false).unwrap();
    assert_eq!(pressed.compact.len(), 2);

    let all = &pressed.compact[0];
    assert_eq!(all.image, 0);
    assert_eq!(all.scenes, [0, 1, 2, 3]);
    assert_eq!(all.images, 0b11_1111);
    assert_eq!(all.bytes.len(), 64 + 32 * (4 + 6));
    assert_eq!(all.bytes[64..], pressed.table[64..all.bytes.len()]);

    // GAME: its own scenes (GAME, GMENU), BOOT, and images 0 and 5; images 1..4 are zero rows.
    let game = &pressed.compact[1];
    assert_eq!(game.image, 5);
    assert_eq!(game.scenes, [0, 2, 3]);
    assert_eq!(game.images, 0b10_0001);
    assert_eq!(game.bytes.len(), compact_bytes(3, 0b10_0001));
    assert_eq!(game.bytes.len(), 64 + 32 * (3 + 6));
    let header = decode_header_checked(&game.bytes).unwrap();
    assert!(header.compact);
    assert_eq!(
        table_scene(&game.bytes, &header, 3),
        Some(pressed.scenes[3])
    );
    assert_eq!(table_scene(&game.bytes, &header, 1), None);
    assert_eq!(
        table_image(&game.bytes, &header, 5),
        Some(pressed.images[5])
    );
    assert_eq!(table_image(&game.bytes, &header, 3), None);

    // A reach that names no scene is refused.
    plan.images[5].reaches = Some(vec!["NOPE".into()]);
    assert_eq!(
        press(boot, &plan, false).err().unwrap(),
        SceneDiscError::Unknown("NOPE".into())
    );
}

#[test]
fn a_bad_plan_is_refused() {
    let ovl =
        |name: &str| ImageSpec::overlay(name, vec![1], BOOT_EXE_NAME, 0x8010_0000, Class::Swap);
    let mut empty = boot_plan();
    empty.images.clear();
    assert_eq!(
        refuse(&empty),
        SceneDiscError::RowCount {
            images: 0,
            scenes: 1
        }
    );
    let mut no_scenes = boot_plan();
    no_scenes.scenes.clear();
    assert!(matches!(
        refuse(&no_scenes),
        SceneDiscError::RowCount { .. }
    ));
    // Image 0 must be the boot exe; no other image may stand for it.
    let mut plan = boot_plan();
    plan.images[0] = ImageSpec::program("BOOT.EXE", None);
    assert_eq!(refuse(&plan), SceneDiscError::BootImage("BOOT.EXE".into()));
    let mut plan = boot_plan();
    plan.images[0] = ovl(BOOT_EXE_NAME);
    plan.images[0].file = None;
    assert_eq!(
        refuse(&plan),
        SceneDiscError::BootImage(BOOT_EXE_NAME.into())
    );
    let mut plan = boot_plan();
    plan.images.push(ImageSpec::program("B.EXE", None));
    assert_eq!(refuse(&plan), SceneDiscError::BootImage("B.EXE".into()));
    // Names.
    let mut plan = boot_plan();
    plan.images.push(ovl("WORLD.PAK"));
    assert_eq!(
        refuse(&plan),
        SceneDiscError::DuplicateName("WORLD.PAK".into())
    );
    let mut plan = boot_plan();
    plan.images.push(ovl("A_NAME_OVER_16.OVL"));
    assert_eq!(
        refuse(&plan),
        SceneDiscError::BadName("A_NAME_OVER_16.OVL".into())
    );
    let mut plan = boot_plan();
    plan.scenes.push(SceneSpec::start("BOOT", BOOT_EXE_NAME));
    assert_eq!(refuse(&plan), SceneDiscError::DuplicateName("BOOT".into()));
    // A Program that is not an exe, a Program with overlay fields.
    let mut plan = boot_plan();
    plan.images
        .push(ImageSpec::program("X.EXE", Some(vec![0; 10])));
    assert_eq!(refuse(&plan), SceneDiscError::NotAnExe("X.EXE".into()));
    let mut plan = boot_plan();
    plan.images[0].class = Some(Class::Swap);
    assert_eq!(refuse(&plan), SceneDiscError::Overlay(BOOT_EXE_NAME.into()));
    // Limits: 33 images; 62 rows pass, 63 do not.
    let mut plan = boot_plan();
    plan.images[0].reserve = Some(Span {
        start: 0x8010_0000,
        end: 0x8011_0000,
    });
    plan.images
        .extend((1..33).map(|i| ovl(&format!("O{i}.OVL"))));
    assert_eq!(
        refuse(&plan),
        SceneDiscError::RowCount {
            images: 33,
            scenes: 1
        }
    );
    plan.images.truncate(32);
    plan.scenes
        .extend((1..30).map(|i| SceneSpec::at(&format!("S{i}"), BOOT_EXE_NAME, BASE)));
    let (_, pressed) = press(small_exe(), &plan, false).unwrap();
    assert_eq!(pressed.scenes.len() + pressed.images.len(), 62);
    plan.scenes.push(SceneSpec::at("S30", BOOT_EXE_NAME, BASE));
    assert_eq!(
        refuse(&plan),
        SceneDiscError::RowCount {
            images: 32,
            scenes: 31
        }
    );
}

#[test]
fn the_table_costs_the_boot_exe_one_sector_of_room() {
    // A program cannot be larger than the RAM below the window (988 sectors),
    // so CDTEST.BIN shrinks the boot area to show the edge: 1024 - 100 - 23.
    let plan = boot_plan();
    let lay = |exe_bytes: Vec<u8>| {
        let mut builder = IsoBuilder::new();
        add_scene_disc_files(&mut builder, exe_bytes, None, None, Some(100), &plan).err()
    };
    assert_eq!(lay(exe(BASE, BASE, 0, &payload(900, 0))), None);
    let too_big = exe(BASE, BASE, 0, &payload(901, 0));
    assert_eq!(
        lay(too_big.clone()),
        Some(SceneDiscError::Layout(
            PlaytestLayoutError::BootExeTooLarge {
                exe_sectors: 902,
                max_sectors: 901
            }
        ))
    );
    // The same exe still fits the plain layout, which starts it at 22.
    let mut builder = IsoBuilder::new();
    crate::add_playtest_files(&mut builder, too_big, None, None, Some(100)).unwrap();
}
