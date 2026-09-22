#!/usr/bin/env python3
"""Run frozen HL/CS HSFX against shared source with actual SDK register lowering.

Only MMIO read/write, device init, and ADPCM transfer endpoints are replaced by
recorders. The current SDK Voice/OneShot implementations compute every register
value and preserve write order. No retail assets or emulator are required.
"""
from pathlib import Path
import argparse, hashlib, json, re, subprocess, tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def replace_body(source, name, body):
    m = re.search(r'^(?:pub )?(?:unsafe )?fn '+name+r'\([^\n]*\)[^{]*\{', source, re.M)
    assert m, name
    end = m.end(); depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}'); end += 1
    return source[:m.end()] + body + source[end-1:]


def hardware_imports(s):
    return s.replace('psx_spu::', 'crate::spu::').replace('psx_sfx::', 'crate::sfx::')


def main():
    ap = argparse.ArgumentParser(); ap.add_argument('--output', type=Path); args = ap.parse_args()
    with tempfile.TemporaryDirectory(prefix='hsfx-oracle-') as scratch:
        p = Path(scratch); (p/'src').mkdir()
        deps = '\n'.join(f'{name} = {{ path = "{ROOT}/sdk/crates/{name}" }}' for name in ['psx-asset', 'psx-io'])
        (p/'Cargo.toml').write_text('[package]\nname="hsfx-oracle"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[dependencies]\n'+deps+'\n')
        spu_path = ROOT/'sdk/crates/psx-spu/src/lib.rs'; spu = spu_path.read_text()
        spu = re.sub(r'^#!.*\n', '', spu, flags=re.M)
        spu = spu.replace('pub mod tones;', f'#[path="{spu_path.parent}/tones.rs"] pub mod tones;')
        spu = replace_body(spu, 'write_reg16', '\ncrate::record(crate::Op::Write(addr,value));\n')
        spu = replace_body(spu, 'read_reg16', '\nlet _ = addr; 0\n')
        spu = replace_body(spu, 'init', '\ncrate::record(crate::Op::Init);\n')
        spu = replace_body(spu, 'upload_adpcm', '\ncrate::record(crate::Op::Upload(dest.byte_offset(),bytes.to_vec()));\n')
        (p/'src/spu.rs').write_text(spu)
        sfx_path = ROOT/'sdk/crates/psx-sfx/src/lib.rs'; sfx = re.sub(r'^#!.*\n','',sfx_path.read_text(),flags=re.M)
        (p/'src/sfx.rs').write_text(hardware_imports(sfx))
        shared_path = HERE.parent/'src/hsfx.rs'; shared = hardware_imports(shared_path.read_text())
        shared += '''
impl<const N:usize,const H:u8,const S:u8> Hsfx<N,H,S> {
 pub fn snapshot(&self)->Vec<u64> {
  let mut v=Vec::new();
  for n in self.addrs {v.push(n as u64)} for n in self.rates {v.push(n as u64)}
  v.extend([self.count as u64,self.next_voice as u64,self.dialogue_base as u64]);
  for n in self.voice_addrs {v.push(n as u64)} for n in self.voice_rates {v.push(n as u64)}
  v.push(self.voice_count as u64);for n in self.map_loop_owner {v.push(n as u64)}
  v.push(self.next_map_loop as u64);for n in self.ear {v.push(n as u64)} v
 }
}
'''
        (p/'src/shared.rs').write_text(shared)
        for short, game in [('hl','hl-psx'),('cs','cs-psx')]:
            old = hardware_imports(((HERE/'oracles'/(game+'-ids.rs')).read_text() + (HERE/'oracles/legacy-hsfx-runtime.rs').read_text()))
            old += '''
pub unsafe fn reset() { ADDRS=[0;MAX_SFX]; RATES=[0;MAX_SFX]; COUNT=0; NEXT_VOICE=0; DIALOGUE_BASE=0; VOICE_ADDRS=[0;MAX_VOICES];VOICE_RATES=[0;MAX_VOICES];VOICE_COUNT=0;MAP_LOOP_OWNER=[MAP_LOOP_OWNER_NONE;MAP_LOOP_VOICE_COUNT];NEXT_MAP_LOOP=0;EAR=[0;3]; }
pub unsafe fn snapshot()->Vec<u64>{let mut v=Vec::new();for n in ADDRS {v.push(n as u64)}for n in RATES{v.push(n as u64)}v.extend([COUNT as u64,NEXT_VOICE as u64,DIALOGUE_BASE as u64]);for n in VOICE_ADDRS{v.push(n as u64)}for n in VOICE_RATES{v.push(n as u64)}v.push(VOICE_COUNT as u64);for n in MAP_LOOP_OWNER{v.push(n as u64)}v.push(NEXT_MAP_LOOP as u64);for n in EAR{v.push(n as u64)}v}
'''
            (p/f'src/old_{short}.rs').write_text(old)
            n = re.search(r'const MAX_SFX: usize = (\d+)', old)[1]
            ids = [re.search(r'pub const '+key+r': u8 = (\d+)', old)[1] for key in ['CHARGER_HEALTH_LOOP','CHARGER_HEV_LOOP']]
            adapter = f'static mut STATE:crate::shared::Hsfx<{n},{ids[0]},{ids[1]}>=crate::shared::Hsfx::new();\npub unsafe fn reset() {{STATE=crate::shared::Hsfx::new();}}\npub unsafe fn snapshot()->Vec<u64>{{STATE.snapshot()}}\n'
            for m in re.finditer(r'^pub unsafe fn (\w+)\((.*?)\)([^\{]*)\{', old, re.M|re.S):
                name, formals, ret = m.groups()
                if name in ['reset','snapshot']: continue
                names=', '.join(x.split(':')[0].strip() for x in formals.split(',') if x.strip())
                adapter += f'pub unsafe fn {name}({formals}){ret}{{STATE.{name}({names})}}\n'
            (p/f'src/new_{short}.rs').write_text(adapter)
        (p/'src/main.rs').write_text((HERE/'support/hsfx_harness.rs').read_text())
        run = subprocess.run(['cargo','run','--release','--quiet','--manifest-path',str(p/'Cargo.toml')], capture_output=True,text=True)
        if run.returncode:
            print(run.stdout);print(run.stderr);raise SystemExit(run.returncode)
        print(run.stdout.strip())
        result={'status':'pass','output':run.stdout.strip(),'shared_sha256':hashlib.sha256(shared_path.read_bytes()).hexdigest(),'sdk_spu_sha256':hashlib.sha256(spu_path.read_bytes()).hexdigest(),'sdk_sfx_sha256':hashlib.sha256(sfx_path.read_bytes()).hexdigest(),'method':'Actual shared and frozen-old source; SDK Voice/OneShot register lowering unchanged; only hardware transfer/MMIO endpoints recorded. Legacy truncated directory panic is preserved.'}
        if args.output: args.output.write_text(json.dumps(result,indent=2)+'\n')

if __name__ == '__main__': main()
