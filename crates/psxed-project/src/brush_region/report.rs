//! Cook report: text and JSON, deterministic for equal inputs (no clocks, no
//! hash iteration, fixed-precision decimals).

use std::fmt::Write as _;

use super::gate::{set_bytes, GateFailure};
use super::geometry::V3;
use super::input::PartitionInput;
use super::layout::SEEK_CLASSES;
use super::{ClosureSource, Partition};

/// Bump when a key is renamed or removed.
pub const REPORT_VERSION: u32 = 1;

/// A rendered report.
#[derive(Clone, Debug)]
pub struct StreamReport {
    pub passed: bool,
    pub text: String,
    pub json: String,
}

enum Value {
    Int(i64),
    Num(f64, usize),
    Str(String),
    Bool(bool),
    Arr(Vec<Value>),
    Obj(Vec<(&'static str, Value)>),
}

fn int(v: impl TryInto<i64>) -> Value {
    Value::Int(v.try_into().unwrap_or(i64::MAX))
}

fn text(v: impl Into<String>) -> Value {
    Value::Str(v.into())
}

fn vec3(v: V3) -> Value {
    Value::Arr(v.iter().map(|&c| Value::Num(c, 1)).collect())
}

fn ids(list: &[u32]) -> Value {
    Value::Arr(list.iter().map(|&i| int(i)).collect())
}

impl Value {
    fn write(&self, out: &mut String, indent: usize) {
        let pad = |out: &mut String, n: usize| {
            for _ in 0..n {
                out.push_str("  ");
            }
        };
        match self {
            Value::Int(v) => {
                let _ = write!(out, "{v}");
            }
            Value::Num(v, d) => {
                if v.is_finite() {
                    let _ = write!(out, "{v:.d$}", d = *d);
                } else {
                    out.push_str("null");
                }
            }
            Value::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Arr(items) => {
                let scalar = items
                    .iter()
                    .all(|i| matches!(i, Value::Int(_) | Value::Num(..) | Value::Bool(_)));
                if items.is_empty() {
                    out.push_str("[]");
                } else if scalar {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        item.write(out, indent);
                    }
                    out.push(']');
                } else {
                    out.push_str("[\n");
                    for (i, item) in items.iter().enumerate() {
                        pad(out, indent + 1);
                        item.write(out, indent + 1);
                        out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                    }
                    pad(out, indent);
                    out.push(']');
                }
            }
            Value::Obj(fields) => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    pad(out, indent + 1);
                    let _ = write!(out, "\"{key}\": ");
                    value.write(out, indent + 1);
                    out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }
}

fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn median(sorted: &[u32]) -> u32 {
    sorted.get(sorted.len() / 2).copied().unwrap_or(0)
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RegionOverCap {
                region,
                limits,
                bytes,
                unsplittable,
            } => write!(
                f,
                "region {region} is over its hard limit ({}) at {} B{}",
                limits.join(", "),
                grouped(u64::from(*bytes)),
                if *unsplittable {
                    "; the cut search found no admissible plane to split it"
                } else {
                    ""
                }
            ),
            Self::RankRowTooWide {
                region,
                closure,
                width_bytes,
                limit_bytes,
            } => write!(
                f,
                "region {region} sees {closure} regions, so its PVS row needs {width_bytes} B against a {limit_bytes} B limit"
            ),
            Self::CollisionCompile { region, error } => {
                write!(f, "region {region} collision did not compile: {error}")
            }
            Self::RhoExceeded { rho, limit, path } => write!(
                f,
                "bytes per unit of travel {rho:.1} exceeds the drive limit {limit:.1} along regions {path:?}"
            ),
            Self::PoolExceeded {
                bytes,
                pool,
                region,
                position,
            } => write!(
                f,
                "resident requirement {} B exceeds the {} B page pool in region {region} near ({:.0}, {:.0}, {:.0})",
                grouped(*bytes),
                grouped(u64::from(*pool)),
                position[0],
                position[1],
                position[2]
            ),
        }
    }
}

impl StreamReport {
    pub fn build(project_name: &str, input: &PartitionInput, partition: &Partition) -> Self {
        let p = partition;
        let params = &p.params;
        let gate = &p.gate;
        let n = p.regions.len();
        let mut sizes: Vec<u32> = p.regions.iter().map(|r| r.counts.bytes()).collect();
        sizes.sort_unstable();
        let total: u64 = sizes.iter().map(|&b| u64::from(b)).sum();
        let need_bytes: Vec<u64> = p
            .closure
            .need
            .iter()
            .map(|s| set_bytes(input, &p.regions, s))
            .collect();
        let fills: Vec<(u32, &'static str)> = p
            .regions
            .iter()
            .map(|r| r.binding_fill_pct(params))
            .collect();
        let mut closure_sizes: Vec<u32> =
            p.closure.visible.iter().map(|v| v.len() as u32).collect();
        closure_sizes.sort_unstable();
        let faces: u64 = p.regions.iter().map(|r| u64::from(r.counts.faces)).sum();
        let single = n == 1;
        let passed = gate.passed();

        // ---- JSON ---------------------------------------------------------
        let regions_json: Vec<Value> = p
            .regions
            .iter()
            .map(|r| {
                let c = &r.counts;
                let v = &p.closure.visible[r.id as usize];
                Value::Obj(vec![
                    ("id", int(r.id)),
                    ("min", vec3(r.bounds.min)),
                    ("max", vec3(r.bounds.max)),
                    ("bytes", int(c.bytes())),
                    ("sectors", int(c.sectors())),
                    ("faces", int(c.faces)),
                    ("vertices", int(c.vertices)),
                    ("planes", int(c.planes)),
                    ("marks", int(c.marks)),
                    ("leaves", int(c.leaves)),
                    ("nodes", int(c.nodes)),
                    ("clip_nodes", int(c.clip_nodes)),
                    ("pvs_bytes", int(c.pvs_bytes)),
                    ("inline_texture_bytes", int(c.inline_texture_bytes)),
                    ("spawns", int(c.spawns)),
                    ("fill_pct", int(fills[r.id as usize].0)),
                    ("binding", text(fills[r.id as usize].1)),
                    ("visible", int(v.len())),
                    ("visible_ids", ids(v)),
                    (
                        "row_bytes",
                        int((v.len() as u32 * params.caps.leaves).div_ceil(8)),
                    ),
                    ("need_regions", int(p.closure.need[r.id as usize].count())),
                    ("need_bytes", int(need_bytes[r.id as usize])),
                    (
                        "neighbours",
                        ids(&p.graph.adjacency[r.id as usize]
                            .iter()
                            .map(|&(n, _)| n)
                            .collect::<Vec<_>>()),
                    ),
                    ("start_sector", int(p.layout.start_sector[r.id as usize])),
                    ("unsplittable", Value::Bool(r.unsplittable)),
                ])
            })
            .collect();
        let windows_json: Vec<Value> = gate
            .worst_windows
            .iter()
            .map(|w| {
                Value::Obj(vec![
                    ("path", ids(&w.path)),
                    ("bytes", int(w.bytes)),
                    ("window_units", Value::Num(w.distance, 1)),
                    ("rho", Value::Num(w.rho, 2)),
                ])
            })
            .collect();
        let peak_json = |peak: &Option<super::gate::PoolPeak>| match peak {
            None => Value::Bool(false),
            Some(pk) => Value::Obj(vec![
                ("horizon_units", Value::Num(pk.horizon, 1)),
                ("bytes", int(pk.bytes)),
                ("region", int(pk.region)),
                ("position", vec3(pk.position)),
                ("need_regions", int(pk.need_regions)),
                ("resident_regions", int(pk.lead_regions)),
            ]),
        };
        let json_value = Value::Obj(vec![
            ("report_version", int(REPORT_VERSION)),
            ("project", text(project_name)),
            (
                "verdict",
                Value::Obj(vec![
                    ("pass", Value::Bool(passed)),
                    (
                        "failures",
                        Value::Arr(gate.failures.iter().map(|f| text(f.to_string())).collect()),
                    ),
                ]),
            ),
            (
                "summary",
                Value::Obj(vec![
                    ("regions", int(n)),
                    ("single_region", Value::Bool(single)),
                    ("payload_bytes", int(total)),
                    ("payload_sectors", int(p.layout.total_sectors)),
                    ("region_bytes_min", int(sizes.first().copied().unwrap_or(0))),
                    ("region_bytes_median", int(median(&sizes))),
                    ("region_bytes_max", int(sizes.last().copied().unwrap_or(0))),
                    ("faces", int(faces)),
                    (
                        "inline_texture_bytes",
                        int(p
                            .regions
                            .iter()
                            .map(|r| u64::from(r.counts.inline_texture_bytes))
                            .sum::<u64>()),
                    ),
                    (
                        "pvs_bytes",
                        int(p
                            .regions
                            .iter()
                            .map(|r| u64::from(r.counts.pvs_bytes))
                            .sum::<u64>()),
                    ),
                    (
                        "regions_without_faces",
                        int(p.regions.iter().filter(|r| r.counts.faces == 0).count()),
                    ),
                    ("input_faces", int(p.total_input_faces)),
                    ("refine_passes", int(p.refine_passes)),
                    ("world_min", vec3(p.world_bounds.min)),
                    ("world_max", vec3(p.world_bounds.max)),
                    ("aperture_count", int(p.graph.apertures.len())),
                    ("surfaces_culled_unreachable", int(input.culled_unreachable)),
                    ("spawns", int(input.spawns.len())),
                    ("hooks", int(input.hooks.len())),
                    ("archetypes", int(input.archetypes.len())),
                    (
                        "start_region",
                        p.start_region.map_or(Value::Bool(false), int),
                    ),
                    ("unreachable_regions", ids(&gate.unreachable)),
                    ("dead_regions", int(gate.dead_regions)),
                ]),
            ),
            (
                "visibility",
                Value::Obj(vec![
                    (
                        "method",
                        text(match p.closure.source {
                            ClosureSource::Sampled => {
                                "sampled line of sight over brush solids (estimate)"
                            }
                            ClosureSource::PortalFlow => "the cook's own portal flow (measured)",
                        }),
                    ),
                    ("closure_max", int(gate.max_closure)),
                    ("closure_median", int(median(&closure_sizes))),
                    ("max_row_bytes", int(gate.max_row_bytes)),
                    (
                        "row_limit_bytes",
                        int(psx_bsp::pxbsp::PXBSP_MAX_VISIBILITY_BYTES),
                    ),
                    ("leaf_cap", int(gate.row_leaf_cap)),
                    ("rays_cast", int(p.closure.rays_cast)),
                    ("max_need_archetypes", int(gate.max_need_archetypes)),
                ]),
            ),
            (
                "drive_model",
                Value::Obj(vec![
                    ("run_speed_units_per_s", Value::Num(p.run_speed, 2)),
                    ("v_max_units_per_s", Value::Num(gate.v_max, 1)),
                    ("region_service_ms", Value::Num(gate.t_region_ms, 1)),
                    (
                        "region_service_pessimistic_ms",
                        Value::Num(gate.t_region_pessimistic_ms, 1),
                    ),
                    ("b_eff_bytes_per_s", Value::Num(gate.b_eff, 0)),
                    (
                        "b_eff_batched_bytes_per_s",
                        Value::Num(gate.b_eff_batched, 0),
                    ),
                    ("utilisation_pct", int(params.utilisation_pct)),
                    ("h_lead_units", Value::Num(gate.h_lead, 1)),
                    (
                        "h_lead_pessimistic_units",
                        Value::Num(gate.h_lead_pessimistic, 1),
                    ),
                ]),
            ),
            (
                "rho",
                Value::Obj(vec![
                    ("limit_bytes_per_unit", Value::Num(gate.rho_limit, 2)),
                    (
                        "limit_batched_bytes_per_unit",
                        Value::Num(gate.rho_limit_batched, 2),
                    ),
                    ("worst_bytes_per_unit", Value::Num(gate.rho_worst(), 2)),
                    ("edges_checked", int(gate.edges_checked)),
                    ("windows_over_limit", int(gate.windows_over_limit)),
                    ("windows_total", int(gate.windows_total)),
                    (
                        "gate_limit_bytes_per_unit",
                        Value::Num(gate.rho_gate_limit, 2),
                    ),
                    ("gate_uses_batched", Value::Bool(params.rho_batched)),
                    ("pass", Value::Bool(gate.rho_worst() <= gate.rho_gate_limit)),
                    (
                        "worst_edge",
                        gate.worst_edge.as_ref().map_or(Value::Bool(false), |e| {
                            Value::Obj(vec![
                                ("from", int(e.from)),
                                ("to", int(e.to)),
                                ("bytes", int(e.bytes)),
                                ("distance_units", Value::Num(e.distance, 1)),
                            ])
                        }),
                    ),
                    ("worst_windows", Value::Arr(windows_json)),
                ]),
            ),
            (
                "overrides",
                Value::Arr(params.overrides.iter().map(|o| text(o.clone())).collect()),
            ),
            (
                "pool",
                Value::Obj(vec![
                    ("pool_bytes", int(params.pool_bytes)),
                    ("skeleton_bytes", int(gate.skeleton_bytes)),
                    ("skeleton_measured", Value::Bool(gate.skeleton_measured)),
                    ("pool_available_bytes", int(gate.pool_available)),
                    ("home_pin_bytes", int(gate.home_pin_bytes)),
                    ("peak", peak_json(&gate.pool)),
                    ("peak_pessimistic", peak_json(&gate.pool_pessimistic)),
                ]),
            ),
            (
                "layout",
                Value::Obj(vec![
                    ("total_sectors", int(p.layout.total_sectors)),
                    ("cost", int(p.layout.cost)),
                    ("baseline_cost", int(p.layout.baseline_cost)),
                    ("edges_le_16", int(p.layout.class_counts[0])),
                    ("edges_le_128", int(p.layout.class_counts[1])),
                    ("edges_le_512", int(p.layout.class_counts[2])),
                    ("edges_gt_512", int(p.layout.class_counts[3])),
                    ("order", ids(&p.layout.order)),
                ]),
            ),
            ("regions", Value::Arr(regions_json)),
        ]);
        let mut json = String::new();
        json_value.write(&mut json, 0);
        json.push('\n');

        // ---- text ---------------------------------------------------------
        let mut t = String::new();
        let _ = writeln!(
            t,
            "Stream partition report v{REPORT_VERSION}: {project_name}"
        );
        let _ = writeln!(
            t,
            "Verdict: {} ({} failure(s))",
            if passed { "PASS" } else { "FAIL" },
            gate.failures.len()
        );
        for failure in gate.failures.iter().take(12) {
            let _ = writeln!(t, "  FAIL {failure}");
        }
        if gate.failures.len() > 12 {
            let mut kinds: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for failure in &gate.failures {
                *kinds.entry(failure.kind()).or_default() += 1;
            }
            let summary: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
            let _ = writeln!(
                t,
                "  ... {} more failures not listed here ({}); the JSON report lists all",
                gate.failures.len() - 12,
                summary.join(", ")
            );
        }
        if !params.overrides.is_empty() {
            let _ = writeln!(
                t,
                "Parameter overrides in effect: {}",
                params.overrides.join("; ")
            );
        }
        let _ = writeln!(
            t,
            "World: {:.0} x {:.0} x {:.0} units, {} input faces ({} surfaces culled as unreachable), {} spawns, {} hooks, {} aperture(s)",
            p.world_bounds.extent(0),
            p.world_bounds.extent(1),
            p.world_bounds.extent(2),
            p.total_input_faces,
            input.culled_unreachable,
            input.spawns.len(),
            input.hooks.len(),
            p.graph.apertures.len()
        );
        let _ = writeln!(
            t,
            "Regions: {}{}  payload {} B ({} sectors)  per region min/median/max {} / {} / {} B  refine passes {}",
            n,
            if single { " (single region, no cuts)" } else { "" },
            grouped(total),
            grouped(u64::from(p.layout.total_sectors)),
            grouped(u64::from(sizes.first().copied().unwrap_or(0))),
            grouped(u64::from(median(&sizes))),
            grouped(u64::from(sizes.last().copied().unwrap_or(0))),
            p.refine_passes
        );
        let fill_min = fills.iter().map(|f| f.0).min().unwrap_or(0);
        let fill_max = fills.iter().map(|f| f.0).max().unwrap_or(0);
        let inline: u64 = p
            .regions
            .iter()
            .map(|r| u64::from(r.counts.inline_texture_bytes))
            .sum();
        let pvs: u64 = p
            .regions
            .iter()
            .map(|r| u64::from(r.counts.pvs_bytes))
            .sum();
        let empty = p.regions.iter().filter(|r| r.counts.faces == 0).count();
        let _ = writeln!(
            t,
            "Payload split: {} B small textures duplicated inline, {} B dense PVS rows (upper bound); {} region(s) hold no faces (buried or open air)",
            grouped(inline),
            grouped(pvs),
            empty
        );
        let _ = writeln!(
            t,
            "Binding-cap fill: {fill_min}% to {fill_max}% (target at least {}%; hard cap {} B, slot caps f/v/n/l/m/c {}/{}/{}/{}/{}/{})",
            params.fill_target_pct,
            grouped(u64::from(params.region_hard_cap_bytes)),
            params.caps.faces,
            params.caps.vertices,
            params.caps.nodes,
            params.caps.leaves,
            params.caps.mark_surfaces,
            params.caps.clip_nodes
        );
        let _ = writeln!(
            t,
            "Visibility [{}]: closure max {} median {}, widest PVS row {} B of {} B (rank rows, leaf cap {})",
            match p.closure.source {
                ClosureSource::Sampled => format!(
                    "estimate: sampled line of sight, {} rays",
                    grouped(p.closure.rays_cast)
                ),
                ClosureSource::PortalFlow =>
                    "measured: the cook's portal flow".to_string(),
            },
            gate.max_closure,
            median(&closure_sizes),
            gate.max_row_bytes,
            psx_bsp::pxbsp::PXBSP_MAX_VISIBILITY_BYTES,
            gate.row_leaf_cap
        );
        let _ = writeln!(
            t,
            "Drive model: v_run {:.0} u/s [D], v_max {:.0} u/s (kappa {}% [E]); region service {:.0} ms ({:.0} ms pessimistic) [M]; B_eff {:.0} B/s, batched k={} {:.0} B/s; lead horizon {:.0} u ({:.0} u pessimistic), q={} [E]",
            p.run_speed,
            gate.v_max,
            params.speed_kappa_pct,
            gate.t_region_ms,
            gate.t_region_pessimistic_ms,
            gate.b_eff,
            params.batch_k,
            gate.b_eff_batched,
            gate.h_lead,
            gate.h_lead_pessimistic,
            params.lead_depth_q
        );
        let worst = gate.rho_worst();
        let _ = writeln!(
            t,
            "rho verdict: {}  worst {:.1} B/unit against limit {:.1} B/unit ({}; U {}% [E]; unbatched {:.1}, batched {:.1})  over {} directed edge(s)",
            if worst <= gate.rho_gate_limit { "PASS" } else { "FAIL" },
            worst,
            gate.rho_gate_limit,
            if params.rho_batched { "batched gate" } else { "unbatched gate, as in the design" },
            params.utilisation_pct,
            gate.rho_limit,
            gate.rho_limit_batched,
            gate.edges_checked
        );
        let _ = writeln!(
            t,
            "  {} of {} crossing windows exceed the limit",
            gate.windows_over_limit, gate.windows_total
        );
        for w in gate.worst_windows.iter().take(3) {
            let _ = writeln!(
                t,
                "  window {:?}: {} B over {:.0} u = {:.1} B/unit",
                w.path,
                grouped(w.bytes),
                w.distance,
                w.rho
            );
        }
        match &gate.pool {
            Some(pk) => {
                let _ = writeln!(
                    t,
                    "Pool verdict: {}  peak {} B of {} B available [{} B pool - 2 x {} B {} skeleton] (inline textures excluded: VRAM) in region {} (ball needs {} regions, with lead {}; home pin {} B)",
                    if pk.bytes <= gate.pool_available { "PASS" } else { "FAIL" },
                    grouped(pk.bytes),
                    grouped(gate.pool_available),
                    grouped(u64::from(params.pool_bytes)),
                    grouped(gate.skeleton_bytes),
                    if gate.skeleton_measured { "measured" } else { "estimated" },
                    pk.region,
                    pk.need_regions,
                    pk.lead_regions,
                    grouped(gate.home_pin_bytes)
                );
            }
            None => {
                let _ = writeln!(t, "Pool verdict: not evaluated (no regions)");
            }
        }
        if let Some(pk) = &gate.pool_pessimistic {
            let _ = writeln!(
                t,
                "  pessimistic lead ({:.0} u): peak {} B in region {}",
                pk.horizon,
                grouped(pk.bytes),
                pk.region
            );
        }
        let _ = writeln!(
            t,
            "Layout: {} sectors; weighted distance {} (cut-tree order {}); edges by sector gap <=16 {}, <={} {}, <={} {}, more {}",
            grouped(u64::from(p.layout.total_sectors)),
            grouped(p.layout.cost),
            grouped(p.layout.baseline_cost),
            p.layout.class_counts[0],
            SEEK_CLASSES[1],
            p.layout.class_counts[1],
            SEEK_CLASSES[2],
            p.layout.class_counts[2],
            p.layout.class_counts[3]
        );
        let _ = writeln!(
            t,
            "Dead regions (no walkable floor: sky layers, roofs, buried cells): {}",
            gate.dead_regions
        );
        if !gate.unreachable.is_empty() {
            let shown: Vec<u32> = gate.unreachable.iter().copied().take(12).collect();
            let _ = writeln!(
                t,
                "Walkable regions the player start cannot reach over open apertures: {} {:?}{}",
                gate.unreachable.len(),
                shown,
                if gate.unreachable.len() > shown.len() {
                    " ..."
                } else {
                    ""
                }
            );
        }
        let _ = writeln!(
            t,
            "Archetypes in the largest closure: {} (pack bytes unmeasured, counted as 0)",
            gate.max_need_archetypes
        );

        // Regions to look at.
        let mut by_need: Vec<usize> = (0..n).collect();
        by_need.sort_by(|&a, &b| need_bytes[b].cmp(&need_bytes[a]).then(a.cmp(&b)));
        let mut by_fill: Vec<usize> = (0..n).collect();
        by_fill.sort_by(|&a, &b| fills[a].0.cmp(&fills[b].0).then(a.cmp(&b)));
        let _ = writeln!(
            t,
            "Look at: largest requirement {}; lowest fill {}",
            by_need
                .iter()
                .take(3)
                .map(|&r| format!("#{r} ({} B)", grouped(need_bytes[r])))
                .collect::<Vec<_>>()
                .join(", "),
            by_fill
                .iter()
                .take(3)
                .map(|&r| format!("#{r} ({}%)", fills[r].0))
                .collect::<Vec<_>>()
                .join(", ")
        );

        let shown = if n <= 24 { n } else { 12 };
        let _ = writeln!(
            t,
            "{} (id: bytes, sectors, faces/leaves/clipnodes, fill, sees, needs B):",
            if shown == n {
                "Regions"
            } else {
                "Heaviest regions"
            }
        );
        let mut order: Vec<usize> = (0..n).collect();
        if shown < n {
            order.sort_by(|&a, &b| {
                p.regions[b]
                    .counts
                    .bytes()
                    .cmp(&p.regions[a].counts.bytes())
                    .then(a.cmp(&b))
            });
        }
        for &r in order.iter().take(shown) {
            let reg = &p.regions[r];
            let c = &reg.counts;
            let _ = writeln!(
                t,
                "  #{r}: {} B, {} sec, {}/{}/{}, {}% {}, sees {}, needs {} B",
                grouped(u64::from(c.bytes())),
                c.sectors(),
                c.faces,
                c.leaves,
                c.clip_nodes,
                fills[r].0,
                fills[r].1,
                p.closure.visible[r].len(),
                grouped(need_bytes[r])
            );
        }
        let _ = writeln!(
            t,
            "Notes: cuts are axis-aligned; visibility is {}; archetype pack bytes and install CPU are unmeasured; collision is costed with the shipping compiler on whole brushes; labels [M] measured, [D] derived, [E] estimate.",
            match p.closure.source {
                ClosureSource::Sampled => "estimated, not portal flow",
                ClosureSource::PortalFlow => "the cook's own portal flow, not an estimate",
            }
        );

        Self {
            passed,
            text: t,
            json,
        }
    }
}
