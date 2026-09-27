#!/usr/bin/env python3
"""Compile actual psx-pad source with deterministic SIO register scheduling.

Time is abstract MMIO-operation ticks, NOT SCPH-110 calibrated cycles. No Select
bits are injected. Tests expose readiness/timeout contracts, not physical cause.
"""
from pathlib import Path
import argparse, hashlib, json, subprocess

p=argparse.ArgumentParser(); p.add_argument('source',type=Path); p.add_argument('out',type=Path); p.add_argument('--input-source',type=Path)
a=p.parse_args(); a.source=a.source.resolve(); a.out=a.out.resolve(); a.out.mkdir(parents=True,exist_ok=True)
src=a.source.read_text()
src='\n'.join(l for l in src.splitlines() if not l.startswith('#!'))
src=src.replace('//!','//')
tracker=a.source.with_name('tracker.rs').read_text().replace('//!','//')
src=src.replace('pub mod tracker;', 'pub mod tracker {\n'+tracker+'\n}')
root=a.source.parents[3]
hw=root.parent/'crates/psx-hw/src/sio.rs'
if not hw.exists(): raise SystemExit(f'missing pinned hardware constants: {hw}')
mock=(Path(__file__).with_name('mock.rs')).read_text().replace('HARDWARE_PATH',str(hw))
input_module=''
if a.input_source:
    body=a.input_source.read_text().replace('//!','//').replace('psx_pad::','crate::')
    input_module='mod collection_input {\n'+body+'\npub fn reset_test_state(){unsafe {LAST_CLEAN=crate::ButtonState::NONE;}}\n}\n'
(a.out/'main.rs').write_text(mock+'\n'+src+'\n'+input_module+'\n'+Path(__file__).with_name('cases.rs').read_text())
cmd=['rustc','--edition=2021','-Awarnings',str(a.out/'main.rs'),'-O','-o',str(a.out/'check')]
if a.input_source:cmd+=['--cfg','has_collection_input']
subprocess.run(cmd,check=True)
r=subprocess.run([str(a.out/'check')],text=True,capture_output=True)
(a.out/'results.log').write_text(r.stdout+r.stderr)
receipt={'source':str(a.source.resolve()),'source_sha256':hashlib.sha256(a.source.read_bytes()).hexdigest(),'generated_sha256':hashlib.sha256((a.out/'main.rs').read_bytes()).hexdigest(),'returncode':r.returncode,'calibration':'abstract MMIO ticks, not physical controller timing','stdout':r.stdout,'stderr':r.stderr}
if a.input_source:receipt['collection_input_sha256']=hashlib.sha256(a.input_source.read_bytes()).hexdigest()
(a.out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(r.stdout+r.stderr,end=''); raise SystemExit(r.returncode)
