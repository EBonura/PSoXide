# Exclusion search: simple deterministic GTE hazard models vs the HWB-008
# live capture (SC 2810 event). All values from the photographed pages.
VI = [[0x0F6D, 0x0000, -0x043F],   # 0xFBC1
      [0x0125, -0x0F68, 0x0427],   # F098
      [-0x0416, -0x044F, -0x0EDB]] # FBEA FBB1 F125
def s16(v): return v - 0x10000 if v >= 0x8000 else v
M0 = [[0x0216, 0x0020, s16(0xFF07)],
      [s16(0xFFE1), 0x0217, 0x0010],
      [0x0029, s16(0xFFE4), 0x0216]]
M1 = [[0x0216, 0x0020, s16(0xFF07)],
      [s16(0xFFDE), 0x0217, s16(0xFFF1)],
      [0x0027, 0x0010, 0x0217]]
R0_HW = [[0x0C70, 0x020A, s16(0xFF91)],
         [0x004E, s16(0xFDF7), 0x006B],
         [s16(0xFF59), s16(0xFF81), s16(0xFE12)]]
R1_HW = [[s16(0xF0C5), 0x01FE, s16(0xFF90)],
         [0x0051, s16(0xFE03), 0x0096],
         [s16(0xFF5C), s16(0xFF58), s16(0xFE1D)]]

def mvmva(rt, v, tr=(0,0,0)):
    out = []
    for i in range(3):
        acc = (tr[i] << 12) + sum(rt[i][k] * v[k] for k in range(3))
        out.append(acc >> 12)
    return out

def col(m, j): return [m[0][j], m[1][j], m[2][j]]
def compose(vi, m): 
    cols = [mvmva(vi, col(m, j)) for j in range(3)]
    return [[cols[j][i] for j in range(3)] for i in range(3)]

R0_T = compose(VI, M0); R1_T = compose(VI, M1)
print("true R0 row0:", [hex(v & 0xFFFF) for v in R0_T[0]])
print("hw   R0 row0:", [hex(v & 0xFFFF) for v in R0_HW[0]])

# H4: MAC1 read 1-instruction stale -> c_j.x = previous MVMVA's MAC1.
# Within one compose, c1.x should equal true c0.x and c2.x true c1.x.
print("\nH4 stale-MAC1-read:")
print("  c1.x hw", R0_HW[0][1], "vs true c0.x", R0_T[0][0], "->", R0_HW[0][1] == R0_T[0][0])
print("  c2.x hw", R0_HW[0][2], "vs true c1.x", R0_T[0][1], "->", R0_HW[0][2] == R0_T[0][1])

# H3/H5: V0 component load-delay (z or xy stale from previous column).
# Any stale V corrupts ALL THREE rows of that column; rows 1-2 of cols 0-1
# are exact on hw, so only col2 could carry it. Test col2 with stale z/xy:
for name, v in [("z from col1", [M0[0][2], M0[1][2], M0[2][1]]),
                ("xy from col1", [M0[0][1], M0[1][1], M0[2][2]])]:
    c = mvmva(VI, v)
    print(f"  V-delay col2 {name}: calc {[hex(x & 0xFFFF) for x in c]} vs hw col2 "
          f"{[hex(R0_HW[i][2] & 0xFFFF) for i in range(3)]} -> {all(c[i] == R0_HW[i][2] for i in range(3))}")

# TRX-stale: adds a CONSTANT to all c_j.x of one compose.
d = [R0_HW[0][j] - R0_T[0][j] for j in range(3)]
print("\nTRX-stale (needs constant diff): R0 x-diffs", d)
d1 = [R1_HW[0][j] - R1_T[0][j] for j in range(3)]
print("                                R1 x-diffs", d1)

# vx-stale (MVMVA ran with V1/V2): same V for all columns -> all c_j.x equal.
print("\nvx-stale (needs equal c_j.x): R0 hw x:", [R0_HW[0][j] for j in range(3)])

# H2: whole row0 regs stale (R11,R12,R13 = unknown a,b,c). Solve exactly over
# integers for R0, then check the SAME row against R1 (regs unchanged between
# the two composes if every load_rotation writes VI identically).
from itertools import product
import numpy as np
A = np.array([[col(M0,j)[k] for k in range(3)] for j in range(3)], dtype=float)
b = np.array([R0_HW[0][j] * 4096.0 for j in range(3)])
sol = np.linalg.solve(A, b)
print("\nH2 stale-row solve (R0):", sol, "(plausible i16 row?)")
# integer check with >>12 flooring over a small lattice around sol
found = None
base = [int(round(x)) for x in sol]
for da, db, dc in product(range(-2,3), repeat=3):
    a, bb, c = base[0]+da, base[1]+db, base[2]+dc
    if all((a*col(M0,j)[0] + bb*col(M0,j)[1] + c*col(M0,j)[2]) >> 12 == R0_HW[0][j] for j in range(3)):
        found = (a, bb, c); break
print("  exact integer row for R0:", found)
if found:
    a, bb, c = found
    pred = [(a*col(M1,j)[0] + bb*col(M1,j)[1] + c*col(M1,j)[2]) >> 12 for j in range(3)]
    print("  same row applied to M1:", pred, "vs R1 hw x:", [R1_HW[0][j] for j in range(3)])
