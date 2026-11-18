//! Swap who drives whom and assert the fights stay symmetric.
//!
//! The fighters are grid-dungeon's training enemy and Aletha, both cooked
//! as entities by the project's own cook (`psoxide_duel::project`). The
//! statistical tests run N = 200 seeds in the walled room and treat each
//! decided fight (a death on one side) as one coin toss: a fair matchup
//! lands side A's share of the decided fights inside 50% plus or minus
//! 2.58 sqrt(1/4n), a 99% interval (about 9 points at n = 200), 99 times in
//! 100. The seeds are fixed, so a result is the same on every run; the
//! interval says how surprised to be by it.

use std::sync::OnceLock;

use psoxide_duel::arena::{Controller, Fighter, Layout, Outcome, PadScript};
use psoxide_duel::project::{self, ProjectFighters};
use psoxide_duel::sweep::{self, Matchup, Summary};

const SEEDS: u32 = 200;

struct Placed {
    loaded: ProjectFighters,
    aletha: Fighter,
    enemy: Fighter,
}

fn placed() -> &'static Placed {
    static PLACED: OnceLock<Placed> = OnceLock::new();
    PLACED.get_or_init(|| {
        let dir = psxed_project::projects_dir().join("grid-dungeon");
        assert!(
            dir.join("project.ron").is_file(),
            "grid-dungeon not found under {} (set PSOXIDE_PROJECTS_DIR)",
            dir.display()
        );
        let loaded = project::load(&dir).expect("grid-dungeon cooks");
        Placed {
            aletha: Fighter::new(loaded.aletha.clone()),
            enemy: Fighter::new(loaded.enemy.clone()),
            loaded,
        }
    })
}

fn matchup<'a>(
    p: &'a Placed,
    a: &'a Fighter,
    b: &'a Fighter,
    controllers: [Controller; 2],
) -> Matchup<'a> {
    Matchup {
        name: String::new(),
        fighters: [a, b],
        controllers,
        layout: Layout::Room,
        distance: p.loaded.start_distance,
    }
}

/// The 99% half-width around 50% for `decided` coin tosses, in percent.
fn half_width_99(decided: u32) -> f64 {
    258.0 * (0.25 / f64::from(decided.max(1))).sqrt()
}

fn assert_fair(summary: &Summary, what: &str) {
    let decided = summary.a_won() + summary.b_won();
    // Stalls and timeouts are not coin tosses; enough decided fights must
    // remain for the interval to mean something.
    assert!(
        decided >= SEEDS / 5,
        "{what}: only {decided} of {SEEDS} fights decided"
    );
    let share = summary.a_share_percent();
    let tolerance = half_width_99(decided);
    assert!(
        (share - 50.0).abs() <= tolerance,
        "{what}: side A won {share:.1}% of {decided} decided fights, outside 50 +/- {tolerance:.1}"
    );
}

const AI: Controller = Controller::Ai;
const PAD: Controller = Controller::Pad(PadScript::Rush);

#[test]
fn the_enemy_ai_mirror_is_a_coin_toss() {
    let p = placed();
    let s = sweep::sweep(&matchup(p, &p.enemy, &p.enemy, [AI, AI]), 1..=SEEDS);
    assert_fair(&s, "enemy AI vs enemy AI");
}

#[test]
fn the_aletha_ai_mirror_is_a_coin_toss() {
    let p = placed();
    let s = sweep::sweep(&matchup(p, &p.aletha, &p.aletha, [AI, AI]), 1..=SEEDS);
    assert_fair(&s, "Aletha AI vs Aletha AI");
}

/// The same scripted pad on the same character from mirrored starts: the
/// rules give both sides the same thing at the same tick, so a fight has no
/// winner (a double KO when the blade reaches the other body from where the
/// pad stops, a stall when it does not: grid-dungeon's Light swings under
/// the other Light's one torso hurtbox), and the two sides' tallies are
/// equal. Any difference between the sides (processing order, a rule that
/// reads which side it is on) would break the tie. The posed blades and
/// hurtboxes are not bit-symmetric under the half turn between the starts
/// (Q12 rotation rounding), so a contact at the very edge of reach may land
/// for one side only: a few fights of twenty may have a winner, either way.
#[test]
fn a_pad_mirror_has_no_winner_but_by_rounding() {
    let p = placed();
    for fighter in [&p.enemy, &p.aletha] {
        let s = sweep::sweep(&matchup(p, fighter, fighter, [PAD, PAD]), 1..=20);
        let tied = s.count(Outcome::DoubleKo) + s.count(Outcome::Stalled);
        assert!(
            tied >= 16,
            "{}: {:?}",
            fighter.spec.name,
            s.runs.iter().map(|r| r.outcome).collect::<Vec<_>>()
        );
        for run in s
            .runs
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::DoubleKo | Outcome::Stalled))
        {
            assert_eq!(run.sides[0], run.sides[1], "seed {}", run.seed);
        }
    }
}

/// Swapping the sides of a matchup (each fighter keeps its controller and
/// its seed) gives each fighter the same share of wins.
fn assert_side_swap_agrees(a: &Fighter, b: &Fighter, controllers: [Controller; 2], what: &str) {
    let p = placed();
    let forward = sweep::sweep(&matchup(p, a, b, controllers), 1..=SEEDS);
    let swapped_controllers = [controllers[1], controllers[0]];
    let backward = sweep::sweep(&matchup(p, b, a, swapped_controllers), 1..=SEEDS);
    let a_forward = forward.a_won();
    let a_backward = backward.b_won();
    let decided = (forward.a_won() + forward.b_won() + backward.a_won() + backward.b_won()) / 2;
    // Two independent shares of the same matchup: their difference has
    // twice the variance of one, so the 99% band is sqrt(2) wider.
    let tolerance = half_width_99(decided) * std::f64::consts::SQRT_2;
    let share =
        |won: u32, s: &Summary| f64::from(won) * 100.0 / f64::from((s.a_won() + s.b_won()).max(1));
    let (f, b_) = (share(a_forward, &forward), share(a_backward, &backward));
    assert!(
        (f - b_).abs() <= tolerance,
        "{what}: the first fighter won {f:.1}% on side A and {b_:.1}% on side B (tolerance {tolerance:.1})"
    );
}

#[test]
fn ai_aletha_against_a_pad_enemy_does_not_depend_on_the_side() {
    let p = placed();
    assert_side_swap_agrees(&p.aletha, &p.enemy, [AI, PAD], "AI Aletha vs pad enemy");
}

#[test]
fn ai_against_ai_does_not_depend_on_the_side() {
    let p = placed();
    assert_side_swap_agrees(&p.aletha, &p.enemy, [AI, AI], "AI Aletha vs AI enemy");
}

/// The possession seam drives the fighter: a pad that stands still never
/// attacks or swaps, whatever the AI would have done, and never wins.
#[test]
fn a_standing_pad_holds_its_fighter() {
    let p = placed();
    let stand = Controller::Pad(PadScript::Stand);
    let s = sweep::sweep(&matchup(p, &p.enemy, &p.enemy, [stand, AI]), 1..=10);
    for run in &s.runs {
        assert_eq!(run.sides[0].attacks, 0, "seed {}", run.seed);
        assert_eq!(run.sides[0].swaps, 0, "seed {}", run.seed);
        assert_ne!(run.outcome, Outcome::AWon, "seed {}", run.seed);
    }
}
