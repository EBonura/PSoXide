#!/usr/bin/env python3
"""Compare cached chunk algorithms against frozen HL/CS at transport boundaries."""
from pathlib import Path
import argparse,hashlib,json,re,subprocess,tempfile
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[3]

def remove_host_functions(s):
    while True:
        m=re.search(r'#\[cfg\(not\(target_arch = "mips"\)\)\]\s*(?:#\[[^\n]+\]\s*)*(?:pub )?(?:unsafe )?fn \w+\([^\{]*\{',s)
        if not m:return s
        end=m.end();depth=1
        while depth:depth+=(s[end]=='{')-(s[end]=='}');end+=1
        s=s[:m.start()]+s[end:]

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--output',type=Path);a=ap.parse_args();results=[]
    for capacity in [16,32]:
      with tempfile.TemporaryDirectory(prefix='chunk-oracle-') as tmp:
        p=Path(tmp);(p/'src').mkdir()
        old=(HERE/'oracles/legacy-cdstream.rs').read_text();old=remove_host_functions(old)
        old=old.replace('#[cfg(target_arch = "mips")]','').replace('use psx_pack::cd::{SectorReader, SECTOR_WORDS};','use crate::fake::{Reader as SectorReader,SECTOR_WORDS};').replace('psx_pack::cd::find_entry','crate::fake::find_entry').replace('use psx_io::cdrom::poll_data_sector as try_sector_ready;','use crate::fake::ready as try_sector_ready;').replace('const PERSIST_ENTRIES: usize = 16;',f'const PERSIST_ENTRIES: usize = {capacity};')
        # All state is reset between paired scenarios. Raw destination addresses
        # are not compared, only the bytes actually written and ordered events.
        old+='''
pub unsafe fn reset(){READER=SectorReader::new();SECTOR_BUF=[0;SECTOR_WORDS];PACK_CACHE_LEN=-1;PERSIST_CACHE=[CachedEntry{id:PERSIST_NONE,sector_offset:0,byte_size:0};PERSIST_ENTRIES];CHUNK_STREAM.active=false;CHUNK_STREAM.just_started=false;ON_SECTOR=None;}
'''
        (p/'src/old.rs').write_text(old)
        (p/'src/shared.rs').write_text((HERE.parent/'src/chunk_stream.rs').read_text())
        (p/'src/main.rs').write_text((HERE/'support/chunk_stream_harness.rs').read_text().replace('PERSIST_CAPACITY',str(capacity)))
        (p/'Cargo.toml').write_text('[package]\nname="chunk-stream-oracle"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[dependencies]\n'+ '\n'.join(f'{n}={{path="{ROOT}/sdk/crates/{n}"}}'for n in ['psx-pack','psx-io'])+'\n')
        run=subprocess.run(['cargo','run','--release','--quiet','--manifest-path',str(p/'Cargo.toml')],capture_output=True,text=True)
        if run.returncode:print(run.stdout);print(run.stderr);raise SystemExit(run.returncode)
        print(run.stdout.strip());results.append({'capacity':capacity,'result':run.stdout.strip()})
    result={'status':'pass','configurations':results,'shared_sha256':hashlib.sha256((HERE.parent/'src/chunk_stream.rs').read_bytes()).hexdigest(),'legacy_sha256':hashlib.sha256((HERE/'oracles/legacy-cdstream.rs').read_bytes()).hexdigest(),'scope':'Actual cache/pump algorithms; transport readiness/read/failure callbacks are simulated. Hardware timing and DMA ordering remain guest gates.'}
    if a.output:a.output.write_text(json.dumps(result,indent=2)+'\n')
if __name__=='__main__':main()
