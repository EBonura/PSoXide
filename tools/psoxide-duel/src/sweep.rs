//! Seed sweeps over a matchup, and their summary.

use serde_json::{json, Value};

use crate::arena::{self, Controller, Duel, Fighter, Layout, Outcome};

/// The enemy's seed on the disc is the duel seed mixed with this
/// (`seed_training_actor(seed ^ 0x9e3779b9)`); side B gets the same mix.
pub const SIDE_B_SEED_MIX: u32 = 0x9e37_79b9;

/// Two fighters, who drives them, where they fight.
#[derive(Clone, Debug)]
pub struct Matchup<'a> {
    /// Short name, `a-vs-b`.
    pub name: String,
    /// Side A, side B.
    pub fighters: [&'a Fighter; 2],
    /// Who drives each side.
    pub controllers: [Controller; 2],
    /// The ground.
    pub layout: Layout,
    /// The swap cooldown every fighter uses.
    pub swap_cooldown_ticks: u16,
    /// Start distance the fighters were placed with.
    pub distance: i32,
}

/// One seed's result, compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedResult {
    /// The duel seed (side A's; side B's is mixed from it).
    pub seed: u32,
    /// How it ended.
    pub outcome: Outcome,
    /// The tick it ended on.
    pub end_tick: u32,
    /// Side tallies.
    pub sides: [arena::SideTally; 2],
}

/// A matchup over many seeds.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Every seed, in order.
    pub runs: Vec<SeedResult>,
}

impl Summary {
    /// Fights side A won.
    pub fn a_won(&self) -> u32 {
        self.count(Outcome::AWon)
    }

    /// Fights side B won.
    pub fn b_won(&self) -> u32 {
        self.count(Outcome::BWon)
    }

    /// Fights that ended with `outcome`.
    pub fn count(&self, outcome: Outcome) -> u32 {
        self.runs.iter().filter(|r| r.outcome == outcome).count() as u32
    }

    /// Side A's share of the decided fights (wins over wins plus losses), in
    /// percent; 50 when none was decided.
    pub fn a_share_percent(&self) -> f64 {
        let decided = self.a_won() + self.b_won();
        if decided == 0 {
            50.0
        } else {
            f64::from(self.a_won()) * 100.0 / f64::from(decided)
        }
    }

    /// Half-width, in percent, of the 95% interval around a 50/50 share for
    /// this many decided fights (normal approximation of the binomial,
    /// 1.96 sqrt(p(1-p)/n) at p = 1/2): a fair mirror lands inside 50 plus
    /// or minus this 19 times in 20.
    pub fn fair_half_width_percent(&self) -> f64 {
        let decided = f64::from(self.a_won() + self.b_won()).max(1.0);
        196.0 * (0.25 / decided).sqrt()
    }

    /// Sum of one tally over every run, per side.
    pub fn total(&self, pick: impl Fn(&arena::SideTally) -> u32) -> [u32; 2] {
        let mut sum = [0; 2];
        for run in &self.runs {
            for (n, side) in run.sides.iter().enumerate() {
                sum[n] += pick(side);
            }
        }
        sum
    }

    /// Mean fight length in ticks.
    pub fn mean_length(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        self.runs.iter().map(|r| f64::from(r.end_tick)).sum::<f64>() / self.runs.len() as f64
    }

    /// The summary as JSON, with the per-seed rows.
    pub fn to_json(&self, matchup: &Matchup<'_>) -> Value {
        let pair = |v: [u32; 2]| json!(v);
        json!({
            "matchup": matchup.name,
            "fighters": [matchup.fighters[0].spec.name, matchup.fighters[1].spec.name],
            "controllers": [matchup.controllers[0].name(), matchup.controllers[1].name()],
            "layout": matchup.layout.name(),
            "start_distance": matchup.distance,
            "seeds": self.runs.len(),
            "a_won": self.a_won(),
            "b_won": self.b_won(),
            "double_ko": self.count(Outcome::DoubleKo),
            "timeouts": self.count(Outcome::Timeout),
            "stalls": self.count(Outcome::Stalled),
            "a_share_percent": self.a_share_percent(),
            "fair_half_width_percent": self.fair_half_width_percent(),
            "parries_a_b": pair(self.total(|s| s.parries)),
            "crits_a_b": pair(self.total(|s| s.crits)),
            "knockdowns_a_b": pair(self.total(|s| s.knockdowns)),
            "staggers_a_b": pair(self.total(|s| s.staggers)),
            "parry_swaps_a_b": pair(self.total(|s| s.parry_swaps)),
            "armour_absorbs_a_b": pair(self.total(|s| s.armour_absorbs)),
            "trades_a_b": pair(self.total(|s| s.trades)),
            "evades_a_b": pair(self.total(|s| s.evades)),
            "swaps_a_b": pair(self.total(|s| s.swaps)),
            "shots_a_b": pair(self.total(|s| s.shots)),
            "attacks_a_b": pair(self.total(|s| s.attacks)),
            "damage_taken_a_b": pair(self.total(|s| s.damage_taken)),
            "mean_length_ticks": self.mean_length(),
            "runs": self.runs.iter().map(|r| json!({
                "seed": r.seed,
                "outcome": r.outcome.disc_name(),
                "end_tick": r.end_tick,
                "parries_player_enemy": [r.sides[0].parries, r.sides[1].parries],
                "crits_player_enemy": [r.sides[0].crits, r.sides[1].crits],
                "knockdowns_player_enemy": [r.sides[0].knockdowns, r.sides[1].knockdowns],
                "staggers_player_enemy": [r.sides[0].staggers, r.sides[1].staggers],
                "evades_player_enemy": [r.sides[0].evades, r.sides[1].evades],
                "damage_taken_player_enemy": [r.sides[0].damage_taken, r.sides[1].damage_taken],
            })).collect::<Vec<_>>(),
        })
    }
}

/// Side A's and side B's seed for duel seed `seed`.
pub const fn seeds_of(seed: u32) -> [u32; 2] {
    [seed, seed ^ SIDE_B_SEED_MIX]
}

/// One fight of `matchup` at duel seed `seed`.
pub fn fight(matchup: &Matchup<'_>, seed: u32, log: bool) -> arena::DuelResult {
    arena::run(&Duel {
        fighters: matchup.fighters,
        controllers: matchup.controllers,
        seeds: seeds_of(seed),
        swap_cooldown_ticks: matchup.swap_cooldown_ticks,
        distance: matchup.distance,
        bearing: arena::bearing_of(seed),
        layout: matchup.layout,
        log,
    })
}

/// `matchup` over `seeds`.
pub fn sweep(matchup: &Matchup<'_>, seeds: impl IntoIterator<Item = u32>) -> Summary {
    Summary {
        runs: seeds
            .into_iter()
            .map(|seed| {
                let result = fight(matchup, seed, false);
                SeedResult {
                    seed,
                    outcome: result.outcome,
                    end_tick: result.end_tick,
                    sides: result.sides,
                }
            })
            .collect(),
    }
}
