//! Pictures in the MDEC's colour space, and scaling them to the movie size.
//!
//! The MDEC turns signed full-range YCbCr into RGB with the JPEG (BT.601
//! full range) weights, `R = Y + 1.402 Cr`, `G = Y - 0.3437 Cb - 0.7143 Cr`,
//! `B = Y + 1.772 Cb`, with every component biased by 128 on output. A
//! [`Planes`] holds exactly what the encoder hands the DCT: luma and chroma
//! as signed values around zero (`-128..=127`), at full precision.

use crate::y4m::{Frame, Header, Range};

/// A 4:2:0 picture in the MDEC's signed full-range YCbCr.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Planes {
    /// Luma width in pixels.
    pub width: usize,
    /// Luma height in pixels.
    pub height: usize,
    /// Luma minus 128, `width * height` samples.
    pub y: Vec<f64>,
    /// Blue difference, `ceil(width / 2) * ceil(height / 2)` samples.
    pub cb: Vec<f64>,
    /// Red difference, the size of [`Planes::cb`].
    pub cr: Vec<f64>,
}

impl Planes {
    /// Chroma plane width.
    #[must_use]
    pub fn chroma_width(&self) -> usize {
        self.width.div_ceil(2)
    }

    /// Chroma plane height.
    #[must_use]
    pub fn chroma_height(&self) -> usize {
        self.height.div_ceil(2)
    }

    /// A Y4M frame converted from its sample range to the MDEC's.
    #[must_use]
    pub fn from_y4m(header: &Header, frame: &Frame) -> Planes {
        let (luma, chroma) = match header.range {
            // Limited range stretches 219 luma and 224 chroma steps to 255.
            Range::Limited => ((16.0, 255.0 / 219.0), 255.0 / 224.0),
            Range::Full => ((0.0, 1.0), 1.0),
        };
        let y = frame
            .y
            .iter()
            .map(|&v| ((f64::from(v) - luma.0) * luma.1).clamp(0.0, 255.0) - 128.0)
            .collect();
        let c = |plane: &[u8]| {
            plane
                .iter()
                .map(|&v| ((f64::from(v) - 128.0) * chroma).clamp(-128.0, 127.0))
                .collect()
        };
        Planes {
            width: header.width,
            height: header.height,
            y,
            cb: c(&frame.cb),
            cr: c(&frame.cr),
        }
    }

    /// This picture scaled to `width` x `height` (both even), each plane by
    /// area averaging: every output sample is the mean of the source area it
    /// covers. Exact halving (640x480 to 320x240) is a 2x2 box.
    #[must_use]
    pub fn scaled(&self, width: usize, height: usize) -> Planes {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        Planes {
            width,
            height,
            y: scale(&self.y, (self.width, self.height), (width, height)),
            cb: scale(
                &self.cb,
                (self.chroma_width(), self.chroma_height()),
                (cw, ch),
            ),
            cr: scale(
                &self.cr,
                (self.chroma_width(), self.chroma_height()),
                (cw, ch),
            ),
        }
    }

    /// The picture as 8-bit RGB, rounded, through the MDEC's colour weights
    /// at full precision: the reference a decoded movie is measured against.
    #[must_use]
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 3);
        let cw = self.chroma_width();
        for row in 0..self.height {
            for col in 0..self.width {
                let y = self.y[row * self.width + col];
                let at = (row / 2) * cw + col / 2;
                let (cb, cr) = (self.cb[at], self.cr[at]);
                for v in [
                    y + 1.402 * cr,
                    y - 0.344_136 * cb - 0.714_136 * cr,
                    y + 1.772 * cb,
                ] {
                    out.push((v + 128.0).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
        out
    }
}

/// Per output sample along one axis: the first source sample it covers and
/// the weight of each covered sample (summing to one).
fn axis_weights(from: usize, to: usize) -> Vec<(usize, Vec<f64>)> {
    let step = from as f64 / to as f64;
    (0..to)
        .map(|i| {
            let (start, end) = (i as f64 * step, (i + 1) as f64 * step);
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(from);
            let weights = (first..last)
                .map(|s| ((s + 1) as f64).min(end) - (s as f64).max(start))
                .map(|w| w / step)
                .collect();
            (first, weights)
        })
        .collect()
}

/// `plane` (`from` = width, height) area-averaged to `to`.
fn scale(plane: &[f64], from: (usize, usize), to: (usize, usize)) -> Vec<f64> {
    if from == to {
        return plane.to_vec();
    }
    let (columns, rows) = (axis_weights(from.0, to.0), axis_weights(from.1, to.1));
    // Horizontal pass, then vertical.
    let mut wide = vec![0.0; to.0 * from.1];
    for r in 0..from.1 {
        let src = &plane[r * from.0..][..from.0];
        for (c, (first, weights)) in columns.iter().enumerate() {
            wide[r * to.0 + c] = weights.iter().zip(&src[*first..]).map(|(w, v)| w * v).sum();
        }
    }
    let mut out = vec![0.0; to.0 * to.1];
    for (r, (first, weights)) in rows.iter().enumerate() {
        for c in 0..to.0 {
            out[r * to.0 + c] = weights
                .iter()
                .enumerate()
                .map(|(k, w)| w * wide[(first + k) * to.0 + c])
                .sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halving_is_a_two_by_two_box() {
        let plane: Vec<f64> = (0..16).map(f64::from).collect();
        let out = scale(&plane, (4, 4), (2, 2));
        assert_eq!(out, vec![2.5, 4.5, 10.5, 12.5]);
    }

    #[test]
    fn area_weights_sum_to_one_for_odd_ratios() {
        for (from, to) in [(640, 320), (720, 320), (300, 320), (7, 3)] {
            for (_, w) in axis_weights(from, to) {
                assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{from} to {to}");
            }
        }
    }

    #[test]
    fn limited_range_reaches_the_ends_of_the_mdec_range() {
        let header = Header {
            width: 2,
            height: 2,
            fps: (15, 1),
            range: Range::Limited,
        };
        let frame = Frame {
            y: vec![16, 235, 16, 235],
            cb: vec![240],
            cr: vec![16],
        };
        let p = Planes::from_y4m(&header, &frame);
        assert_eq!(p.y, vec![-128.0, 127.0, -128.0, 127.0]);
        // Chroma 16 and 240 are 112 steps of 255 / 224 from the middle.
        assert_eq!((p.cb[0], p.cr[0]), (127.0, -127.5));
        // Grey stays grey through the colour weights.
        let grey = Planes {
            width: 2,
            height: 2,
            y: vec![0.0; 4],
            cb: vec![0.0],
            cr: vec![0.0],
        };
        assert!(grey.to_rgb().iter().all(|&v| v == 128));
    }
}
