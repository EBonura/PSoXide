#!/usr/bin/env python3
"""Flag MFC0/MFC2 load-delay hazards in guest assembly.

On the R3000 the destination of `mfc0`/`mfc2` arrives one instruction
late: the very next instruction reads the STALE register. This has now
shipped twice (psx-rt's enable_cpu_interrupts historically, and the
demo-disc loader's quiesce(), where the stale value landed in SR with BEV
set and every interrupt vectored into the ROM). The emulator models the
hazard faithfully, but only code that RUNS under it gets caught; this
check reads the source instead.

Heuristic: inside .rs files, for every asm line containing `mfc0`/`mfc2`
with a register destination, look at the next asm-looking line in the
file; if it mentions the same register, fail. A `nop` (or any line not
touching the register) is fine. Scans sdk/ and engine/ by default.

    python3 tools/check_mfc0.py [paths...]
"""

import re
import sys
from pathlib import Path

MFC = re.compile(r'\bmfc[02]\s+(\$\w+)')
ASMISH = re.compile(r'"|^\s*[a-z]+\s+\$|^\s*\.')


def check_file(path: Path) -> list[str]:
    problems = []
    lines = path.read_text(errors="replace").splitlines()
    for i, line in enumerate(lines):
        m = MFC.search(line)
        if not m:
            continue
        reg = re.escape(m.group(1))
        # The delay slot is the next line that looks like an instruction.
        for nxt in lines[i + 1 : i + 4]:
            stripped = nxt.strip().strip('",')
            if not stripped or stripped.startswith("//") or stripped.startswith("#"):
                continue
            if re.search(rf'(?<!mfc0 ){reg}\b', stripped) and "nop" not in stripped:
                problems.append(
                    f"{path}:{i + 1}: `{line.strip()}` is followed by "
                    f"`{stripped}` which reads {m.group(1)} in the load-delay slot"
                )
            break
    return problems


def main() -> int:
    roots = [Path(p) for p in sys.argv[1:]] or [Path("sdk"), Path("engine")]
    problems = []
    for root in roots:
        for path in sorted(root.rglob("*.rs")):
            if "target" in path.parts:
                continue
            problems.extend(check_file(path))
    for p in problems:
        print(p)
    if problems:
        print(f"\n{len(problems)} mfc load-delay hazard(s). Put a nop after the mfc.")
        return 1
    print("mfc0/mfc2 delay slots clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
