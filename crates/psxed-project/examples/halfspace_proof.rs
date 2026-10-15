//! Standalone offline experiment; compile with rustc, no shipping linkage.
#[path = "../src/solid_cell_constraints.rs"]
mod proof;
use proof::{reduce, verify, Halfspace};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let mut lines = text.lines();
    let mut total = 0;
    let mut kept = 0;
    let mut proofs = String::new();
    while let Some(header) = lines.next() {
        let h: Vec<usize> = header
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        let terms: Vec<_> = (0..h[2])
            .map(|_| {
                let v: Vec<i64> = lines
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(|s| s.parse().unwrap())
                    .collect();
                Halfspace {
                    normal: [v[0], v[1], v[2]],
                    bound: v[3],
                    strict: v[4] != 0,
                }
            })
            .collect();
        let result = reduce(&terms, h[1]);
        let mut active: Vec<_> = (0..terms.len()).collect();
        for p in &result.proofs {
            proofs.push_str(&format!(
                "{} {} {} {} {}\n",
                h[0],
                p.removed,
                p.denominator,
                p.support
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                p.numerators
                    .iter()
                    .map(i128::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            assert!(verify(&terms, &active, p));
            active.retain(|&i| i != p.removed);
        }
        assert_eq!(result.retained, active);
        let original: Vec<_> = active.iter().copied().filter(|&i| i < h[1]).collect();
        total += h[1];
        kept += original.len();
        println!(
            "{} {} {}",
            h[0],
            h[1],
            original
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    if let Some(path) = args.get(2) {
        std::fs::write(path, proofs).unwrap();
    }
    eprintln!("original={total} retained={kept}");
}
