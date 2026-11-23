use super::*;
use crate::{build_world_pack, load_boot_exe_from_disc, Disc, EXE_HEADER_BYTES};
use psx_scene_abi::{decode_header_checked, table_row, ROW_FLAG_PARTNER_CREDIT};

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

fn boot() -> SceneSlot {
    SceneSlot::new(BOOT_EXE_NAME, Kind::Program, None)
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

#[test]
fn the_table_sits_at_22_the_boot_exe_at_23_and_scenes_follow_it() {
    let boot_payload = payload(3, 1);
    let game_payload = payload(5, 2);
    let mut game = SceneSlot::new(
        "game.exe",
        Kind::Program,
        Some(exe(0x8001_0000, 0x8001_0040, 0x801f_d700, &game_payload)),
    );
    game.return_scene = 0;
    game.read_attempts = 5;
    game.partner_credit = 3;
    game.flags = ROW_FLAG_PARTNER_CREDIT;
    let plan = ScenePlan {
        slots: vec![boot(), game],
        core_image: None,
    };
    let (disc, pressed) = press(
        exe(0x8001_0000, 0x8001_0010, 0x801f_d700, &boot_payload),
        &plan,
        true,
    )
    .unwrap();

    // The BIOS path is unchanged: SYSTEM.CNF boots PSX.EXE, now at 23.
    let booted = load_boot_exe_from_disc(&disc).expect("boots");
    assert_eq!(booted.exe.payload, boot_payload);
    assert_eq!(disc.find_file("SYSTEM.CNF").unwrap().lba, 21);
    assert_eq!(disc.find_file(TABLE_FILE_NAME).unwrap().lba, TABLE_LBA);
    assert_eq!(disc.find_file(BOOT_EXE_NAME).unwrap().lba, BOOT_EXE_LBA);
    // The first scene is contiguous with the boot exe: 23 + 1 header + 3.
    assert_eq!(disc.find_file("GAME.EXE").unwrap().lba, 27);
    assert_eq!(
        disc.find_file(WORLD_PACK_FILE_NAME).unwrap().lba,
        WORLD_PACK_DEFAULT_START_LBA
    );

    // The table on the disc is the one returned, and it checks.
    let table = disc.read_file(TABLE_FILE_NAME).unwrap();
    assert_eq!(table, pressed.table);
    let header = decode_header_checked(&table).expect("valid table");
    assert_eq!(header.row_count, 2);
    assert_eq!(header.table_sectors, 1);
    assert_eq!(header.boot_scene, 0);

    let boot_row = table_row(&table, &header, 0).unwrap();
    assert_eq!(boot_row.name_str(), "PSX.EXE");
    assert_eq!((boot_row.lba, boot_row.sectors), (23, 4));
    assert_eq!(boot_row.pc, 0x8001_0010);
    assert_eq!(boot_row.sp, 0x801f_d700);
    assert_eq!(boot_row.return_scene, NO_SCENE);

    let row = table_row(&table, &header, 1).unwrap();
    assert_eq!(row, pressed.rows[1]);
    assert_eq!(row.kind, Kind::Program);
    assert_eq!((row.lba, row.sectors), (27, 6));
    assert_eq!(row.t_addr, 0x8001_0000);
    assert_eq!(row.t_size, 5 * SECTOR_BYTES as u32);
    assert_eq!(row.pc, 0x8001_0040);
    assert_eq!(row.checksum, psx_pack::WordFnv32::of(&words(&game_payload)));
    assert_eq!(row.core_hash, 0);
    assert_eq!(row.return_scene, 0);
    assert_eq!(row.read_attempts, 5);
    assert_eq!(row.min_block_version, 1);
    assert_eq!(row.partner_credit, 3);
    assert_eq!(row.flags, ROW_FLAG_PARTNER_CREDIT);
    // The row's LBA is where the program's header sector really is.
    assert_eq!(&disc.read_sector_user(row.lba).unwrap()[..8], b"PS-X EXE");
}

#[test]
fn a_scene_too_big_for_the_boot_area_and_every_one_after_it_go_after_the_packs() {
    let big = SceneSlot::new(
        "BIG.EXE",
        Kind::Program,
        Some(exe(0x8001_0000, 0x8001_0000, 0, &payload(900, 3))),
    );
    let small = SceneSlot::new("SMALL.DAT", Kind::Data, Some(vec![7u8; 100]));
    let plan = ScenePlan {
        slots: vec![boot(), big, small],
        core_image: None,
    };
    let (disc, pressed) = press(
        exe(0x8001_0000, 0x8001_0000, 0, &payload(200, 0)),
        &plan,
        true,
    )
    .unwrap();
    let ui = disc.find_file(UI_PACK_FILE_NAME).unwrap();
    let after_ui = ui.lba + sectors_of(ui.size as usize);
    // Table order holds even though SMALL.DAT alone would have fitted.
    assert_eq!(pressed.rows[1].lba, after_ui);
    assert_eq!(pressed.rows[2].lba, after_ui + 901);
    let data = pressed.rows[2];
    assert_eq!((data.kind, data.t_addr, data.t_size), (Kind::Data, 0, 2048));
    let mut padded = vec![7u8; 100];
    padded.resize(2048, 0);
    assert_eq!(data.checksum, psx_pack::WordFnv32::of(&words(&padded)));
}

#[test]
fn rows_carry_the_core_hash_and_a_program_must_contain_the_core() {
    let core: Vec<u8> = (0..256u32)
        .flat_map(|w| (w * 0x0101_0101).to_le_bytes())
        .collect();
    let hash = psx_pack::WordFnv32::of(&words(&core));
    let mut with_core = payload(2, 9);
    with_core[0x400..0x400 + core.len()].copy_from_slice(&core);
    let game = SceneSlot::new(
        "GAME.EXE",
        Kind::Program,
        Some(exe(0x8001_0000, 0x8001_0000, 0, &with_core)),
    );
    let plan = ScenePlan {
        slots: vec![boot(), game.clone()],
        core_image: Some(core.clone()),
    };
    let (_, pressed) = press(exe(0x8001_0000, 0x8001_0000, 0, &with_core), &plan, false).unwrap();
    assert_eq!(pressed.rows[0].core_hash, hash);
    assert_eq!(pressed.rows[1].core_hash, hash);

    // A program that does not carry the core is refused...
    let mut plan = plan;
    plan.slots[1].file = Some(exe(0x8001_0000, 0x8001_0000, 0, &payload(2, 9)));
    let boot_exe = exe(0x8001_0000, 0x8001_0000, 0, &with_core);
    assert_eq!(
        press(boot_exe.clone(), &plan, false).err().unwrap(),
        SceneDiscError::CoreMissing("GAME.EXE".into())
    );
    // ...unless the row names its own hash (a program of another pin).
    plan.slots[1].core_hash = Some(0x1234_5678);
    let (_, pressed) = press(boot_exe, &plan, false).unwrap();
    assert_eq!(pressed.rows[1].core_hash, 0x1234_5678);

    plan.core_image = Some(vec![1, 2, 3]);
    assert_eq!(
        press(exe(0x8001_0000, 0x8001_0000, 0, &with_core), &plan, false)
            .err()
            .unwrap(),
        SceneDiscError::CoreImage(3)
    );
}

#[test]
fn programs_must_load_between_the_kernel_and_the_core_window() {
    let small = exe(0x8001_0000, 0x8001_0000, 0, &payload(1, 0));
    // Ends exactly at the window: accepted.
    let top = CORE_WINDOW.start - 2 * SECTOR_BYTES as u32;
    let fits = SceneSlot::new(
        "TOP.EXE",
        Kind::Program,
        Some(exe(top, top, 0, &payload(2, 0))),
    );
    let plan = ScenePlan {
        slots: vec![boot(), fits],
        core_image: None,
    };
    press(small.clone(), &plan, false).unwrap();
    // One sector into the window, or below the kernel's 64 KiB: refused.
    for t_addr in [top + SECTOR_BYTES as u32, 0x8000_f800] {
        let mut plan = plan.clone();
        plan.slots[1].file = Some(exe(t_addr, t_addr, 0, &payload(2, 0)));
        assert_eq!(
            press(small.clone(), &plan, false).err().unwrap(),
            SceneDiscError::Bounds {
                name: "TOP.EXE".into(),
                t_addr,
                t_size: 4096
            }
        );
    }
    // The boot exe is held to the same bounds.
    let plan = ScenePlan {
        slots: vec![boot()],
        core_image: None,
    };
    let high = CORE_WINDOW.start - SECTOR_BYTES as u32;
    assert!(matches!(
        press(exe(high, high, 0, &payload(2, 0)), &plan, false),
        Err(SceneDiscError::Bounds { .. })
    ));
}

#[test]
fn a_bad_plan_is_refused() {
    let small = || exe(0x8001_0000, 0x8001_0000, 0, &payload(1, 0));
    let data = |name: &str| SceneSlot::new(name, Kind::Data, Some(vec![1]));
    let refuse = |slots: Vec<SceneSlot>| {
        press(
            small(),
            &ScenePlan {
                slots,
                core_image: None,
            },
            false,
        )
        .err()
        .unwrap()
    };
    assert_eq!(refuse(vec![]), SceneDiscError::RowCount(0));
    assert_eq!(refuse(vec![data("A.DAT")]), SceneDiscError::BootSlots(0));
    assert_eq!(refuse(vec![boot(), boot()]), SceneDiscError::BootSlots(2));
    assert_eq!(
        refuse(vec![SceneSlot::new("BOOT.EXE", Kind::Program, None)]),
        SceneDiscError::BootName("BOOT.EXE".into())
    );
    assert_eq!(
        refuse(vec![boot(), data("A.DAT"), data("a.dat")]),
        SceneDiscError::DuplicateName("A.DAT".into())
    );
    assert_eq!(
        refuse(vec![boot(), data("WORLD.PAK")]),
        SceneDiscError::DuplicateName("WORLD.PAK".into())
    );
    assert_eq!(
        refuse(vec![boot(), data("A_NAME_LONGER_THAN_20.DAT")]),
        SceneDiscError::BadName("A_NAME_LONGER_THAN_20.DAT".into())
    );
    assert_eq!(
        refuse(vec![
            boot(),
            SceneSlot::new("X.EXE", Kind::Program, Some(vec![0; 10]))
        ]),
        SceneDiscError::NotAnExe("X.EXE".into())
    );
    let mut many = vec![boot()];
    many.extend((0..31).map(|i| data(&alloc::format!("D{i}.DAT"))));
    assert_eq!(refuse(many), SceneDiscError::RowCount(32));
}

#[test]
fn the_table_costs_the_boot_exe_one_sector_of_room() {
    // A program cannot be larger than the RAM below the window (988 sectors),
    // so CDTEST.BIN shrinks the boot area to show the edge: 1024 - 100 - 23.
    let plan = ScenePlan {
        slots: vec![boot()],
        core_image: None,
    };
    let lay = |exe_bytes: Vec<u8>| {
        let mut builder = IsoBuilder::new();
        add_scene_disc_files(&mut builder, exe_bytes, None, None, Some(100), &plan).err()
    };
    assert_eq!(
        lay(exe(0x8001_0000, 0x8001_0000, 0, &payload(900, 0))),
        None
    );
    let too_big = exe(0x8001_0000, 0x8001_0000, 0, &payload(901, 0));
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
