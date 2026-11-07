//! Read-only semantic comparison, ignoring RON formatting and omitted defaults.
use psxed_project::ProjectDocument;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: compare_projects before.ron after.ron".into());
    }
    let load = |path: &str| -> Result<ProjectDocument, Box<dyn std::error::Error>> {
        Ok(ProjectDocument::from_ron_str(&std::fs::read_to_string(
            path,
        )?)?)
    };
    let a = load(&args[0])?;
    let b = load(&args[1])?;
    println!("Semantically equal: {}", a == b);
    let left = a.to_ron_string()?;
    let right = b.to_ron_string()?;
    for (i, (a, b)) in left
        .lines()
        .zip(right.lines())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(30)
    {
        println!("Line {}: {} -> {}", i + 1, a, b);
    }
    Ok(())
}
