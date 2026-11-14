//! Pixel geometry for the draft cell-border protocol.
// Adapted from Ghostty src/terminal/borders.zig at
// 62f25299908b4fc432dca76f330bf43da7185a02.
// Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use termwiz::cell::border::{BorderStroke, BorderStyle, CellBorders};
use termwiz::color::ColorAttribute;

#[cfg(test)]
#[path = "cell_border_geometry_tests.rs"]
mod tests;

/// One clipped-at-paint-time solid rectangle, relative to its owning cell.
#[derive(Debug, Clone, Copy)]
pub(super) struct BorderRect {
    /// Pixel bounds, with an exclusive right and bottom.
    pub bounds: [i32; 4],
    /// Stroke for border paint, absent for outside fills.
    pub stroke: Option<BorderStroke>,
    /// Outside colour, absent for border paint.
    pub outside: Option<ColorAttribute>,
    /// Supersampled coverage in 0..=255.
    pub alpha: u8,
}

/// Raw neighbour sets in left, right, above, below order.
pub(super) type Neighbors<'a> = [Option<&'a CellBorders>; 4];

/// Remove centered strokes that lose to their matching neighbor.
pub(super) fn resolve(raw: &CellBorders, neighbors: Neighbors<'_>) -> [Option<BorderStroke>; 12] {
    let mut drawn = raw.segments;
    for (neighbor, pairs, first) in [
        (neighbors[0], [(6, 2), (7, 3)], false),
        (neighbors[1], [(2, 6), (3, 7)], true),
        (neighbors[2], [(0, 4), (1, 5)], false),
        (neighbors[3], [(4, 0), (5, 1)], true),
    ] {
        let Some(neighbor) = neighbor else { continue };
        for (own, other) in pairs {
            if let (Some(a), Some(b)) = (drawn[own], neighbor.segments[other]) {
                if a.centered && b.centered {
                    let a_rank = (a.priority, a.width);
                    let b_rank = (b.priority, b.width);
                    if a_rank < b_rank || (first && a_rank == b_rank) {
                        drawn[own] = None;
                    }
                }
            }
        }
    }
    drawn
}

const ARMS: [[[i32; 2]; 2]; 12] = [
    [[0, 0], [1, 0]],
    [[1, 0], [2, 0]],
    [[2, 0], [2, 1]],
    [[2, 1], [2, 2]],
    [[0, 2], [1, 2]],
    [[1, 2], [2, 2]],
    [[0, 0], [0, 1]],
    [[0, 1], [0, 2]],
    [[0, 1], [1, 1]],
    [[1, 1], [2, 1]],
    [[1, 0], [1, 1]],
    [[1, 1], [1, 2]],
];
const JOINT_FLAGS: [[usize; 2]; 8] = [
    [0, 0],
    [2, 1],
    [8, 2],
    [6, 3],
    [4, 3],
    [4, 0],
    [4, 1],
    [4, 2],
];

fn thickness(stroke: BorderStroke, width: i32) -> i32 {
    let min = if stroke.style == BorderStyle::Double {
        3
    } else {
        1
    };
    min.max((width * i32::from(stroke.width) + 128) / 256)
}

fn lines(stroke: BorderStroke, t: i32) -> (usize, [[i32; 2]; 2]) {
    if stroke.style == BorderStyle::Double {
        let line = 1.max((t + 1) / 3);
        (2, [[0, line], [t - line, t]])
    } else {
        (1, [[0, t], [0, t]])
    }
}

fn horizontal(index: usize) -> bool {
    ARMS[index][0][1] == ARMS[index][1][1]
}

fn nominal(k: i32, size: i32) -> i32 {
    match k {
        0 => 0,
        1 => size / 2,
        _ => size,
    }
}

fn band_start(index: usize, stroke: BorderStroke, w: i32, h: i32) -> i32 {
    let t = thickness(stroke, w);
    let axis = usize::from(horizontal(index));
    let size = if axis == 1 { h } else { w };
    match ARMS[index][0][axis] {
        1 => size / 2 - t / 2,
        0 => {
            if stroke.centered {
                -t / 2
            } else {
                0
            }
        }
        _ => {
            if stroke.centered {
                size - t / 2
            } else {
                size - t
            }
        }
    }
}

/// A stroke incident on a lattice joint.
#[derive(Clone, Copy)]
struct Slot {
    /// Full stroke for junction width and style.
    stroke: BorderStroke,
    /// Start of the perpendicular band.
    lo: i32,
    /// Band thickness in device pixels.
    t: i32,
    /// Segment index when owned by this cell.
    own: Option<usize>,
}
type Joint = [Option<Slot>; 4];

fn direction(from: [i32; 2], to: [i32; 2]) -> usize {
    if to[0] > from[0] {
        0
    } else if to[1] > from[1] {
        1
    } else if to[0] < from[0] {
        2
    } else {
        3
    }
}

fn collect(raw: &CellBorders, neighbors: Neighbors<'_>, w: i32, h: i32) -> [Joint; 9] {
    let mut joints = [[None; 4]; 9];
    for (ci, (set, dx, dy)) in [
        (Some(raw), 0, 0),
        (neighbors[0], -1, 0),
        (neighbors[1], 1, 0),
        (neighbors[2], 0, -1),
        (neighbors[3], 0, 1),
    ]
    .iter()
    .copied()
    .enumerate()
    {
        let Some(set) = set else { continue };
        for (i, stroke) in set.segments.iter().enumerate() {
            let Some(stroke) = *stroke else { continue };
            let lo = band_start(i, stroke, w, h) + if horizontal(i) { dy * h } else { dx * w };
            for end in 0..2 {
                let p = ARMS[i][end];
                let gx = dx * 2 + p[0];
                let gy = dy * 2 + p[1];
                if !(0..=2).contains(&gx) || !(0..=2).contains(&gy) {
                    continue;
                }
                let slot = &mut joints[(gy * 3 + gx) as usize][direction(p, ARMS[i][1 - end])];
                if slot.is_none() {
                    *slot = Some(Slot {
                        stroke,
                        lo,
                        t: thickness(stroke, w),
                        own: (ci == 0).then_some(i),
                    });
                }
            }
        }
    }
    joints
}

fn arm_end(joint: Joint, d: usize, sides: &[usize], start: i32) -> i32 {
    let positive = d < 2;
    let opposite = joint[(d + 2) % 4];
    let mut best: Option<i32> = None;
    for &side in sides {
        let end = if let Some(p) = joint[side] {
            let (n, ls) = lines(p.stroke, p.t);
            if positive {
                p.lo + ls[n - 1][0]
            } else {
                p.lo + ls[0][1]
            }
        } else if opposite.is_some() {
            start
        } else if let Some(p) = joint[(side + 2) % 4] {
            if positive {
                p.lo
            } else {
                p.lo + p.t
            }
        } else {
            start
        };
        best = Some(match best {
            Some(b) if positive => b.min(end),
            Some(b) => b.max(end),
            None => end,
        });
    }
    best.unwrap_or(start)
}

/// An arm trimmed by a rounded quadrant, possibly spanning two half segments.
#[derive(Default, Clone, Copy)]
struct ArcArm {
    /// The owned segment and its identical collinear continuation.
    segments: [Option<usize>; 2],
    /// Tangency coordinate where straight paint ends.
    cut: i32,
    /// Whether the arm leaves the joint toward increasing coordinates.
    positive: bool,
}

/// One quarter ring and the straight arms it replaces.
struct ArcPlan {
    /// Horizontal incident stroke.
    hs: Slot,
    /// Vertical incident stroke.
    vs: Slot,
    /// Horizontal direction, 0 or 2.
    hd: usize,
    /// Vertical direction, 1 or 3.
    vd: usize,
    /// Radius after same-cell continuation and dimension limits.
    radius: i32,
    /// The horizontal arm owns arc colour, style and layer.
    stroke: BorderStroke,
    /// Horizontal and vertical trim plans.
    arms: [ArcArm; 2],
}

fn plan_arc(
    joints: &[Joint; 9],
    drawn: &[Option<BorderStroke>; 12],
    j: usize,
    q: usize,
    w: i32,
    h: i32,
    radius: Option<u16>,
) -> ArcPlan {
    let hd = if q & 1 == 0 { q } else { (q + 1) % 4 };
    let vd = if q & 1 == 0 { (q + 1) % 4 } else { q };
    // Rounded flags were validated against both owned surviving arms.
    let hs = joints[j][hd].unwrap();
    let vs = joints[j][vd].unwrap();
    let mut out = ArcPlan {
        hs,
        vs,
        hd,
        vd,
        radius: w
            .min(h)
            .min((w * i32::from(radius.unwrap_or(128)) + 128) / 256),
        stroke: drawn[hs.own.unwrap()].unwrap(),
        arms: [ArcArm {
            positive: true,
            ..ArcArm::default()
        }; 2],
    };
    let point = [(j % 3) as i32, (j / 3) as i32];
    for (k, (d, slot, perp)) in [(hd, hs, vs), (vd, vs, hs)].iter().copied().enumerate() {
        let seg = slot.own.unwrap();
        let size = if k == 0 { w } else { h };
        let mut far = ARMS[seg][usize::from(ARMS[seg][0] == point)];
        let mut continuation = None;
        if far[k] == 1 {
            if let Some(c) = joints[(far[1] * 3 + far[0]) as usize][d] {
                if let Some(ci) = c.own {
                    if drawn[ci] == drawn[seg] {
                        continuation = Some(ci);
                        far[k] += if d < 2 { 1 } else { -1 };
                    }
                }
            }
        }
        let center2 = 2 * perp.lo + perp.t;
        out.radius = out
            .radius
            .min((2 * nominal(far[k], size) - center2).abs() / 2);
        if joints[j][(d + 2) % 4].is_none() {
            out.arms[k] = ArcArm {
                segments: [Some(seg), continuation],
                positive: d < 2,
                cut: 0,
            };
        }
    }
    out.radius = out.radius.max(1);
    for (arm, perp) in out.arms.iter_mut().zip([vs, hs]) {
        let twice = 2 * perp.lo
            + perp.t
            + if arm.positive {
                2 * out.radius
            } else {
                -2 * out.radius
            };
        arm.cut = if arm.positive {
            twice.div_euclid(2)
        } else {
            -(-twice).div_euclid(2)
        };
    }
    out
}

/// Produce backend-independent solid rectangles, retaining reference paint order.
pub(super) fn geometry(
    raw: &CellBorders,
    neighbors: Neighbors<'_>,
    w: i32,
    h: i32,
) -> Vec<BorderRect> {
    let drawn = resolve(raw, neighbors);
    let joints = collect(raw, neighbors, w, h);
    let mut rounded = [0u8; 9];
    for (bit, [j, q]) in JOINT_FLAGS.iter().copied().enumerate() {
        if raw.joints & (1 << bit) == 0 {
            continue;
        }
        if let (Some(a), Some(b)) = (joints[j][q], joints[j][(q + 1) % 4]) {
            if let (Some(ai), Some(bi)) = (a.own, b.own) {
                if drawn[ai].is_some() && drawn[bi].is_some() {
                    rounded[j] |= 1 << q;
                }
            }
        }
    }
    let mut arcs = Vec::new();
    for (j, mask) in rounded.iter().copied().enumerate() {
        for q in 0..4 {
            if mask & (1 << q) != 0 {
                arcs.push(plan_arc(&joints, &drawn, j, q, w, h, raw.radius));
            }
        }
    }
    let mut out = Vec::new();
    for (i, stroke) in drawn.iter().enumerate() {
        let Some(stroke) = *stroke else { continue };
        let t = thickness(stroke, w);
        let lo = band_start(i, stroke, w, h);
        let [pa, pb] = ARMS[i];
        let d = direction(pa, pb);
        let horiz = horizontal(i);
        let axis = usize::from(!horiz);
        let size = if horiz { w } else { h };
        let na = nominal(pa[axis], size);
        let nb = nominal(pb[axis], size);
        let ja = (pa[1] * 3 + pa[0]) as usize;
        let jb = (pb[1] * 3 + pb[0]) as usize;
        let (n, ls) = lines(stroke, t);
        let both = if horiz { [3, 1] } else { [2, 0] };
        for k in 0..n {
            let sides = if n == 1 { &both[..] } else { &both[k..k + 1] };
            let mut span = match stroke.style {
                BorderStyle::Solid | BorderStyle::Double => [
                    arm_end(joints[ja], d, sides, na),
                    arm_end(joints[jb], (d + 2) % 4, sides, nb),
                ],
                BorderStyle::Dashed => [na + (nb - na) / 4, nb - (nb - na) / 4],
                BorderStyle::Dotted => {
                    let start = (na + nb) / 2 - t / 2;
                    [start, start + t]
                }
            };
            for arc in &arcs {
                for arm in arc.arms {
                    if arm.segments.contains(&Some(i)) {
                        if arm.positive {
                            span[0] = span[0].max(arm.cut);
                        } else {
                            span[1] = span[1].min(arm.cut);
                        }
                    }
                }
            }
            if span[1] <= span[0] {
                continue;
            }
            let across = [lo + ls[k][0], lo + ls[k][1]];
            out.push(BorderRect {
                bounds: if horiz {
                    [span[0], across[0], span[1], across[1]]
                } else {
                    [across[0], span[0], across[1], span[1]]
                },
                stroke: Some(stroke),
                outside: None,
                alpha: 255,
            });
        }
    }
    if let Some(outside) = raw.outside {
        for arc in &arcs {
            raster_arc(&mut out, arc, Some(outside), w, h);
        }
    }
    for arc in &arcs {
        raster_arc(&mut out, arc, None, w, h);
    }
    out
}

fn raster_arc(
    out: &mut Vec<BorderRect>,
    arc: &ArcPlan,
    outside: Option<ColorAttribute>,
    w: i32,
    h: i32,
) {
    let sx = if arc.hd == 0 { 1. } else { -1. };
    let sy = if arc.vd == 1 { 1. } else { -1. };
    let r = arc.radius as f32;
    let ox = (2 * arc.vs.lo + arc.vs.t) as f32 / 2. + sx * r;
    let oy = (2 * arc.hs.lo + arc.hs.t) as f32 / 2. + sy * r;
    let pi = std::f32::consts::PI;
    let window = match arc.stroke.style {
        BorderStyle::Solid | BorderStyle::Double => [0., pi / 2.],
        BorderStyle::Dashed => [pi / 8., 3. * pi / 8.],
        BorderStyle::Dotted => {
            let half = arc.hs.t as f32 / 2. / r.max(1.);
            [pi / 4. - half, pi / 4. + half]
        }
    };
    let (n, bands) = if outside.is_some() {
        (1, [[0, arc.hs.t], [0, arc.hs.t]])
    } else {
        lines(arc.stroke, arc.hs.t)
    };
    for band in &bands[..n] {
        let distances = [
            (arc.hs.lo + band[0]) as f32 - oy,
            (arc.hs.lo + band[1]) as f32 - oy,
        ];
        let rin = distances[0].abs().min(distances[1].abs());
        let rout = distances[0].abs().max(distances[1].abs());
        let mut left = ox.min(ox - sx * rout).floor() as i32;
        let mut right = ox.max(ox - sx * rout).ceil() as i32;
        let mut top = oy.min(oy - sy * rout).floor() as i32;
        let mut bottom = oy.max(oy - sy * rout).ceil() as i32;
        if outside.is_some() {
            left = left.max(0);
            right = right.min(w);
            top = top.max(0);
            bottom = bottom.min(h);
        }
        let rect = |x0, y, x1, alpha| BorderRect {
            bounds: [x0, y, x1, y + 1],
            alpha,
            outside,
            stroke: outside.is_none().then_some(arc.stroke),
        };
        for y in top..bottom {
            let mut run = None;
            for x in left..=right {
                let mut hits = 0u32;
                if x < right {
                    for iy in 0..4 {
                        for ix in 0..4 {
                            let u = (ox - (x as f32 + (ix as f32 + 0.5) / 4.)) * sx;
                            let v = (oy - (y as f32 + (iy as f32 + 0.5) / 4.)) * sy;
                            if u < 0. || v < 0. {
                                continue;
                            }
                            let d2 = u * u + v * v;
                            if outside.is_some() {
                                if u > rout || v > rout || d2 <= rout * rout {
                                    continue;
                                }
                            } else {
                                if d2 < rin * rin || d2 > rout * rout {
                                    continue;
                                }
                                let angle = u.atan2(v);
                                if angle < window[0] || angle > window[1] {
                                    continue;
                                }
                            }
                            hits += 1;
                        }
                    }
                }
                let alpha = ((hits * 255 + 8) / 16) as u8;
                if alpha == 255 {
                    if run.is_none() {
                        run = Some(x);
                    }
                    continue;
                }
                if let Some(start) = run.take() {
                    out.push(rect(start, y, x, 255));
                }
                if alpha != 0 {
                    out.push(rect(x, y, x + 1, alpha));
                }
            }
        }
    }
}
