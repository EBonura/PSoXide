#!/usr/bin/env python3
"""Render a per-vblank work chart from a `frontend launch --profile-log` CSV.

The profile log writes one row per guest vblank (a `frame_begin` marker fires
each sim tick, which advances once per display vblank). Column `frame_cycles`
is the total guest CPU cycles spent in that vblank; per-stage columns break it
down.

# How the bars are partitioned (why there is no vague "other")

Every cycle in a bar is attributed to a named band. The partition is anchored
on three totals that the profiler guarantees are non-overlapping and together
cover the whole vblank:

    frame_cycles  =  update  +  render  +  present  +  (frame remainder)

`update` is the gameplay sim, `render` is the whole `Scene::render`, `present`
is the vblank-wait + page-flip. `render` is then split into its leaf
sub-stages (room, sky, player, props, world_flush, ot_submit, ot_wait, ...);
whatever those leaves do not cover is the **render remainder** (render-side
camera/view setup, mid-frame VRAM uploads, glue). The two remainders are
*derived* from the totals, never a sum of guessed columns, so the bars always
total `frame_cycles` exactly and a stage reused across passes (e.g. `camera`,
which runs in both the render pass and the sim pass) is never double-counted:
its render part stays inside `render`, its sim part inside `update`.

`ot_submit` is the DMA *kick* (CPU); `ot_wait` is the CPU blocked on the OT
DMA/GPU walk = GPU draw cost. `ot_wait` is ~0 under the emulator, which does
not model GPU time; it is meaningful only on hardware.

Usage:
    python3 tools/vblank_chart.py --in /tmp/demo10-vblank.csv --out /tmp/x.html
    python3 tools/vblank_chart.py --in run.csv --out run.html --title "demo10"

The output is a single self-contained HTML file (scroll = zoom, drag = pan,
double-click = reset) with a full legend below the chart. Summary stats are
also printed to stdout.
"""
from __future__ import annotations

import argparse
import csv
import json
import sys

# NTSC: 33.8688 MHz CPU / 60 Hz = one display field.
ONE_VBLANK_CYCLES = 564480
CPU_HZ = 33_868_800  # NTSC R3000A clock, for cycles -> ms.

# Stacked leaf bands, bottom -> top: (key, label, color, [columns to sum],
# description). `update` is the sim half; the rest are leaf children of the
# `render` stage. Parents (room/player/models/props) carry only their own
# column here -- their sub-stages show in the tooltip, and anything a parent
# does not cover falls into the derived render remainder, never double-counted.
BANDS = [
    ("update", "sim: update", "#1f6feb", ["update"],
     "Gameplay sim for this tick: input, collision, character motor, stream "
     "residency, and the sim-side camera. Runs every vblank (the 60 Hz sim clock)."),
    ("frame_clear", "frame clear", "#6e7681", ["frame_clear"],
     "Clear the back-buffer before drawing the scene."),
    ("room", "room", "#2f81f7", ["room"],
     "Cooked room geometry: visible-cell select, GTE vertex projection, surface "
     "cull/light, packet build. Per-phase room_* detail is in the tooltip."),
    ("sky", "sky", "#58a6ff", ["sky"],
     "Cooked sky / cyclorama backdrop."),
    ("far_vista", "far vista", "#79c0ff", ["far_vista"],
     "Distant far-vista ring rendered behind the room."),
    ("props", "props (image/box)", "#2ea043", ["image_props"],
     "Editor-authored image/card and box props. Box/card detail is in the tooltip."),
    ("models", "models", "#3fb950", ["model_instances"],
     "Placed model-instance rendering (whole-model bounds cull + draw)."),
    ("player", "player", "#56d364", ["player"],
     "Player model: joint/skinning, GTE projection, face cull + packet build. "
     "joints/project/faces detail is in the tooltip."),
    ("equipment", "equipment", "#2dd4bf", ["equipment"],
     "Player-attached equipment / weapon render + hit-volume evaluation."),
    ("world_flush", "world flush/sort", "#db61a2", ["world_flush"],
     "Deferred world-command depth sort + ordering-table insertion (painter sort)."),
    ("ot_submit", "ot submit (kick)", "#e3a008", ["ot_submit"],
     "Ordering-table DMA kick: set registers and start the linked-list DMA. "
     "CPU-side, tiny."),
    ("ot_wait", "gpu draw (ot wait)", "#e3633a", ["ot_wait"],
     "Block on the OT DMA/GPU walk = GPU draw cost. ~0 under the emulator (GPU "
     "time is not modeled); meaningful only on real hardware."),
]
# Derived remainder after the render leaves (= render - sum(render leaves)).
RENDER_OTHER = ("render: camera/view + glue", "#768390",
                "Render time not inside a named sub-stage: render-side camera / "
                "view-matrix setup, mid-frame VRAM uploads, portal visibility, glue.")
# Real `present` column, placed after the render block.
PRESENT_BAND = ("present (wait/swap)", "#8957e5",
                "Wait for the next vblank + draw_sync + framebuffer page-flip "
                "(makes the drawn buffer visible). Mostly idle spin when the "
                "render finished inside its budget.")
# Derived frame remainder (= frame_cycles - update - render - present).
FRAME_OTHER = ("idle / loop (vblank wait)", "#373e47",
               "Whatever is left of the vblank. On sim-only vblanks this is the "
               "CPU idling until the next vblank after the sim finished -- the "
               "slack a render-spread could fill. On render vblanks it is just "
               "main-loop glue (pad poll, scheduler, telemetry markers).")

# Sub-stage breakdowns surfaced in the tooltip only (NOT stacked), grouped under
# the parent band they belong to. Shown when non-zero.
DETAIL = [
    ("room sub", ["room_visible_list", "room_cell_select", "room_project",
                  "room_depth_prep", "room_surface_draw"]),
    ("player sub", ["player_bounds", "player_draw", "textured_model_joints",
                    "textured_model_project", "textured_model_faces"]),
    ("props sub", ["box_props", "box_prop_debris", "box_prop_shards", "image_cards"]),
    ("models sub", ["model_bounds", "model_draw"]),
    ("cross-pass", ["camera", "portal_visibility", "active_room_window",
                    "room_surface_cache", "vram_upload", "cd_room_chunk_load"]),
]


def col(header, name):
    try:
        return header.index(name)
    except ValueError:
        return None


def num(row, idx):
    if idx is None or idx >= len(row):
        return 0
    try:
        return int(row[idx])
    except (ValueError, TypeError):
        try:
            return int(float(row[idx]))
        except (ValueError, TypeError):
            return 0


def percentile(sorted_vals, p):
    if not sorted_vals:
        return 0
    i = int((len(sorted_vals) - 1) * p / 100.0)
    return sorted_vals[i]


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--in", dest="inp", required=True, help="profile-log CSV")
    ap.add_argument("--out", dest="out", required=True, help="output HTML")
    ap.add_argument("--title", default=None, help="chart title")
    ap.add_argument("--budget", type=int, default=ONE_VBLANK_CYCLES,
                    help="one-vblank cycle budget (default NTSC 564480)")
    args = ap.parse_args()

    with open(args.inp, newline="") as f:
        reader = csv.reader(f)
        header = next(reader)
        rows = [r for r in reader if r]

    idx_fc = col(header, "frame_cycles")
    idx_render = col(header, "render")
    idx_present = col(header, "present")
    idx_miss = col(header, "visual_deadline_misses")
    idx_room = col(header, "current_room")
    if idx_fc is None:
        sys.exit("error: CSV has no 'frame_cycles' column -- is this a profile-log?")

    # Resolve band columns once.
    band_idx = [(key, [col(header, c) for c in cols]) for (key, _l, _c, cols, _d) in BANDS]
    render_leaf_keys = [b[0] for b in BANDS[1:]]  # everything except `update`
    detail_idx = [(group, [(c, col(header, c)) for c in cols]) for (group, cols) in DETAIL]

    # Stack order = BANDS (12) + render_other + present + frame_other. Reduce
    # every band to a uniform (label, color, desc) so the three arrays come from
    # one pass instead of indexing two different tuple shapes by hand.
    band_meta = [(b[1], b[2], b[4]) for b in BANDS] + [RENDER_OTHER, PRESENT_BAND, FRAME_OTHER]
    labels = [m[0] for m in band_meta]
    colors = [m[1] for m in band_meta]
    descs = [m[2] for m in band_meta]

    bars = []
    render_cyc, sim_cyc = [], []
    misses = 0
    frame_idx = -1  # 30fps-frame group index; bumps on each render vblank
    for r in rows:
        fc = num(r, idx_fc)
        render = num(r, idx_render)
        present_v = num(r, idx_present)
        vals = {key: sum(num(r, i) for i in idxs) for (key, idxs) in band_idx}
        render_leaf_sum = sum(vals[k] for k in render_leaf_keys)
        render_other = max(0, render - render_leaf_sum)
        frame_other = max(0, fc - vals["update"] - render - present_v)
        stacks = [vals[b[0]] for b in BANDS] + [render_other, present_v, frame_other]

        is_render = render > 0
        if is_render:
            frame_idx += 1
        detail = {}
        for (group, cols) in detail_idx:
            for (name, i) in cols:
                v = num(r, i)
                if v > 0:
                    detail[name] = v
        bars.append({
            "s": stacks, "fc": fc, "r": 1 if is_render else 0, "g": frame_idx,
            "m": num(r, idx_miss), "room": num(r, idx_room), "t": detail,
        })
        (render_cyc if is_render else sim_cyc).append(fc)
        misses += num(r, idx_miss)

    render_cyc.sort()
    sim_cyc.sort()
    budget = args.budget
    r_avg = sum(render_cyc) // max(1, len(render_cyc))
    s_avg = sum(sim_cyc) // max(1, len(sim_cyc))
    overall_avg = (sum(render_cyc) + sum(sim_cyc)) // max(1, len(bars))

    title = args.title or f"per-vblank work - {args.inp.split('/')[-1]}"
    stats = {
        "vblanks": len(bars),
        "render_vblanks": len(render_cyc),
        "sim_vblanks": len(sim_cyc),
        "render_avg": r_avg,
        "render_p50": percentile(render_cyc, 50),
        "render_max": render_cyc[-1] if render_cyc else 0,
        "render_over_budget": sum(1 for c in render_cyc if c > budget),
        "sim_avg": s_avg,
        "sim_p50": percentile(sim_cyc, 50),
        "overall_avg": overall_avg,
        "spread_target": overall_avg,
        "misses": misses,
        "budget": budget,
        "budget2": budget * 2,
    }

    # y-axis cap: keep the steady state readable; clip rare streaming stalls.
    cap = max(budget * 2, int(percentile(sorted(b["fc"] for b in bars), 99) * 1.1))

    payload = {"title": title, "labels": labels, "colors": colors, "descs": descs,
               "bars": bars, "stats": stats, "cap": cap, "hz": CPU_HZ}
    html = HTML_TEMPLATE.replace("__DATA__", json.dumps(payload, separators=(",", ":")))
    with open(args.out, "w") as f:
        f.write(html)

    # Console summary.
    def pc(v):
        return f"{v:>10,} ({v / budget * 100:5.1f}% of 1 vblank)"
    print(f"wrote {args.out}  ({len(bars)} vblanks)")
    print(f"  render vblanks : {len(render_cyc):>4}  avg {pc(r_avg)}  "
          f"p50 {pc(stats['render_p50'])}  over-budget {stats['render_over_budget']}/{len(render_cyc)}")
    print(f"  sim-only       : {len(sim_cyc):>4}  avg {pc(s_avg)}  "
          f"p50 {pc(stats['sim_p50'])}")
    print(f"  overall avg    : {pc(overall_avg)}   <- perfectly-spread target")
    print(f"  deadline misses: {misses}")


HTML_TEMPLATE = r"""<!DOCTYPE html><html><head><meta charset="utf-8"><title>per-vblank work</title>
<style>
 body{margin:0;background:#0d1117;color:#c9d1d9;font:13px/1.45 -apple-system,Segoe UI,Roboto,sans-serif}
 #hdr{padding:12px 16px;border-bottom:1px solid #21262d}
 h1{margin:0 0 6px;font-size:15px;font-weight:600}
 .stat{display:inline-block;margin:0 16px 0 0}.stat b{color:#58a6ff}
 #lg{padding:8px 16px;font-size:12px;color:#8b949e}
 .sw{display:inline-block;width:11px;height:11px;border-radius:2px;margin:0 4px 0 14px;vertical-align:-1px}
 #wrap{position:relative}#c{display:block;width:100%}
 #tip{position:absolute;pointer-events:none;background:#161b22;border:1px solid #30363d;border-radius:6px;
   padding:7px 10px;font-size:12px;display:none;box-shadow:0 4px 16px #000a;z-index:5;white-space:nowrap}
 #tip table{border-collapse:collapse}#tip td{padding:0 6px 0 0}#tip td.n{text-align:right;color:#c9d1d9}
 #tip .hd{color:#8b949e;font-size:11px;text-transform:uppercase;letter-spacing:.04em;padding-top:4px}
 #ft{padding:6px 16px;color:#6e7681;font-size:12px}
 #legend{padding:10px 16px 24px;border-top:1px solid #21262d}
 #legend h2{font-size:12px;font-weight:600;color:#8b949e;text-transform:uppercase;letter-spacing:.04em;margin:0 0 8px}
 .lrow{display:flex;align-items:flex-start;gap:8px;margin:0 0 6px;max-width:1000px}
 .lsw{flex:0 0 auto;width:12px;height:12px;border-radius:2px;margin-top:2px}
 .lname{flex:0 0 180px;font-weight:600;color:#c9d1d9}
 .ldesc{flex:1 1 auto;color:#8b949e}
</style></head><body>
<div id="hdr"><h1 id="t"></h1><div id="s"></div></div>
<div id="lg"></div>
<div id="wrap"><canvas id="c"></canvas><div id="tip"></div></div>
<div id="ft"><span style="color:#a8b1bb">&middot; &middot; &middot;</span> dotted line = 1 vblank (16.67ms, the 60fps budget). Each bar is one sim tick; a render bar + the next sim bar = one 30fps frame, so the <b>pair</b> must fit 2&times; the line. &nbsp;&middot;&nbsp; <span style="color:#f85149">red baseline tick</span> = 30fps slot missed (render vb + sim vb &gt; 2 vblanks) &nbsp;&middot;&nbsp; <span style="color:#f85149">red top tick</span> = off-scale stall<br><span style="color:#f0c674">&#9650;</span> = frame drawn here (render vblank) &nbsp;&middot;&nbsp; shaded bands group each render+sim pair into one 30fps frame &nbsp;&middot;&nbsp; scroll = zoom &middot; drag = pan &middot; double-click = reset</div>
<div id="legend"><h2>phases (bottom &rarr; top of each bar)</h2><div id="legbody"></div></div>
<script>
const D=__DATA__;
const c=document.getElementById('c'),ctx=c.getContext('2d'),tip=document.getElementById('tip');
const bars=D.bars,labels=D.labels,colors=D.colors,descs=D.descs,st=D.stats,cap=D.cap,budget=st.budget;
document.getElementById('t').textContent=D.title;
const fmt=n=>n.toLocaleString();
const ms=cyc=>(cyc/D.hz*1000).toFixed(2);
const vbl=cyc=>(cyc/budget).toFixed(2);
const pctb=cyc=>(cyc/budget*100).toFixed(0);
document.getElementById('s').innerHTML=
 `<span class=stat>vblanks <b>${st.vblanks}</b></span>`+
 `<span class=stat>render <b>${st.render_vblanks}</b> avg <b>${ms(st.render_avg)}ms</b> (${pctb(st.render_avg)}% of 1vb), ${st.render_over_budget} over 1vb</span>`+
 `<span class=stat>sim-only <b>${st.sim_vblanks}</b> avg <b>${ms(st.sim_avg)}ms</b> (${pctb(st.sim_avg)}%)</span>`+
 `<span class=stat>deadline misses <b>${st.misses}</b></span>`+
 `<span class=stat>spread target <b>${ms(st.spread_target)}ms</b>/vb (${pctb(st.spread_target)}%)</span>`+
 `<span class=stat style="color:#6e7681">1 vblank = ${ms(budget)}ms &middot; 60 Hz sim / 30 Hz render</span>`;
// compact top legend (color key) + detailed legend below the chart
let lg='';for(let i=0;i<labels.length;i++)lg+='<span class="sw" style="background:'+colors[i]+'"></span>'+labels[i];
document.getElementById('lg').innerHTML=lg;
let lb='';for(let i=0;i<labels.length;i++)lb+=
  `<div class=lrow><span class=lsw style="background:${colors[i]}"></span>`+
  `<span class=lname>${labels[i]}</span><span class=ldesc>${descs[i]}</span></div>`;
document.getElementById('legbody').innerHTML=lb;

let view0=0,view1=bars.length,drag=false,dragX=0,dragV0=0,dragV1=0;
function resize(){c.width=c.clientWidth*devicePixelRatio;c.height=420*devicePixelRatio;c.style.height='420px';ctx.setTransform(devicePixelRatio,0,0,devicePixelRatio,0,0);draw();}
addEventListener('resize',resize);
const W=()=>c.clientWidth,H=()=>420,PADB=26,PADT=8;
function draw(){
 const w=W(),h=H(),plot=h-PADB-PADT;
 ctx.clearRect(0,0,w,h);
 const n=view1-view0,bw=w/n;
 const yMax=cap;
 const y=v=>PADT+plot-(v/yMax)*plot;
 // 30fps-frame grouping: tint alternate frames (a render vblank + the sim
 // vblank(s) until the next render) so the 60 Hz-sim / 30 Hz-render cadence and
 // the render+sim pairing read at a glance.
 for(let i=Math.floor(view0);i<Math.ceil(view1);i++){
   if(i<0||i>=bars.length)continue;
   if(bars[i].g%2===0){ctx.fillStyle='rgba(120,160,255,0.05)';ctx.fillRect((i-view0)*bw,PADT,bw,plot);}
 }
 // bars first, then the budget line ON TOP so the line and its label are never
 // hidden behind a tall bar.
 for(let i=Math.floor(view0);i<Math.ceil(view1);i++){
   if(i<0||i>=bars.length)continue;
   const b=bars[i],x=(i-view0)*bw;
   let acc=0;
   for(let s=0;s<b.s.length;s++){
     const v=b.s[s];if(v<=0)continue;
     const y0=y(acc),y1=y(acc+v);
     ctx.fillStyle=colors[s];
     ctx.fillRect(x+0.5,y1,Math.max(1,bw-1),Math.max(0,y0-y1));
     acc+=v;
   }
   if(b.fc>yMax){ctx.fillStyle='#f85149';ctx.fillRect(x+0.5,PADT,Math.max(1,bw-1),3);}
   if(b.m>0){ctx.fillStyle='#f85149';const mw=Math.min(Math.max(1,bw-1),7);ctx.fillRect(x+(bw-mw)/2,PADT+plot+1,mw,4);}
 }
 // 1-vblank budget reference: a dotted line drawn over the bars. The wordy
 // explanation lives in the footer, not on the plot.
 if(budget<=yMax){
   ctx.strokeStyle='#a8b1bb';ctx.lineWidth=1;ctx.setLineDash([4,4]);
   ctx.beginPath();ctx.moveTo(0,y(budget));ctx.lineTo(w,y(budget));ctx.stroke();
   ctx.setLineDash([]);
 }
 // "frame drawn" caret under each render vblank (where the 30 Hz render fires).
 // Sim-only vblanks get none, so it is clear exactly when a frame is drawn.
 // Skipped when bars are too thin to read.
 if(bw>=5){for(let i=Math.floor(view0);i<Math.ceil(view1);i++){
   if(i<0||i>=bars.length||!bars[i].r)continue;
   const cx=(i-view0)*bw+bw/2,by=PADT+plot+6;
   ctx.fillStyle='#f0c674';
   ctx.beginPath();ctx.moveTo(cx-4,by+5);ctx.lineTo(cx+4,by+5);ctx.lineTo(cx,by);ctx.closePath();ctx.fill();
 }}
 ctx.fillStyle='#6e7681';ctx.textAlign='center';
 const step=Math.max(1,Math.round(n/12));
 for(let i=Math.ceil(view0);i<view1;i++){if(i%step)continue;ctx.fillText(i,(i-view0)*bw+bw/2,h-8);}
 ctx.textAlign='left';ctx.fillText('vblank #',4,h-8);
}
function at(ev){const r=c.getBoundingClientRect();return view0+((ev.clientX-r.left)/W())*(view1-view0);}
c.addEventListener('mousemove',ev=>{
 if(drag){const d=at(ev)-(dragV0+((dragX-c.getBoundingClientRect().left)/W())*(dragV1-dragV0));
   view0=dragV0-d;view1=dragV1-d;clampView();draw();tip.style.display='none';return;}
 const i=Math.floor(at(ev));const b=bars[i];if(!b){tip.style.display='none';return;}
 let rows=`<tr><td>vblank</td><td class=n>${i} ${b.r?'<span style="color:#56d364">RENDER</span>':'<span style="color:#8b949e">sim-only</span>'} &middot; room ${b.room}</td></tr>`+
   `<tr><td>total</td><td class=n>${ms(b.fc)} ms &middot; ${vbl(b.fc)} vbl &middot; ${fmt(b.fc)} cyc</td></tr>`;
 if(b.r){const p=bars[i+1];const pair=b.fc+(p&&!p.r?p.fc:0);const miss=pair>st.budget2;
   rows+=`<tr><td>30fps frame (this+sim vb)</td><td class=n style="color:${miss?'#f85149':'#c9d1d9'}">${ms(pair)}ms = ${(pair/st.budget2*100).toFixed(0)}% of 2vb${miss?' (MISS)':''}</td></tr>`;}
 rows+=`<tr><td colspan=2 class=hd>phases (bottom &rarr; top)</td></tr>`;
 for(let s=0;s<b.s.length;s++){const v=b.s[s];if(v<=0)continue;
   rows+=`<tr><td><span class=sw style="background:${colors[s]};margin:0 4px 0 0"></span>${labels[s]}</td>`+
         `<td class=n>${ms(v)}ms (${pctb(v)}%)</td></tr>`;}
 const dk=Object.keys(b.t);
 if(dk.length){rows+=`<tr><td colspan=2 class=hd>detail (sub-stages)</td></tr>`;
   for(const k of dk)rows+=`<tr><td>${k}</td><td class=n>${ms(b.t[k])}ms</td></tr>`;}
 tip.innerHTML=`<table>${rows}</table>`;tip.style.display='block';
 const r=c.getBoundingClientRect();let tx=ev.clientX-r.left+14,ty=ev.clientY-r.top+12;
 if(tx+tip.offsetWidth>W())tx-=tip.offsetWidth+28;tip.style.left=tx+'px';tip.style.top=ty+'px';
});
c.addEventListener('mouseleave',()=>{tip.style.display='none';});
c.addEventListener('mousedown',ev=>{drag=true;dragX=ev.clientX;dragV0=view0;dragV1=view1;});
addEventListener('mouseup',()=>{drag=false;});
c.addEventListener('wheel',ev=>{ev.preventDefault();const f=at(ev),k=ev.deltaY<0?0.85:1/0.85;
 view0=f-(f-view0)*k;view1=f+(view1-f)*k;clampView();draw();},{passive:false});
c.addEventListener('dblclick',()=>{view0=0;view1=bars.length;draw();});
function clampView(){const min=8;if(view1-view0<min){const m=(view0+view1)/2;view0=m-min/2;view1=m+min/2;}
 if(view0<0){view1-=view0;view0=0;}if(view1>bars.length){view0-=view1-bars.length;view1=bars.length;}
 if(view0<0)view0=0;}
resize();
</script></body></html>"""


if __name__ == "__main__":
    main()
