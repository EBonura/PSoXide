fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mb = std::fs::read(&a[0]).unwrap();
    let model = psx_asset::Model::from_bytes(&mb).unwrap();
    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
    for v in 0..model.vertex_count() {
        let p = model.vertex(v).unwrap().position;
        for (i, c) in [p.x as i32, p.y as i32, p.z as i32].iter().enumerate() {
            lo[i] = lo[i].min(*c);
            hi[i] = hi[i].max(*c);
        }
    }
    println!(
        "verts={} local_to_world_q12={} min={:?} max={:?}",
        model.vertex_count(),
        model.local_to_world_q12(),
        lo,
        hi
    );
}
