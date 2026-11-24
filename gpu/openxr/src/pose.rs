//! Each eye's matrices, and the debug eye capture (`docs/quest.md`, "Seeing
//! what the headset shows"): the pieces that do not need a headset. The
//! session polls a trigger file, parses it here ([`Request::parse`]), may
//! replace the head's pose with a fixed one looking at the stage
//! ([`StagePose::eyes`]), and writes each eye's layer as a PNG ([`png_rgb`]).
//!
//! No capture runs unless the trigger file appears.

/// Eye → clip (column-major, 0–1 depth, right-handed, looking down −Z)
/// from a field of view's angles in radians, `[left, right, up, down]`
/// (left and down negative, as OpenXR's `XrFovf`), and the near and far
/// planes.
pub fn fov_projection(angles: [f32; 4], near: f32, far: f32) -> [f32; 16] {
    let [l, r, u, d] = angles.map(f32::tan);
    let (w, h) = (r - l, u - d);
    [
        2.0 / w,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / h,
        0.0,
        0.0,
        (r + l) / w,
        (u + d) / h,
        far / (near - far),
        -1.0,
        0.0,
        0.0,
        near * far / (near - far),
        0.0,
    ]
}

/// Reference space → eye (column-major): the inverse of an eye's pose, its
/// orientation a unit quaternion `[x, y, z, w]` and its position in metres.
pub fn pose_view(orientation: [f32; 4], position: [f32; 3]) -> [f32; 16] {
    let [x, y, z, w] = orientation;
    // Rows of the pose's rotation.
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    let p = position;
    // The inverse is Rᵀ with −Rᵀp; column j of Rᵀ is row j of R.
    let t = [0, 1, 2].map(|i| -(r[0][i] * p[0] + r[1][i] * p[1] + r[2][i] * p[2]));
    [
        r[0][0], r[0][1], r[0][2], 0.0, //
        r[1][0], r[1][1], r[1][2], 0.0, //
        r[2][0], r[2][1], r[2][2], 0.0, //
        t[0], t[1], t[2], 1.0,
    ]
}

/// What one capture asks for: the head as tracked, or a fixed pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Request {
    /// A fixed head pose looking at the stage instead of the tracked head.
    pub pose: Option<StagePose>,
}

/// A head looking at a point (the stage's centre): from `distance` metres
/// away, `pitch_deg` degrees down, turned `yaw_deg` degrees about +Y (zero
/// stands on the +Z side, the person's side of a tabletop).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StagePose {
    /// Metres from the head to the focus.
    pub distance: f32,
    /// Degrees the head looks down.
    pub pitch_deg: f32,
    /// Degrees the head stands turned about +Y.
    pub yaw_deg: f32,
    /// The point it looks at, where that is not the stage's centre (`look`).
    pub focus: Option<[f32; 3]>,
}

impl Default for StagePose {
    fn default() -> Self {
        Self {
            distance: 0.6,
            pitch_deg: 40.0,
            yaw_deg: 0.0,
            focus: None,
        }
    }
}

/// The distance between the eyes of a fixed pose, metres.
pub const IPD: f32 = 0.064;

impl Request {
    /// The trigger file's text: empty or `head` for the tracked head;
    /// `stage [distance_m [pitch_deg [yaw_deg]]]` for the fixed pose
    /// (0.6 m, 40° down, 0° by default); `look <ex ey ez> <tx ty tz>` for a
    /// head at a point of the reference space looking at another (a close
    /// look at something that is not the stage's centre: a panel, a hand).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut words = text.split_whitespace();
        match words.next() {
            None | Some("head") => Ok(Self { pose: None }),
            Some("stage") => {
                let mut pose = StagePose::default();
                let numbers: Vec<f32> = words
                    .map(|w| {
                        w.parse::<f32>()
                            .map_err(|_| format!("`{w}` is not a number"))
                    })
                    .collect::<Result<_, _>>()?;
                if numbers.len() > 3 {
                    return Err("stage takes at most distance, pitch and yaw".into());
                }
                if let Some(&d) = numbers.first() {
                    if !(d.is_finite() && d > 0.0) {
                        return Err("the distance is positive metres".into());
                    }
                    pose.distance = d;
                }
                if let Some(&p) = numbers.get(1) {
                    pose.pitch_deg = p;
                }
                if let Some(&y) = numbers.get(2) {
                    pose.yaw_deg = y;
                }
                Ok(Self { pose: Some(pose) })
            }
            Some("look") => {
                let n: Vec<f32> = words
                    .map(|w| match w.parse::<f32>() {
                        Ok(v) if v.is_finite() => Ok(v),
                        _ => Err(format!("`{w}` is not a number")),
                    })
                    .collect::<Result<_, _>>()?;
                if n.len() != 6 {
                    return Err("look takes an eye and a target, three numbers each".into());
                }
                let f = [n[3] - n[0], n[4] - n[1], n[5] - n[2]];
                let distance = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();
                let flat = (f[0] * f[0] + f[2] * f[2]).sqrt();
                if distance < 1e-4 {
                    return Err("look: the eye is at its target".into());
                }
                // Straight down or up, the head stands unturned.
                let yaw = if flat < 1e-5 {
                    0.0
                } else {
                    (-f[0]).atan2(-f[2])
                };
                Ok(Self {
                    pose: Some(StagePose {
                        distance,
                        pitch_deg: (-f[1]).atan2(flat).to_degrees(),
                        yaw_deg: yaw.to_degrees(),
                        focus: Some([n[3], n[4], n[5]]),
                    }),
                })
            }
            Some(other) => Err(format!(
                "`{other}`: the trigger says `head`, `stage …` or `look …`"
            )),
        }
    }
}

type Quat = [f32; 4];
type Vec3 = [f32; 3];

fn mul(a: Quat, b: Quat) -> Quat {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

/// `v` turned by the unit quaternion `q`.
pub fn rotate(q: Quat, v: Vec3) -> Vec3 {
    let u = [q[0], q[1], q[2]];
    let w = q[3];
    let cross = |a: Vec3, b: Vec3| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v).map(|c| 2.0 * c);
    let ut = cross(u, t);
    [0, 1, 2].map(|i| v[i] + w * t[i] + ut[i])
}

impl StagePose {
    /// The head: its orientation (`[x, y, z, w]`, OpenXR's: looking down −Z
    /// with +Y up at identity) and position, looking at `focus`.
    pub fn head(&self, focus: Vec3) -> (Quat, Vec3) {
        let focus = self.focus.unwrap_or(focus);
        let (pitch, yaw) = (-self.pitch_deg.to_radians(), self.yaw_deg.to_radians());
        let qx = [(pitch / 2.0).sin(), 0.0, 0.0, (pitch / 2.0).cos()];
        let qy = [0.0, (yaw / 2.0).sin(), 0.0, (yaw / 2.0).cos()];
        let q = mul(qy, qx);
        let forward = rotate(q, [0.0, 0.0, -1.0]);
        let position = [0, 1, 2].map(|i| focus[i] - self.distance * forward[i]);
        (q, position)
    }

    /// Both eyes' poses (left, right): the head's orientation, each eye half
    /// of [`IPD`] to its side.
    pub fn eyes(&self, focus: Vec3) -> [(Quat, Vec3); 2] {
        let (q, head) = self.head(focus);
        [-0.5f32, 0.5].map(|side| {
            let offset = rotate(q, [side * IPD, 0.0, 0.0]);
            (q, [0, 1, 2].map(|i| head[i] + offset[i]))
        })
    }
}

/// Packed RGB from packed RGBA (the alpha dropped).
pub fn rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (n, entry) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *entry = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for chunk in chunks {
        for &b in *chunk {
            crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// An 8-bit RGB PNG of `width × height` packed pixels, stored uncompressed
/// (a debug capture: no compressor to carry, about 9 MB an eye).
pub fn png_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let stride = width as usize * 3;
    assert_eq!(rgb.len(), stride * height as usize, "packed RGB rows");
    // The filtered image: a zero filter byte before each row.
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgb.chunks_exact(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib: a header, stored deflate blocks, Adler-32.
    let mut z = vec![0x78, 0x01];
    let blocks = raw.chunks(65_535).collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    if blocks.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trigger_parses() {
        assert_eq!(Request::parse("").unwrap().pose, None);
        assert_eq!(Request::parse("head\n").unwrap().pose, None);
        assert_eq!(
            Request::parse("stage").unwrap().pose,
            Some(StagePose::default())
        );
        let pose = Request::parse("stage 0.8 30 -20").unwrap().pose.unwrap();
        assert_eq!(
            (pose.distance, pose.pitch_deg, pose.yaw_deg),
            (0.8, 30.0, -20.0)
        );
        assert!(Request::parse("stage -1").is_err());
        assert!(Request::parse("stage x").is_err());
        assert!(Request::parse("left").is_err());
    }

    #[test]
    fn a_look_stands_the_head_at_its_eye_facing_its_target() {
        let (eye, target) = ([0.3f32, 1.4, 0.2], [-0.2f32, 1.0, -0.6]);
        let pose = Request::parse("look 0.3 1.4 0.2  -0.2 1.0 -0.6")
            .unwrap()
            .pose
            .unwrap();
        // Wherever the stage is.
        let (q, head) = pose.head([9.0, 9.0, 9.0]);
        for i in 0..3 {
            assert!((head[i] - eye[i]).abs() < 1e-5, "{head:?}");
        }
        let view = pose_view(q, head);
        let ahead = [0, 1, 2]
            .map(|r| (0..3).map(|c| view[c * 4 + r] * target[c]).sum::<f32>() + view[12 + r]);
        assert!(
            ahead[0].abs() < 1e-5 && ahead[1].abs() < 1e-5 && ahead[2] < 0.0,
            "{ahead:?}"
        );
        // Straight down is a pose too; no eye at its target, nor five numbers.
        let down = Request::parse("look 0 2 0  0 1 0").unwrap().pose.unwrap();
        assert!((down.pitch_deg - 90.0).abs() < 1e-3 && down.yaw_deg == 0.0);
        assert!(Request::parse("look 0 1 0 0 1 0").is_err());
        assert!(Request::parse("look 0 1 0 0 1").is_err());
    }

    #[test]
    fn the_stage_pose_looks_at_the_focus_from_above() {
        let focus = [0.0, 0.85, -0.9];
        let pose = StagePose::default();
        let (q, head) = pose.head(focus);
        // 0.6 m back toward the person and up, 40° down.
        let rad = 40f32.to_radians();
        assert!(
            (head[1] - (0.85 + 0.6 * rad.sin())).abs() < 1e-5,
            "{head:?}"
        );
        assert!(
            (head[2] - (-0.9 + 0.6 * rad.cos())).abs() < 1e-5,
            "{head:?}"
        );
        assert!(head[0].abs() < 1e-6);
        // The view (`pose_view`) puts the focus straight ahead.
        let view = pose_view(q, head);
        let ahead = [0, 1, 2]
            .map(|r| (0..3).map(|c| view[c * 4 + r] * focus[c]).sum::<f32>() + view[12 + r]);
        assert!(ahead[0].abs() < 1e-5 && ahead[1].abs() < 1e-5, "{ahead:?}");
        assert!((ahead[2] + 0.6).abs() < 1e-5, "{ahead:?}");
        let [left, right] = pose.eyes(focus);
        assert!((right.1[0] - left.1[0] - IPD).abs() < 1e-6);
    }

    #[test]
    fn a_png_has_its_chunks_and_checksums() {
        let rgb: Vec<u8> = (0..2 * 3 * 3).map(|i| i as u8).collect();
        let png = png_rgb(3, 2, &rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // IEND's CRC is fixed.
        assert_eq!(&png[png.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
        assert_eq!(
            super::rgb(&[1, 2, 3, 255, 4, 5, 6, 0]),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    /// `m` (column-major) applied to a point, divided through.
    fn project(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
        let v = [p[0], p[1], p[2], 1.0];
        let out: Vec<f32> = (0..4)
            .map(|row| (0..4).map(|col| m[col * 4 + row] * v[col]).sum())
            .collect();
        [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
    }

    #[test]
    fn a_fov_projection_maps_the_frustum_edges_and_depth() {
        let (l, r, u, d) = (-0.9f32, 0.7f32, 0.8f32, -0.85f32);
        let m = fov_projection([l, r, u, d], 0.1, 50.0);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(project(&m, [r.tan() * 2.0, 0.0, -2.0])[0], 1.0));
        assert!(close(project(&m, [l.tan() * 2.0, 0.0, -2.0])[0], -1.0));
        assert!(close(project(&m, [0.0, u.tan() * 3.0, -3.0])[1], 1.0));
        assert!(close(project(&m, [0.0, d.tan() * 3.0, -3.0])[1], -1.0));
        assert!(close(project(&m, [0.0, 0.0, -0.1])[2], 0.0));
        assert!(close(project(&m, [0.0, 0.0, -50.0])[2], 1.0));
    }

    #[test]
    fn a_pose_view_takes_the_eye_to_the_origin_looking_down_minus_z() {
        // Turned 90° left about +Y (looking down −X), standing at (1, 1.6, 2).
        let half = std::f32::consts::FRAC_PI_4;
        let m = pose_view([0.0, half.sin(), 0.0, half.cos()], [1.0, 1.6, 2.0]);
        let eye = project(&m, [1.0, 1.6, 2.0]);
        assert!(eye.iter().all(|c| c.abs() < 1e-5), "{eye:?}");
        // A point a metre down −X from the eye is a metre ahead (−Z).
        let ahead = project(&m, [0.0, 1.6, 2.0]);
        assert!(
            (ahead[2] + 1.0).abs() < 1e-5 && ahead[0].abs() < 1e-5,
            "{ahead:?}"
        );
    }
}
