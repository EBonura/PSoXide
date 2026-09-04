"""First melee hit per attack start from a --counter-log CSV.
usage: hitphase.py counters.csv [label]"""
import csv, sys, collections
NAMES = {6: "LightAttack", 7: "HeavyAttack", 30: "VertLightAttack", 31: "VertHeavyAttack"}
# The cook trims each attack clip to its authored frame range, so the runtime
# phase counts from the range start; add it back to read editor-timeline frames.
FRAME_START = {6: 26, 7: 9, 30: 3, 31: 6}
SWING = {6: (48, 68), 7: (55, 76), 30: (28, 47), 31: (54, 76)}  # weapon trail windows
rows = list(csv.DictReader(open(sys.argv[1])))
label = sys.argv[2] if len(sys.argv) > 2 else sys.argv[1]
starts = []
prev_s = prev_h = None
for r in rows:
    s, h = int(r["player_attack_starts_total"]), int(r["player_melee_hits_total"])
    if prev_s is not None and s > prev_s:
        starts.append({"frame": int(r["guest_frame"]), "action": int(r["player_anim_action"]), "hit": None})
    if prev_h is not None and h > prev_h and starts and starts[-1]["hit"] is None:
        starts[-1]["hit"] = (int(r["guest_frame"]), int(r["player_anim_phase_q12"]) >> 12, int(r["player_anim_action"]))
    prev_s, prev_h = s, h
per = collections.defaultdict(list)
for st in starts:
    if st["hit"]:
        per[st["hit"][2]].append((st["hit"][1], st["hit"][0] - st["frame"]))
print(f"== {label}: {len(starts)} attack starts, {sum(1 for s in starts if s['hit'])} landed")
for action, hits in sorted(per.items()):
    phases = sorted(p + FRAME_START.get(action, 0) for p, _ in hits)
    delays = sorted(d for _, d in hits)
    lo, hi = SWING.get(action, (0, 0))
    inside = sum(lo <= p <= hi for p in phases)
    print(f"  {NAMES.get(action, action):16} landed={len(hits):2}  first-hit editor frame min/median/max = {phases[0]}/{phases[len(phases)//2]}/{phases[-1]}  swing {lo}-{hi}: {inside}/{len(hits)} inside   guest frames after start = {delays[0]}..{delays[-1]}")
