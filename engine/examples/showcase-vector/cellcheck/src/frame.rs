//! PPM frames.

/// An RGB frame.
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 3]>,
}

impl Frame {
    /// Read a binary (P6) PPM file.
    pub fn read(path: &str) -> Result<Self, String> {
        let d = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let mut pos = 0;
        let mut token = || -> Result<String, String> {
            while pos < d.len() && d[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let start = pos;
            while pos < d.len() && !d[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if start == pos {
                return Err("truncated ppm header".into());
            }
            Ok(String::from_utf8_lossy(&d[start..pos]).into_owned())
        };
        if token()? != "P6" {
            return Err("not a P6 ppm".into());
        }
        let w: usize = token()?.parse().map_err(|_| "bad width")?;
        let h: usize = token()?.parse().map_err(|_| "bad height")?;
        let _max = token()?;
        pos += 1;
        let body = d.get(pos..pos + w * h * 3).ok_or("truncated ppm body")?;
        let px = body.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        Ok(Self { w, h, px })
    }

    /// Pixel at `(x, y)`; panics when outside the frame.
    pub fn at(&self, x: i32, y: i32) -> [u8; 3] {
        self.px[y as usize * self.w + x as usize]
    }

    /// True when `(x, y)` is inside the frame.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h
    }

    /// Write the frame as a binary PPM.
    pub fn write(&self, path: &str) -> Result<(), String> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px {
            out.extend_from_slice(p);
        }
        std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
    }
}
