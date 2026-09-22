#!/usr/bin/env python3
"""Frozen old numerical renderer versus the shared owner and both view policies."""
from pathlib import Path
import argparse,hashlib,json,subprocess,tempfile
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[3]
def main():
 ap=argparse.ArgumentParser();ap.add_argument('--output',type=Path);a=ap.parse_args();results=[]
 provenance=json.loads((HERE/'oracles/RENDER-PROVENANCE.json').read_text());original=(HERE/'oracles/legacy-render.rs').read_text();assert hashlib.sha256(original.encode()).hexdigest()==provenance['hl_sha256']
 for game in ['hl','cs']:
  old=original
  if game=='cs':
   lines=old.splitlines(True)
   for d in reversed(provenance['cs_line_delta']):lines[d['start']:d['end']]=d['replacement']
   old=''.join(lines)
  assert hashlib.sha256(old.encode()).hexdigest()==provenance[game+'_sha256']
  with tempfile.TemporaryDirectory(prefix='render-oracle-')as tmp:
   p=Path(tmp);(p/'src').mkdir();(p/'src/old.rs').write_text(old)
   adapter=(HERE/f'support/{game}_render_adapter.rs').read_text();unit_tests=old.split('#[cfg(test)]',1)[1].replace('use super::*;', '''use super::*;
    use crate::new::{project_soft,close_inv_q12,set_projection_h,projection_h,visible_clip,guard_clip,on_visible_boundary,quad_outside_vertical};
    const OFY:i32=120;
    fn ofy()->i32{120}
    fn view_plane_distance(v:&SVert,p:ViewPlane)->i32 {super::view_plane_distance(&FullView::new(),v,p)}
''')
   (p/'src/new.rs').write_text(adapter);(p/'src/shared.rs').write_text((HERE.parent/'src/render.rs').read_text()+'\n#[cfg(test)]'+unit_tests)
   main=(HERE/'support/render_harness.rs').read_text().replace('SELECT_VIEW',('old::set_view_rect(x,y,w,h);new::set_view_rect(x,y,w,h);'if game=='cs'else'assert_eq!((x,y,w,h),(0,0,320,240));')).replace('VIEW_RECTS',('[(0,0,320,240),(0,0,320,120),(0,120,320,120),(32,24,256,192)]'if game=='cs'else'[(0,0,320,240)]'))
   (p/'src/main.rs').write_text(main);(p/'Cargo.toml').write_text('[package]\nname="render-oracle"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[dependencies]\npsx-engine={path="'+str(ROOT/'engine/crates/psx-engine')+'"}\n')
   for lane in [['cargo','run','--release','--quiet'],['cargo','test','--release','--quiet','--','--test-threads=1']]:
    run=subprocess.run(lane,cwd=p,capture_output=True,text=True)
    if run.returncode:print(run.stdout);print(run.stderr);raise SystemExit(run.returncode)
    print(game,run.stdout.strip());results.append({'game':game,'command':lane,'result':run.stdout.strip()})
 result={'status':'pass','results':results,'shared_sha256':hashlib.sha256((HERE.parent/'src/render.rs').read_bytes()).hexdigest(),'scope':'Exact signed integer, clipping topology/order, RGB/UV/depth and guard decisions; both original unit test suites retained against old and new. Host release arithmetic; target codegen, GPU packets, gameplay and timing require separate gates.'}
 if a.output:a.output.write_text(json.dumps(result,indent=2)+'\n')
if __name__=='__main__':main()
