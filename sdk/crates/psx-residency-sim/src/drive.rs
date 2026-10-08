//! The CD drive as the silicon measurements describe it.

/// Seek time by distance, measured on silicon at double speed: sectors and
/// milliseconds. Survey table; the swing between runs is about 2x.
pub const MEASURED_SEEK_MILLIS: [(u32, u32); 4] = [(1, 11), (16, 79), (128, 137), (512, 310)];

/// Microseconds per sector at double speed (6.49 ms), measured on silicon.
pub const READ_MICROS_PER_SECTOR_DOUBLE: u32 = 6_490;

/// Microseconds per sector at single speed (13.20 ms), measured on silicon.
pub const READ_MICROS_PER_SECTOR_SINGLE: u32 = 13_200;

/// How the run-to-run swing of seek time is modelled. The measured table is
/// one run; the survey reports the true value swinging by about 2x.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Variance {
    /// Seeks take the table value.
    Nominal,
    /// Every seek takes twice the table value: the design's pessimistic corner
    /// ("the swing applies to the seek only").
    Pessimistic,
    /// Each seek takes the table value times a factor drawn uniformly from
    /// 1.0 to 2.0 by a deterministic generator seeded with this value.
    Seeded(u64),
}

/// A drive: where the head is, how long a read takes, how long a seek takes.
#[derive(Clone, Debug)]
pub struct DriveModel {
    seek_millis: [(u32, u32); 4],
    far_seek_millis: u32,
    micros_per_sector: u32,
    abort_micros: u32,
    variance: Variance,
    rng: u64,
    head: Option<u32>,
}

impl DriveModel {
    /// Double speed, nominal seeks, the measured table, clamped beyond 512
    /// sectors, instant abort.
    pub fn double_speed() -> Self {
        Self {
            seek_millis: MEASURED_SEEK_MILLIS,
            far_seek_millis: MEASURED_SEEK_MILLIS[3].1,
            micros_per_sector: READ_MICROS_PER_SECTOR_DOUBLE,
            abort_micros: 0,
            variance: Variance::Nominal,
            rng: 0x9E37_79B9_7F4A_7C15,
            head: None,
        }
    }

    /// The same at single speed. The seek table was measured at double speed
    /// and is kept as is (an assumption).
    pub fn single_speed() -> Self {
        Self {
            micros_per_sector: READ_MICROS_PER_SECTOR_SINGLE,
            ..Self::double_speed()
        }
    }

    /// Choose how seek time varies between runs.
    pub fn with_variance(mut self, variance: Variance) -> Self {
        self.variance = variance;
        if let Variance::Seeded(seed) = variance {
            self.rng = seed | 1;
        }
        self
    }

    /// Seek time for any distance beyond the measured 512 sectors. Unmeasured:
    /// the default clamps to the 512-sector value.
    pub fn with_far_seek_millis(mut self, millis: u32) -> Self {
        self.far_seek_millis = millis;
        self
    }

    /// Time a cancelled read takes to stop (default 0: unmeasured).
    pub fn with_abort_micros(mut self, micros: u32) -> Self {
        self.abort_micros = micros;
        self
    }

    /// Time a cancelled read takes to stop.
    pub fn abort_micros(&self) -> u32 {
        self.abort_micros
    }

    /// Microseconds to read one sector.
    pub fn micros_per_sector(&self) -> u32 {
        self.micros_per_sector
    }

    /// The sector under the head, when known.
    pub fn head(&self) -> Option<u32> {
        self.head
    }

    /// Forget the head position (after an abort or a stop).
    pub fn lose_head(&mut self) {
        self.head = None;
    }

    /// The table seek time in microseconds for a jump of `distance` sectors,
    /// before variance. Linear between measured points (an assumption).
    pub fn nominal_seek_micros(&self, distance: u32) -> u32 {
        if distance == 0 {
            return 0;
        }
        let table = &self.seek_millis;
        if distance <= table[0].0 {
            return table[0].1 * 1000;
        }
        for pair in table.windows(2) {
            let (x0, y0) = (pair[0].0 as u64, pair[0].1 as u64 * 1000);
            let (x1, y1) = (pair[1].0 as u64, pair[1].1 as u64 * 1000);
            if distance as u64 <= x1 {
                return (y0 + (y1 - y0) * (distance as u64 - x0) / (x1 - x0)) as u32;
            }
        }
        self.far_seek_millis * 1000
    }

    fn next_factor_permille(&mut self) -> u64 {
        match self.variance {
            Variance::Nominal => 1000,
            Variance::Pessimistic => 2000,
            Variance::Seeded(_) => {
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 7;
                self.rng ^= self.rng << 17;
                1000 + (self.rng >> 11) % 1001
            }
        }
    }

    /// Time to read `sectors` sectors starting at `lba`, moving the head. A
    /// read that starts where the head already is needs no seek (the
    /// transport chains contiguous requests). Returns `(micros, seeked)`.
    pub fn read(&mut self, lba: u32, sectors: u32) -> (u64, bool) {
        let distance = match self.head {
            Some(head) if head == lba => 0,
            Some(head) => head.abs_diff(lba),
            // Unknown position: charge the far seek.
            None => u32::MAX,
        };
        let seek = if distance == 0 {
            0
        } else {
            let nominal = if distance == u32::MAX {
                self.far_seek_millis as u64 * 1000
            } else {
                self.nominal_seek_micros(distance) as u64
            };
            nominal * self.next_factor_permille() / 1000
        };
        self.head = Some(lba + sectors);
        (
            seek + sectors as u64 * self.micros_per_sector as u64,
            distance != 0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_points_are_reproduced_exactly() {
        let drive = DriveModel::double_speed();
        for (sectors, millis) in MEASURED_SEEK_MILLIS {
            assert_eq!(drive.nominal_seek_micros(sectors), millis * 1000);
        }
        assert_eq!(drive.nominal_seek_micros(0), 0);
        assert_eq!(drive.nominal_seek_micros(100_000), 310_000, "clamped");
    }

    #[test]
    fn interpolation_is_monotonic_between_points() {
        let drive = DriveModel::double_speed();
        let mut last = 0;
        for distance in 1..=600 {
            let micros = drive.nominal_seek_micros(distance);
            assert!(micros >= last, "seek time fell at {distance}");
            last = micros;
        }
        assert_eq!(
            drive.nominal_seek_micros(72),
            79_000 + (137_000 - 79_000) * 56 / 112
        );
    }

    #[test]
    fn a_contiguous_read_needs_no_seek_and_costs_only_the_transfer() {
        let mut drive = DriveModel::double_speed();
        let (first, seeked) = drive.read(1000, 16);
        assert!(seeked, "head position unknown at start");
        assert_eq!(first, 310_000 + 16 * 6_490);
        let (second, seeked) = drive.read(1016, 16);
        assert!(!seeked);
        assert_eq!(second, 16 * 6_490, "104 ms for a 16 sector region");
        let (third, seeked) = drive.read(1016 + 16 + 128, 16);
        assert!(seeked);
        assert_eq!(third, 137_000 + 16 * 6_490);
    }

    #[test]
    fn pessimistic_doubles_the_seek_only() {
        let mut drive = DriveModel::double_speed().with_variance(Variance::Pessimistic);
        drive.read(0, 1);
        let (micros, _) = drive.read(1 + 128, 16);
        assert_eq!(micros, 2 * 137_000 + 16 * 6_490);
    }

    #[test]
    fn seeded_variance_is_deterministic_and_within_one_to_two_times() {
        let run = |seed| {
            let mut drive = DriveModel::double_speed().with_variance(Variance::Seeded(seed));
            let mut out = alloc::vec::Vec::new();
            drive.read(0, 1);
            for step in 1..40u32 {
                let (micros, _) = drive.read(step * 1000, 1);
                out.push(micros - 6_490);
            }
            out
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
        for seek in run(5) {
            assert!((310_000..=620_000).contains(&seek), "{seek}");
        }
    }

    #[test]
    fn single_speed_reads_take_twice_as_long() {
        let mut drive = DriveModel::single_speed();
        drive.read(0, 1);
        assert_eq!(drive.read(1, 10).0, 10 * 13_200);
    }
}
