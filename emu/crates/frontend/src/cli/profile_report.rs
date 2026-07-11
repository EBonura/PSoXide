//! Guest-runtime profile printing for `launch --dump-guest-profile`:
//! GTE usage, task/stage/counter summaries, render breakdown, pacing.

use emulator_core::{telemetry, GteProfileSnapshot};

use super::counter_total;

const NTSC_CPU_CYCLES_PER_VBLANK: u64 = 33_868_800 / 60;
const GUEST_RENDER_BREAKDOWN_STAGES: &[(u16, &str)] = &[
    (telemetry::stage::SKY, "sky"),
    (telemetry::stage::FAR_VISTA, "far vista"),
    (telemetry::stage::ROOM, "room"),
    (telemetry::stage::ENTITY_MARKERS, "markers"),
    (telemetry::stage::IMAGE_PROPS, "image props"),
    (telemetry::stage::MODEL_INSTANCES, "models"),
    (telemetry::stage::PLAYER, "player"),
    (telemetry::stage::EQUIPMENT, "equipment"),
    (telemetry::stage::WORLD_FLUSH, "flush/sort"),
    (telemetry::stage::OT_SUBMIT, "ot submit"),
    (telemetry::stage::OT_WAIT, "ot wait"),
];

pub(super) fn print_gte_profile(
    before: &GteProfileSnapshot,
    after: &GteProfileSnapshot,
    summary: &telemetry::GuestTelemetrySummary,
) {
    let ops = after.ops.saturating_sub(before.ops);
    let cycles = after
        .estimated_cycles
        .saturating_sub(before.estimated_cycles);
    let guest_frames = summary.frames.max(1);
    println!("gte_profile:");
    println!(
        "  ops={}  per_guest_frame={:.0}",
        ops,
        ops as f64 / guest_frames as f64
    );
    println!(
        "  estimated_cycles={}  per_guest_frame={:.0}",
        cycles,
        cycles as f64 / guest_frames as f64
    );
    // GTE load against the render budget. NOTE: this emulator does not charge
    // GTE op latency to the CPU frametime, so `estimated_cycles` is the real
    // hardware GTE load; the rest of the per-frame budget is headroom available
    // for offloading CPU geometry math (matrix-vector, perspective divide,
    // cross-product backface) onto the otherwise-idle GTE.
    let visual_frames = counter_total(summary, telemetry::counter::VISUAL_FRAMES).max(1);
    let interval_total = counter_total(summary, telemetry::counter::VISUAL_INTERVAL_VBLANKS);
    let per_render = cycles / visual_frames;
    if interval_total > 0 && summary.frames > 0 {
        let vblanks = interval_total as f64 / summary.frames as f64;
        let budget = vblanks * NTSC_CPU_CYCLES_PER_VBLANK as f64;
        let pct = if budget > 0.0 {
            100.0 * per_render as f64 / budget
        } else {
            0.0
        };
        println!(
            "  estimated_cycles_per_visual_frame={}  ({:.1}% of the {:.0}-cycle render budget, ~{:.1}% GTE headroom)",
            per_render,
            pct,
            budget,
            (100.0 - pct).max(0.0)
        );
    }
    println!("  opcodes:");
    for opcode in 0..after.opcode_counts.len() {
        let count = after.opcode_counts[opcode].saturating_sub(before.opcode_counts[opcode]);
        if count == 0 {
            continue;
        }
        println!(
            "    0x{opcode:02x} {:<6} count={:<10} per_guest_frame={:.0}",
            gte_opcode_name(opcode as u8),
            count,
            count as f64 / guest_frames as f64
        );
    }
}

fn gte_opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x01 => "RTPS",
        0x06 => "NCLIP",
        0x0c => "OP",
        0x10 => "DPCS",
        0x11 => "INTPL",
        0x12 => "MVMVA",
        0x13 => "NCDS",
        0x14 => "CDP",
        0x16 => "NCDT",
        0x1b => "NCCS",
        0x1c => "CC",
        0x1e => "NCS",
        0x20 => "NCT",
        0x28 => "SQR",
        0x29 => "DCPL",
        0x2a => "DPCT",
        0x2d => "AVSZ3",
        0x2e => "AVSZ4",
        0x30 => "RTPT",
        0x3d => "GPF",
        0x3e => "GPL",
        0x3f => "NCCT",
        _ => "UNKNOWN",
    }
}

pub(super) fn print_guest_profile(summary: &telemetry::GuestTelemetrySummary) {
    if !summary.has_data() {
        println!("guest_profile=empty");
        return;
    }

    let frames = summary.frames.max(1) as f32;
    println!("guest_profile_frames={}", summary.frames);
    println!("guest_profile_frame_meaning=frame_begin_markers");
    print_guest_pacing_profile(summary);
    print_guest_render_breakdown(summary);
    println!("guest_profile_tasks:");
    for id in 0..telemetry::TASK_COUNT {
        let cycles = summary.task_cycles[id];
        if cycles == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_hit={:.0} max_hit={} hits={}",
            telemetry::task_name(id as u16),
            cycles,
            cycles as f32 / (summary.task_hits[id].max(1) as f32),
            summary.task_max_cycles[id],
            summary.task_hits[id],
        );
    }
    println!("guest_profile_stages:");
    for id in 1..telemetry::STAGE_COUNT {
        let cycles = summary.stage_cycles[id];
        if cycles == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_frame={:.0} per_hit={:.0} max_hit={} hits={}",
            telemetry::stage_name(id as u16),
            cycles,
            cycles as f32 / frames,
            cycles as f32 / (summary.stage_hits[id].max(1) as f32),
            summary.stage_max_cycles[id],
            summary.stage_hits[id],
        );
    }
    println!("guest_profile_counters:");
    for id in 1..telemetry::COUNTER_COUNT {
        let value = summary.counters[id];
        if value == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_frame={:.0} latest={}",
            telemetry::counter_name(id as u16),
            value,
            value as f32 / frames,
            summary.counter_latest_values[id],
        );
    }
}

fn print_guest_render_breakdown(summary: &telemetry::GuestTelemetrySummary) {
    let render_cycles = summary.stage_cycles[telemetry::stage::RENDER as usize];
    if render_cycles == 0 {
        println!("guest_profile_render_breakdown=not_emitted");
        return;
    }

    let render_hits = summary.stage_hits[telemetry::stage::RENDER as usize].max(1);
    let mut accounted = 0u64;
    println!("guest_profile_render_breakdown:");
    for &(stage_id, label) in GUEST_RENDER_BREAKDOWN_STAGES {
        let cycles = summary.stage_cycles[stage_id as usize];
        if cycles == 0 {
            continue;
        }
        accounted = accounted.saturating_add(cycles);
        println!(
            "  {:<18} pct={:>5.1} per_render={:.0} cycles={}",
            label,
            percent_u64(cycles, render_cycles),
            cycles as f64 / render_hits as f64,
            cycles,
        );
    }

    let other = render_cycles.saturating_sub(accounted);
    if other > render_cycles / 200 {
        println!(
            "  {:<18} pct={:>5.1} per_render={:.0} cycles={}",
            "other",
            percent_u64(other, render_cycles),
            other as f64 / render_hits as f64,
            other,
        );
    }
}

fn percent_u64(part: u64, total: u64) -> f64 {
    (part as f64) * 100.0 / total.max(1) as f64
}

fn print_guest_pacing_profile(summary: &telemetry::GuestTelemetrySummary) {
    let pacing_ids = [
        telemetry::counter::SIM_TICKS,
        telemetry::counter::VISUAL_FRAMES,
        telemetry::counter::VISUAL_SKIPPED_VBLANKS,
        telemetry::counter::VISUAL_DEADLINE_MISSES,
        telemetry::counter::VISUAL_INTERVAL_VBLANKS,
        telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS,
    ];
    let has_pacing = pacing_ids
        .iter()
        .any(|&id| counter_total(summary, id) > 0 || counter_max_value(summary, id) > 0);
    if !has_pacing {
        println!("guest_profile_pacing=not_emitted");
        return;
    }

    let sim_ticks = counter_total(summary, telemetry::counter::SIM_TICKS);
    let visual_frames = counter_total(summary, telemetry::counter::VISUAL_FRAMES);
    let skipped = counter_total(summary, telemetry::counter::VISUAL_SKIPPED_VBLANKS);
    let misses = counter_total(summary, telemetry::counter::VISUAL_DEADLINE_MISSES);
    let interval_total = counter_total(summary, telemetry::counter::VISUAL_INTERVAL_VBLANKS);
    let max_lateness = counter_max_value(summary, telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS);
    let interval = if summary.frames > 0 && interval_total > 0 {
        Some(interval_total as f64 / summary.frames as f64)
    } else {
        None
    };
    let update_per_sim = div_u64(
        summary.stage_cycles[telemetry::stage::UPDATE as usize],
        sim_ticks,
    );
    let render_per_visual = div_u64(
        summary.stage_cycles[telemetry::stage::RENDER as usize],
        visual_frames,
    );
    let visual_budget = interval.map(|vblanks| vblanks * NTSC_CPU_CYCLES_PER_VBLANK as f64);

    println!("guest_profile_pacing:");
    println!("  sim_ticks={}", fmt_known_u64(sim_ticks));
    println!("  visual_frames={}", fmt_known_u64(visual_frames));
    println!("  visual_skipped_vblanks={}", skipped);
    println!("  visual_deadline_misses={}", misses);
    println!("  visual_interval_vblanks={}", fmt_optional_f64(interval));
    println!("  visual_max_lateness_vblanks={}", max_lateness);
    println!(
        "  update_cycles_per_sim_tick={}",
        fmt_optional_f64(update_per_sim)
    );
    println!(
        "  render_cycles_per_visual_frame={}",
        fmt_optional_f64(render_per_visual)
    );
    println!(
        "  visual_budget_cycles={}  vblanks={}  cycles_per_vblank={}",
        fmt_optional_f64(visual_budget),
        fmt_optional_f64_2(interval),
        NTSC_CPU_CYCLES_PER_VBLANK
    );
    println!(
        "  visual_budget_status={}",
        visual_budget_status(render_per_visual, visual_budget)
    );
    println!(
        "  cadence_status={}",
        cadence_status(interval, misses, max_lateness)
    );
}

fn counter_max_value(summary: &telemetry::GuestTelemetrySummary, id: u16) -> u32 {
    summary
        .counter_max_values
        .get(id as usize)
        .copied()
        .unwrap_or_default()
}

fn div_u64(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn fmt_known_u64(value: u64) -> String {
    if value == 0 {
        "unknown".to_string()
    } else {
        value.to_string()
    }
}

fn fmt_optional_f64(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.0}"),
        None => "unknown".to_string(),
    }
}

fn fmt_optional_f64_2(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.2}"),
        None => "unknown".to_string(),
    }
}

fn visual_budget_status(
    render_per_visual: Option<f64>,
    visual_budget: Option<f64>,
) -> &'static str {
    match (render_per_visual, visual_budget) {
        (Some(cycles), Some(budget)) if cycles <= budget => "pass",
        (Some(_), Some(_)) => "fail",
        _ => "unknown",
    }
}

fn cadence_status(interval: Option<f64>, misses: u64, max_lateness: u32) -> &'static str {
    match interval {
        Some(_) if misses == 0 && max_lateness == 0 => "steady",
        Some(_) => "missed_or_late",
        None => "unknown",
    }
}
