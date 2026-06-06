#!/usr/bin/env python3
"""Render a per-vblank work chart from a `frontend launch --profile-log` CSV.

The profile log writes one row per guest vblank (a `frame_begin` marker fires
each sim tick, which advances once per display vblank). Column `frame_cycles`
is the total guest CPU cycles spent in that vblank; the per-stage columns
(`update`, `render`, `present`, `player`, `room`, ...) break it down.

This tool stacks those stages into per-vblank bars so the 60 Hz-sim / 30 Hz-render
cadence is visible at a glance: render vblanks carry the whole frame build and
overflow the one-vblank budget, while the alternate sim-only vblanks run light.

Usage:
    python3 tools/vblank_chart.py --in /tmp/demo10-vblank.csv --out /tmp/demo10-vblank.html
    python3 tools/vblank_chart.py --in run.csv --out run.html --title "demo10 gameplay"

The output is a single self-contained HTML file (scroll = zoom, drag = pan,
double-click = reset). Summary stats are also printed to stdout.
"""
from __future__ import annotations

import argparse
import csv
import json
import sys

# NTSC: 33.8688 MHz CPU / 60 Hz = one display field.
ONE_VBLANK_CYCLES = 564480

# Stacked series, bottom -> top. Each maps a label/color to the profile columns
# it sums. `render`/`update` parents are intentionally excluded; we stack their
# leaf sub-stages so the build cost is legible. Anything left over (frame_cycles
# minus the sum) lands in the gray "untracked" band so bars always total the
# real per-vblank cost.
SERIES = [
    ("sim / update", "#1f6feb", ["update"]),
    ("room / sky / world", "#1a7f37",
     ["frame_clear", "room", "sky", "far_vista", "model_instances",
      "world_flush", "ot_submit"]),
    ("props (image / box)", "#2ea043",
     ["image_props", "box_props", "box_prop_debris", "box_prop_shards",
      "image_cards", "equipment"]),
    ("player model", "#3fb950", ["player"]),
    ("present (vsync / swap)", "#db61a2", ["present"]),
]
OTHER_COLOR = "#6e7681"

# Extra columns surfaced only in the hover tooltip (deeper render breakdown).
TOOLTIP_COLS = ["player", "image_props", "room", "sky", "world_flush",
                "ot_submit", "present", "update", "current_room",
                "visual_deadline_misses"]


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
    if idx_fc is None:
        sys.exit("error: CSV has no 'frame_cycles' column -- is this a profile-log?")

    series_idx = [(label, color, [col(header, c) for c in cols])
                  for (label, color, cols) in SERIES]
    tip_idx = [(c, col(header, c)) for c in TOOLTIP_COLS]

    bars = []
    render_cyc, sim_cyc = [], []
    misses = 0
    idx_miss = col(header, "visual_deadline_misses")
    for r in rows:
        fc = num(r, idx_fc)
        stacks = []
        used = 0
        for (_label, _color, idxs) in series_idx:
            v = sum(num(r, i) for i in idxs)
            stacks.append(v)
            used += v
        other = max(0, fc - used)
        stacks.append(other)
        is_render = num(r, idx_render) > 0
        tip = {name: num(r, i) for (name, i) in tip_idx if i is not None}
        bars.append({"s": stacks, "fc": fc, "r": 1 if is_render else 0,
                     "m": num(r, idx_miss), "t": tip})
        (render_cyc if is_render else sim_cyc).append(fc)
        misses += num(r, idx_miss)

    render_cyc.sort()
    sim_cyc.sort()
    budget = args.budget
    r_avg = sum(render_cyc) // max(1, len(render_cyc))
    s_avg = sum(sim_cyc) // max(1, len(sim_cyc))
    overall_avg = (sum(render_cyc) + sum(sim_cyc)) // max(1, len(bars))
    r_over = sum(1 for c in render_cyc if c > budget)
    # Perfectly-spread target: total work / number of vblanks.
    spread = overall_avg

    title = args.title or f"per-vblank work — {args.inp.split('/')[-1]}"
    labels = [s[0] for s in SERIES] + ["untracked"]
    colors = [s[1] for s in SERIES] + [OTHER_COLOR]

    stats = {
        "vblanks": len(bars),
        "render_vblanks": len(render_cyc),
        "sim_vblanks": len(sim_cyc),
        "render_avg": r_avg,
        "render_p50": percentile(render_cyc, 50),
        "render_max": render_cyc[-1] if render_cyc else 0,
        "render_over_budget": r_over,
        "sim_avg": s_avg,
        "sim_p50": percentile(sim_cyc, 50),
        "overall_avg": overall_avg,
        "spread_target": spread,
        "misses": misses,
        "budget": budget,
        "budget2": budget * 2,
    }

    # y-axis cap: keep the steady state readable; clip rare streaming stalls.
    cap = max(budget * 2, int(percentile(sorted(b["fc"] for b in bars), 99) * 1.1))

    payload = {"title": title, "labels": labels, "colors": colors,
               "bars": bars, "stats": stats, "cap": cap}
    html = HTML_TEMPLATE.replace("__DATA__", json.dumps(payload, separators=(",", ":")))
    with open(args.out, "w") as f:
        f.write(html)

    # Console summary.
    def pc(v):
        return f"{v:>10,} ({v / budget * 100:5.1f}% of 1 vblank)"
    print(f"wrote {args.out}  ({len(bars)} vblanks)")
    print(f"  render vblanks : {len(render_cyc):>4}  avg {pc(r_avg)}  "
          f"p50 {pc(stats['render_p50'])}  over-budget {r_over}/{len(render_cyc)}")
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
 #ft{padding:6px 16px;color:#6e7681;font-size:12px}
</style></head><body>
<div id="hdr"><h1 id="t"></h1><div id="s"></div></div>
<div id="lg"></div>
<div id="wrap"><canvas id="c"></canvas><div id="tip"></div></div>
<div id="ft">scroll = zoom · drag = pan · double-click = reset &nbsp;·&nbsp; <span style="color:#f85149">red baseline tick</span> = 30fps slot missed (render vb + sim vb &gt; 2 vblanks) &nbsp;·&nbsp; <span style="color:#f85149">red top tick</span> = off-scale stall</div>
<script>
const D=__DATA__;
const c=document.getElementById('c'),ctx=c.getContext('2d'),tip=document.getElementById('tip');
const bars=D.bars,labels=D.labels,colors=D.colors,st=D.stats,cap=D.cap,budget=st.budget;
document.getElementById('t').textContent=D.title;
const fmt=n=>n.toLocaleString();
document.getElementById('s').innerHTML=
 `<span class=stat>vblanks <b>${st.vblanks}</b></span>`+
 `<span class=stat>render <b>${st.render_vblanks}</b> avg <b>${fmt(st.render_avg)}</b> (${(st.render_avg/budget*100).toFixed(0)}% of 1vb), ${st.render_over_budget} over budget</span>`+
 `<span class=stat>sim-only <b>${st.sim_vblanks}</b> avg <b>${fmt(st.sim_avg)}</b> (${(st.sim_avg/budget*100).toFixed(0)}%)</span>`+
 `<span class=stat>deadline misses <b>${st.misses}</b></span>`+
 `<span class=stat>spread target <b>${fmt(st.spread_target)}</b>/vb (${(st.spread_target/budget*100).toFixed(0)}%)</span>`;
let lg='';for(let i=0;i<labels.length;i++)lg+='<span class="sw" style="background:'+colors[i]+'"></span>'+labels[i];
document.getElementById('lg').innerHTML=lg;

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
 // budget gridlines: 1 vblank (solid) and 2 vblanks/30fps (dashed)
 ctx.strokeStyle='#30363d';ctx.fillStyle='#8b949e';ctx.font='11px sans-serif';ctx.textAlign='left';
 // Per-vblank reference is the 1-vblank budget. A presented 30fps frame owns
 // TWO vblanks (a render vblank + its sim vblank), so the deadline is a property
 // of that PAIR, not a single bar: the pair must fit 2x this line. The red
 // baseline ticks flag pairs that did not (a slipped 30fps slot). Do not draw a
 // flat 2-vblank line over single bars; it reads as "bars under here are fine",
 // which is false (one render bar is only half a frame).
 ctx.setLineDash([]);
 if(budget<=yMax){ctx.beginPath();ctx.moveTo(0,y(budget));ctx.lineTo(w,y(budget));ctx.stroke();
   ctx.fillText('1-vblank budget '+fmt(budget)+'  (30fps frame = render vb + sim vb, the pair must fit 2x)',4,y(budget)-3);}
 // bars
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
   // off-scale marker for streaming stalls (red tick at top)
   if(b.fc>yMax){ctx.fillStyle='#f85149';ctx.fillRect(x+0.5,PADT,Math.max(1,bw-1),3);}
   // deadline-miss marker (red tick at baseline)
   if(b.m>0){ctx.fillStyle='#f85149';const mw=Math.min(Math.max(1,bw-1),7);ctx.fillRect(x+(bw-mw)/2,PADT+plot+1,mw,4);}
 }
 // x labels (sparse)
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
 let rows=`<tr><td>vblank</td><td class=n>${i}</td></tr>`+
   `<tr><td>${b.r?'RENDER':'sim-only'}</td><td class=n>${fmt(b.fc)} cyc</td></tr>`+
   `<tr><td>% of 1 vblank</td><td class=n>${(b.fc/budget*100).toFixed(0)}%</td></tr>`;
 if(b.r){const p=bars[i+1];const pair=b.fc+(p&&!p.r?p.fc:0);const miss=pair>st.budget2;
   rows+=`<tr><td>30fps frame (this+sim vb)</td><td class=n style="color:${miss?'#f85149':'#c9d1d9'}">${fmt(pair)} = ${(pair/st.budget2*100).toFixed(0)}% of 2vb${miss?' (MISS)':''}</td></tr>`;}
 for(const k in b.t)rows+=`<tr><td>${k}</td><td class=n>${fmt(b.t[k])}</td></tr>`;
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
