//! Gate tests that need the stress-world generator.

use std::path::PathBuf;

use crate::stream_world::{generate, load_donor, StreamWorldConfig};

use super::{partition, report_for_project, Partition, PartitionInput, PartitionParams};

fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects")
}

#[test]
fn generated_small_world_partitions() {
    let donor = load_donor(&projects_dir()).expect("donor");
    let world = generate(&StreamWorldConfig::small([4, 4]), &donor).expect("world");
    let root = projects_dir().join("graybox-reach");
    let (part, report) =
        report_for_project(&world.project, &root, &PartitionParams::default()).expect("report");
    assert!(part.regions.len() >= 2);
    assert!(report.text.contains("Verdict:"));
}

/// Run the gates on one generated world under the cut target and the pool it
/// is made for. Nothing else is relaxed.
fn gate_run(config: &StreamWorldConfig, mutate: impl Fn(&mut PartitionParams)) -> Partition {
    let donor = load_donor(&projects_dir()).expect("donor");
    let world = generate(config, &donor).expect("world");
    let root = projects_dir().join("graybox-reach");
    let input = PartitionInput::from_project(&world.project, &root).expect("input");
    let mut params = PartitionParams::default();
    world.overrides.apply(&mut params);
    mutate(&mut params);
    partition(&input, &params)
}

fn has(part: &Partition, kind: &str) -> bool {
    part.gate.failures.iter().any(|f| f.kind() == kind)
}

#[test]
fn dense_variant_is_rejected_by_the_cook_gates() {
    let base_config = StreamWorldConfig::small([7, 7]);
    let dense_config = base_config.dense_variant();

    // Same budgets for both worlds: the derived region sizes, everything else
    // at the design defaults.
    let base = gate_run(&base_config, |_| {});
    let dense = gate_run(&dense_config, |_| {});
    assert!(
        has(&dense, "pool") || has(&dense, "rho") || has(&dense, "rank-row-too-wide"),
        "the dense variant must fail a gate: {:?}",
        dense.gate.failures
    );

    // Each gate separates the two worlds: put its threshold between them.
    let (base_rho, dense_rho) = (base.gate.rho_worst(), dense.gate.rho_worst());
    assert!(dense_rho > base_rho, "{dense_rho} vs {base_rho}");
    let (base_pool, dense_pool) = (
        base.gate.pool.as_ref().unwrap().bytes,
        dense.gate.pool.as_ref().unwrap().bytes,
    );
    assert!(dense_pool > base_pool);

    // rho: choose the utilisation whose limit sits between them.
    let mid = (base_rho + dense_rho) / 2.0;
    let util =
        (mid / base.gate.rho_gate_limit * f64::from(base.params.utilisation_pct)).floor() as u32;
    let tight = |part_config: &StreamWorldConfig| {
        gate_run(part_config, |p| {
            p.utilisation_pct = util;
            p.pool_bytes = u32::MAX; // isolate rho
        })
    };
    assert!(!has(&tight(&base_config), "rho"), "base passes at {util}%");
    assert!(has(&tight(&dense_config), "rho"), "dense fails at {util}%");

    // pool: exactly what the base world's peak and skeleton (paid twice: the
    // baked container and its heap copy) need, which the dense world's larger
    // peak does not fit.
    let pool = (base_pool + 2 * base.gate.skeleton_bytes) as u32;
    let sized = |part_config: &StreamWorldConfig| {
        gate_run(part_config, |p| {
            p.pool_bytes = pool;
            p.utilisation_pct = 100; // isolate the pool
        })
    };
    assert!(!has(&sized(&base_config), "pool"));
    assert!(has(&sized(&dense_config), "pool"));
}

#[test]
fn need_contains_visibility_and_hook_landing_closures() {
    let donor = load_donor(&projects_dir()).expect("donor");
    let world = generate(&StreamWorldConfig::small([8, 8]), &donor).expect("world");
    assert!(world.stats.hooks > 0, "the world should hold a hook");
    let root = projects_dir().join("graybox-reach");
    let input = PartitionInput::from_project(&world.project, &root).expect("input");
    let part = partition(&input, &PartitionParams::default());
    let mut pulled = 0;
    for (r, visible) in part.closure.visible.iter().enumerate() {
        for &q in visible {
            assert!(part.closure.need[r].contains(q));
        }
        for &landing in &part.closure.hook_pulls[r] {
            pulled += 1;
            for &q in &part.closure.visible[landing as usize] {
                assert!(
                    part.closure.need[r].contains(q),
                    "hook landing closure of {landing} missing for {r}"
                );
            }
        }
    }
    // Hook pulls are the design's discontinuity rule; if none fired the
    // world placed its hooks out of range of anything that sees them.
    assert!(pulled > 0, "no region pulled in a hook landing");
}

#[test]
fn module_estimates_bound_the_cut_search_model_so_no_module_is_cut() {
    use super::cuts::model_bytes_in;
    use super::geometry::Aabb;

    let donor = load_donor(&projects_dir()).expect("donor");
    let root = projects_dir().join("graybox-reach");
    let mut worst: std::collections::BTreeMap<String, (f64, f64)> = Default::default();
    for seed in [3u64, 11, 31] {
        let config = StreamWorldConfig {
            seed,
            terrain_pct: 15,
            courtyard_pct: 15,
            // Room for the heaviest module of every kind.
            region_target_bytes: 24_576,
            ..StreamWorldConfig::small([6, 6])
        };
        let world = generate(&config, &donor).expect("world");
        let input = PartitionInput::from_project(&world.project, &root).expect("input");
        let m = f64::from(config.module_units);
        for (index, estimate) in world.module_bytes.iter().enumerate() {
            let info = world.routes.modules[index];
            let cell = Aabb {
                min: [f64::from(info.x), -1.0e6, f64::from(info.z)],
                max: [f64::from(info.x) + m, 1.0e6, f64::from(info.z) + m],
            };
            let actual = model_bytes_in(&input, &cell);
            let entry = worst
                .entry(format!("{:?}", info.kind))
                .or_insert((f64::MAX, 0.0));
            let ratio = actual / f64::from(*estimate);
            entry.0 = entry.0.min(ratio);
            entry.1 = entry.1.max(ratio);
            assert!(
                f64::from(*estimate) <= f64::from(config.region_target_bytes),
                "module {index} was built over the budget"
            );
        }
        assert_eq!(world.stats.over_budget_modules, 0);
    }
    eprintln!("actual / estimate by kind (min, max): {worst:?}");
    for (kind, (_, max)) in &worst {
        assert!(*max <= 1.0, "{kind} models {max:.2}x its estimate");
    }
}
