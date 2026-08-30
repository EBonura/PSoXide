// SPDX-License-Identifier: GPL-2.0-or-later
//! Colour quantisation onto the PS1's RGB555 CLUT.
//!
//! The palette a PS1 texture can actually hold is a set of at most 16 (4bpp)
//! or 256 (8bpp) points on the 32×32×32 RGB555 lattice. Anything the quantiser
//! decides in a finer space has to survive that snap, so this solver treats the
//! lattice as the search space from the start rather than as a lossy encoding
//! step bolted on at the end.
//!
//! Four stages:
//!
//! 1. [`Histogram`] folds the source into RGB555 buckets, keeping each
//!    bucket's exact 8-bit weighted mean so sub-lattice detail still steers the
//!    clustering. The working set is then bounded at 32768 regardless of how
//!    big the input is.
//! 2. [`seed`] splits clusters by population-weighted squared Oklab error
//!    along their principal axis. Weighting by population is the whole point:
//!    splitting on raw channel range, as plain median cut does, hands the
//!    palette to whichever handful of texels are loudest and starves the mass
//!    of the picture.
//! 3. [`lloyd`] refines the centres.
//! 4. [`snap`] moves each centre onto the lattice, and [`refill`] reclaims any
//!    entry that collided with another or ended up with no pixels.
//!
//! Every stage is deterministic: buckets come out of a `BTreeMap`, ties break
//! on index, and [`crate::perceptual`] avoids libm.

use std::collections::BTreeMap;

use crate::perceptual::{distance_squared, oklab};

/// Cluster refinement passes. Lloyd converges fast on inputs this small and
/// exits early when no bucket changes hands, so the cap is only a backstop.
const LLOYD_ROUNDS: usize = 32;

/// Power iterations used to find a cluster's principal axis.
const POWER_ITERATIONS: usize = 32;

/// 8-bit RGB -> the CLUT word that stores it, truncating like the PS1 encoder.
pub fn to_word(rgb: [u8; 3]) -> u16 {
    (rgb[0] as u16 >> 3) | ((rgb[1] as u16 >> 3) << 5) | ((rgb[2] as u16 >> 3) << 10)
}

/// CLUT word -> the 8-bit RGB it renders as.
pub fn from_word(word: u16) -> [u8; 3] {
    let expand = |value: u16| -> u8 {
        let value = (value & 0x1F) as u8;
        (value << 3) | (value >> 2)
    };
    [expand(word), expand(word >> 5), expand(word >> 10)]
}

/// A distinct source colour and how many texels wear it.
struct Sample {
    rgb: [u8; 3],
    lab: [f64; 3],
    weight: f64,
}

/// One RGB555 cell's worth of source colour.
///
/// Colours inside a cell are indistinguishable to the CLUT, but their *mean*
/// is not: a cell holding mostly black with a few bright texels should pull a
/// centre differently than one holding the cell's midpoint uniformly, so the
/// exact 8-bit weighted mean is what gets clustered.
struct Bucket {
    lab: [f64; 3],
    weight: f64,
    word: u16,
}

/// The source image reduced to what the palette search needs.
struct Histogram {
    buckets: Vec<Bucket>,
    samples: Vec<Sample>,
}

impl Histogram {
    fn build(pixels: &[[u8; 3]]) -> Self {
        let mut tally: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        for pixel in pixels {
            *tally.entry(*pixel).or_insert(0) += 1;
        }
        let samples: Vec<Sample> = tally
            .iter()
            .map(|(rgb, count)| Sample {
                rgb: *rgb,
                lab: oklab(*rgb),
                weight: *count as f64,
            })
            .collect();

        let mut cells: BTreeMap<u16, ([u64; 3], u64)> = BTreeMap::new();
        for (rgb, count) in &tally {
            let cell = cells.entry(to_word(*rgb)).or_insert(([0; 3], 0));
            for (sum, channel) in cell.0.iter_mut().zip(rgb) {
                *sum += u64::from(*channel) * count;
            }
            cell.1 += count;
        }
        let buckets: Vec<Bucket> = cells
            .iter()
            .map(|(word, (sums, count))| {
                let mean = [
                    (sums[0] / count) as u8,
                    (sums[1] / count) as u8,
                    (sums[2] / count) as u8,
                ];
                Bucket {
                    lab: oklab(mean),
                    weight: *count as f64,
                    word: *word,
                }
            })
            .collect();

        Self { buckets, samples }
    }
}

/// A working cluster during the seeding split.
struct Cluster {
    members: Vec<usize>,
    weight: f64,
    mean: [f64; 3],
    /// Population-weighted squared Oklab error, i.e. how much picture is
    /// currently being served badly by this one entry.
    error: f64,
}

impl Cluster {
    fn new(members: Vec<usize>, buckets: &[Bucket]) -> Self {
        let mut weight = 0.0;
        let mut mean = [0.0f64; 3];
        for &index in &members {
            let bucket = &buckets[index];
            weight += bucket.weight;
            for (total, axis) in mean.iter_mut().zip(bucket.lab) {
                *total += axis * bucket.weight;
            }
        }
        if weight > 0.0 {
            for value in &mut mean {
                *value /= weight;
            }
        }
        let mut error = 0.0;
        for &index in &members {
            let bucket = &buckets[index];
            error += bucket.weight * distance_squared(&bucket.lab, &mean);
        }
        Self {
            members,
            weight,
            mean,
            error,
        }
    }

    fn splittable(&self) -> bool {
        self.members.len() >= 2 && self.error > 0.0
    }
}

/// Direction of greatest weighted spread, by power iteration on the cluster's
/// covariance. Normalising by the largest component instead of the vector
/// length keeps this to `+ - * /`.
fn principal_axis(cluster: &Cluster, buckets: &[Bucket]) -> [f64; 3] {
    let mut covariance = [[0.0f64; 3]; 3];
    for &index in &cluster.members {
        let bucket = &buckets[index];
        let delta = [
            bucket.lab[0] - cluster.mean[0],
            bucket.lab[1] - cluster.mean[1],
            bucket.lab[2] - cluster.mean[2],
        ];
        for row in 0..3 {
            for column in 0..3 {
                covariance[row][column] += bucket.weight * delta[row] * delta[column];
            }
        }
    }

    let mut axis = [1.0f64, 0.0, 0.0];
    for _ in 0..POWER_ITERATIONS {
        let mut next = [0.0f64; 3];
        for row in 0..3 {
            for column in 0..3 {
                next[row] += covariance[row][column] * axis[column];
            }
        }
        let scale = next
            .iter()
            .fold(0.0f64, |accumulator, value| accumulator.max(value.abs()));
        if scale == 0.0 {
            // Degenerate covariance (every member sits on the mean); the
            // caller only splits clusters with non-zero error, so fall back to
            // lightness and let the median cut still separate them.
            return [1.0, 0.0, 0.0];
        }
        for value in &mut next {
            *value /= scale;
        }
        axis = next;
    }
    axis
}

/// Split into `k` clusters, always cutting whichever one currently carries the
/// most population-weighted error.
fn seed(buckets: &[Bucket], k: usize) -> Vec<Cluster> {
    let mut clusters = vec![Cluster::new((0..buckets.len()).collect(), buckets)];

    while clusters.len() < k {
        let Some(target) = clusters
            .iter()
            .enumerate()
            .filter(|(_, cluster)| cluster.splittable())
            .max_by(|(left_index, left), (right_index, right)| {
                // Ties break on the lower index so the split order never
                // depends on iteration order.
                left.error
                    .partial_cmp(&right.error)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(right_index.cmp(left_index))
            })
            .map(|(index, _)| index)
        else {
            // Fewer distinct colours than palette entries; `refill` pads out
            // the rest with distinct words.
            break;
        };

        let cluster = &clusters[target];
        let axis = principal_axis(cluster, buckets);
        let mut ordered = cluster.members.clone();
        ordered.sort_by(|&left, &right| {
            let project = |index: usize| {
                let lab = &buckets[index].lab;
                lab[0] * axis[0] + lab[1] * axis[1] + lab[2] * axis[2]
            };
            project(left)
                .partial_cmp(&project(right))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.cmp(&right))
        });

        // Cut at the weighted median so both halves carry comparable
        // population rather than comparable member counts.
        let half = cluster.weight / 2.0;
        let mut running = 0.0;
        let mut cut = 0;
        for (position, &index) in ordered.iter().enumerate() {
            running += buckets[index].weight;
            if running >= half {
                cut = position + 1;
                break;
            }
        }
        let cut = cut.clamp(1, ordered.len() - 1);

        let high = ordered.split_off(cut);
        clusters[target] = Cluster::new(ordered, buckets);
        clusters.push(Cluster::new(high, buckets));
    }

    clusters
}

/// Weighted Lloyd refinement in Oklab.
fn lloyd(buckets: &[Bucket], centres: &mut [[f64; 3]]) {
    let mut labels = vec![usize::MAX; buckets.len()];
    for _ in 0..LLOYD_ROUNDS {
        let mut moved = false;
        for (label, bucket) in labels.iter_mut().zip(buckets) {
            let nearest = nearest_centre(&bucket.lab, centres);
            if *label != nearest {
                *label = nearest;
                moved = true;
            }
        }
        if !moved {
            return;
        }

        let mut sums = vec![[0.0f64; 3]; centres.len()];
        let mut weights = vec![0.0f64; centres.len()];
        for (label, bucket) in labels.iter().zip(buckets) {
            weights[*label] += bucket.weight;
            for (total, axis) in sums[*label].iter_mut().zip(bucket.lab) {
                *total += axis * bucket.weight;
            }
        }
        for (centre, (sum, weight)) in centres.iter_mut().zip(sums.iter().zip(&weights)) {
            if *weight > 0.0 {
                for (value, total) in centre.iter_mut().zip(sum) {
                    *value = total / weight;
                }
            }
        }
    }
}

fn nearest_centre(lab: &[f64; 3], centres: &[[f64; 3]]) -> usize {
    let mut best = 0usize;
    let mut best_distance = f64::INFINITY;
    for (index, centre) in centres.iter().enumerate() {
        let distance = distance_squared(lab, centre);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best
}

/// The 26 lattice words one step away from `word` in RGB555.
fn lattice_neighbours(word: u16) -> Vec<u16> {
    let (r, g, b) = (
        (word & 0x1F) as i32,
        ((word >> 5) & 0x1F) as i32,
        ((word >> 10) & 0x1F) as i32,
    );
    let mut out = Vec::with_capacity(26);
    for dr in -1..=1 {
        for dg in -1..=1 {
            for db in -1..=1 {
                if dr == 0 && dg == 0 && db == 0 {
                    continue;
                }
                let (nr, ng, nb) = (r + dr, g + dg, b + db);
                if (0..32).contains(&nr) && (0..32).contains(&ng) && (0..32).contains(&nb) {
                    out.push((nr as u16) | ((ng as u16) << 5) | ((nb as u16) << 10));
                }
            }
        }
    }
    out
}

/// Walk a centre onto the lattice point nearest it in Oklab.
///
/// Starts from `start` -- a real colour the cluster contains -- and hill-climbs
/// the lattice. Going through the source rather than inverting Oklab keeps the
/// whole solver on the forward transform, which is the half that is cheap to
/// make bit-reproducible.
fn snap(centre: &[f64; 3], start: u16) -> u16 {
    let mut word = start;
    let mut best = distance_squared(&oklab(from_word(word)), centre);
    loop {
        let mut improved = None;
        for candidate in lattice_neighbours(word) {
            let distance = distance_squared(&oklab(from_word(candidate)), centre);
            if distance < best {
                best = distance;
                improved = Some(candidate);
            }
        }
        match improved {
            Some(next) => word = next,
            None => return word,
        }
    }
}

/// Give every palette entry something to do.
///
/// Two ways an entry ends up wasted: it snapped onto a word another entry
/// already holds, or it is distinct but never the closest match for any texel.
/// Both are the same fix -- hand the entry to whichever source colour the rest
/// of the palette currently serves worst, which is exactly where a spare entry
/// buys the most.
fn refill(words: &mut Vec<u16>, wanted: usize, samples: &[Sample]) {
    let mut taken: Vec<u16> = Vec::with_capacity(wanted);
    for word in words.iter() {
        if !taken.contains(word) {
            taken.push(*word);
        }
    }

    // Rebuild in the surviving order, then top back up to `wanted`.
    *words = taken;
    while words.len() < wanted {
        let palette: Vec<[f64; 3]> = words.iter().map(|w| oklab(from_word(*w))).collect();
        let Some(candidate) = worst_served(samples, &palette, words) else {
            // The source has fewer distinct RGB555 colours than the CLUT has
            // slots. The leftovers cannot be reached by any texel whatever we
            // put in them, so they repeat the last real colour, matching how
            // `encode_clut` pads a short palette.
            let filler = words.last().copied().unwrap_or(0);
            words.resize(wanted, filler);
            break;
        };
        words.push(candidate);
    }

    // Now that the palette is full, evict anything no texel reaches. Each pass
    // must strictly reduce the dead count or we stop, so this cannot spin.
    let mut dead_before = usize::MAX;
    for _ in 0..wanted {
        let dead = dead_slots(words, samples);
        if dead.is_empty() || dead.len() >= dead_before {
            return;
        }
        dead_before = dead.len();
        for slot in dead {
            let palette: Vec<[f64; 3]> = words.iter().map(|w| oklab(from_word(*w))).collect();
            let Some(candidate) = worst_served(samples, &palette, words) else {
                return;
            };
            words[slot] = candidate;
        }
    }
}

/// Palette slots no sample resolves to.
fn dead_slots(words: &[u16], samples: &[Sample]) -> Vec<usize> {
    let palette: Vec<[f64; 3]> = words.iter().map(|w| oklab(from_word(*w))).collect();
    let mut referenced = vec![false; words.len()];
    for sample in samples {
        referenced[nearest_centre(&sample.lab, &palette)] = true;
    }
    (0..words.len()).filter(|slot| !referenced[*slot]).collect()
}

/// The lattice word for the source colour the current palette serves worst,
/// skipping words the palette already holds.
///
/// The word is the *snapped* one, not plain truncation: truncation can land
/// further from the colour than a neighbouring lattice point does, in which
/// case the entry we just spent would lose its own sample to a neighbour and
/// stay dead.
fn worst_served(samples: &[Sample], palette: &[[f64; 3]], taken: &[u16]) -> Option<u16> {
    let mut ranked: Vec<(f64, usize)> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let nearest = nearest_centre(&sample.lab, palette);
            (
                sample.weight * distance_squared(&sample.lab, &palette[nearest]),
                index,
            )
        })
        .collect();
    // Worst first; ties fall back to sample order, which is `BTreeMap` order.
    ranked.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.1.cmp(&right.1))
    });

    // Snapping is the expensive half, so only the candidates we actually reach
    // pay for it.
    ranked.into_iter().find_map(|(_, index)| {
        let sample = &samples[index];
        let word = snap(&sample.lab, to_word(sample.rgb));
        (!taken.contains(&word)).then_some(word)
    })
}

/// Quantise `pixels` to `n_entries` RGB555-exact colours.
///
/// Returns the palette in ascending CLUT-word order and one index per input
/// pixel. Palette channels are always 8-bit expansions of 5-bit values, so
/// `encode_clut` stores them without further loss and these indices stay valid
/// through to the cooked blob.
pub fn solve(pixels: &[[u8; 3]], n_entries: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let histogram = Histogram::build(pixels);
    if histogram.samples.is_empty() {
        return (vec![[0, 0, 0]; n_entries], vec![0; pixels.len()]);
    }

    let clusters = seed(&histogram.buckets, n_entries);
    let mut centres: Vec<[f64; 3]> = clusters.iter().map(|cluster| cluster.mean).collect();
    lloyd(&histogram.buckets, &mut centres);

    // Snap each centre onto the lattice, starting from a colour its own
    // cluster actually contains.
    let mut words: Vec<u16> = Vec::with_capacity(centres.len());
    for (index, centre) in centres.iter().enumerate() {
        let start = clusters
            .get(index)
            .and_then(|cluster| {
                cluster
                    .members
                    .iter()
                    .min_by(|&&left, &&right| {
                        let distance =
                            |i: usize| distance_squared(&histogram.buckets[i].lab, centre);
                        distance(left)
                            .partial_cmp(&distance(right))
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(left.cmp(&right))
                    })
                    .map(|&member| histogram.buckets[member].word)
            })
            .unwrap_or(0);
        words.push(snap(centre, start));
    }

    refill(&mut words, n_entries, &histogram.samples);
    words.sort_unstable();

    let palette_lab: Vec<[f64; 3]> = words.iter().map(|w| oklab(from_word(*w))).collect();
    let lookup: BTreeMap<[u8; 3], u8> = histogram
        .samples
        .iter()
        .map(|sample| (sample.rgb, nearest_centre(&sample.lab, &palette_lab) as u8))
        .collect();

    let palette: Vec<[u8; 3]> = words.iter().map(|w| from_word(*w)).collect();
    let indices: Vec<u8> = pixels.iter().map(|pixel| lookup[pixel]).collect();
    (palette, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_entries(palette: &[[u8; 3]], indices: &[u8]) -> usize {
        let mut used = vec![false; palette.len()];
        for &index in indices {
            used[index as usize] = true;
        }
        used.iter().filter(|seen| !**seen).count()
    }

    #[test]
    fn palette_is_exactly_representable_in_rgb555() {
        let pixels: Vec<[u8; 3]> = (0..64)
            .map(|i| [(i * 3) as u8, (255 - i * 3) as u8, (i * 2) as u8])
            .collect();
        let (palette, _) = solve(&pixels, 16);
        for entry in &palette {
            assert_eq!(from_word(to_word(*entry)), *entry, "{entry:?} lost bits");
        }
    }

    #[test]
    fn palette_words_are_distinct() {
        let pixels: Vec<[u8; 3]> = (0..256).map(|i| [i as u8, i as u8, i as u8]).collect();
        let (palette, _) = solve(&pixels, 16);
        let mut words: Vec<u16> = palette.iter().map(|c| to_word(*c)).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), 16, "duplicate CLUT words waste palette budget");
    }

    /// A dark mass carrying nearly all the pixels, plus a small high-chroma
    /// accent with a much wider raw channel range. Splitting on range hands
    /// the palette to the accent and leaves entries with nothing to do;
    /// splitting on population-weighted error puts them in the dark mass,
    /// where the picture is.
    fn dark_mass_with_bright_accent() -> Vec<[u8; 3]> {
        let mut pixels = Vec::new();
        for step in 0..=24u32 {
            let value = step * 5;
            for _ in 0..160 {
                pixels.push([value as u8, (value * 3 / 4) as u8, (value / 2) as u8]);
            }
        }
        for i in 0..60u32 {
            pixels.push([250, (200 + i % 20) as u8, 40]);
        }
        pixels
    }

    #[test]
    fn dark_mass_with_bright_accent_wastes_no_entries() {
        let pixels = dark_mass_with_bright_accent();
        let (palette, indices) = solve(&pixels, 16);
        assert_eq!(palette.len(), 16);
        assert_eq!(dead_entries(&palette, &indices), 0);
    }

    #[test]
    fn palette_follows_population_not_channel_range() {
        let pixels = dark_mass_with_bright_accent();
        let (palette, _) = solve(&pixels, 16);
        // 98.5% of the texels are in the dark ramp, so the overwhelming
        // majority of the budget belongs to it.
        let dark = palette
            .iter()
            .filter(|entry| entry.iter().all(|channel| *channel < 140))
            .count();
        assert!(dark >= 13, "only {dark}/16 entries serve the dark mass");
    }

    /// When the source holds fewer distinct RGB555 colours than the CLUT has
    /// slots, the extra slots are unreachable no matter what goes in them. What
    /// must hold is that every source colour got its own entry -- the palette
    /// only pads once it has run out of picture.
    #[test]
    fn fewer_colours_than_entries_spends_every_reachable_slot_first() {
        let pixels = vec![[10, 20, 30], [200, 100, 50], [0, 0, 0]];
        let (palette, indices) = solve(&pixels, 16);
        assert_eq!(palette.len(), 16);
        assert_eq!(indices.len(), 3);

        let mut chosen: Vec<[u8; 3]> = indices
            .iter()
            .map(|index| palette[*index as usize])
            .collect();
        chosen.sort_unstable();
        chosen.dedup();
        assert_eq!(chosen.len(), 3, "distinct source colours were merged");
        for (pixel, index) in pixels.iter().zip(&indices) {
            let target = oklab(*pixel);
            let chosen = distance_squared(&oklab(palette[*index as usize]), &target);
            let truncated = distance_squared(&oklab(from_word(to_word(*pixel))), &target);
            assert!(
                chosen <= truncated,
                "{pixel:?} landed worse than truncation"
            );
        }
    }

    /// A flat colour must land on the lattice point nearest it in Oklab --
    /// which is not always the one plain channel truncation would pick, since
    /// Oklab distance mixes the channels.
    #[test]
    fn flat_colour_lands_on_the_nearest_lattice_point() {
        let source = [64u8, 128, 192];
        let pixels = vec![source; 32];
        let (palette, indices) = solve(&pixels, 16);
        let chosen = to_word(palette[indices[0] as usize]);

        let target = oklab(source);
        let best = distance_squared(&oklab(from_word(chosen)), &target);
        for neighbour in lattice_neighbours(chosen) {
            let distance = distance_squared(&oklab(from_word(neighbour)), &target);
            assert!(
                distance >= best,
                "0x{neighbour:04x} beats the chosen 0x{chosen:04x}"
            );
        }
        assert!(
            best <= distance_squared(&oklab(from_word(to_word(source))), &target),
            "truncation beat the snap"
        );
    }

    #[test]
    fn repeated_runs_are_identical() {
        let pixels: Vec<[u8; 3]> = (0..2048)
            .map(|i| [(i % 251) as u8, (i * 7 % 253) as u8, (i * 13 % 247) as u8])
            .collect();
        let first = solve(&pixels, 16);
        let second = solve(&pixels, 16);
        assert_eq!(first, second);
    }
}
