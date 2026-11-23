//! Data-only local poses, rig hierarchy and conservative bounds. No playback executor.
#![allow(missing_docs)]
use super::{Clip, Interpolation, Model, TrackPath};
use crate::{Component, Mat4, Quat, Transform, Vec3};
/// Compact saved arrays, ten floats per imported node: translation, xyzw rotation, scale.
/// Imported nodes are never entities. The composed palette is not saved.
/// animation::step writes these rows; the renderer interpolates completed ticks.
#[derive(Default, Clone, Debug, Component)]
pub struct Pose {
    /// Previous completed tick's local node poses; ten floats per node.
    pub previous: Vec<f32>,
    /// Current local node poses; translation metres, xyzw quaternion, scale.
    pub local: Vec<f32>,
    /// Normalized clip phase; one-shot endpoints are zero and one.
    pub phase: f32,
    /// Model-local displacement extracted this tick, in metres.
    pub root_motion: Vec3,
    /// Marker names crossed in the last animation step.
    pub crossed: Vec<String>,
    /// Conservative model-local AABB [min x, y, z, max x, y, z], in metres.
    pub bounds: [f32; 6],
    pub(crate) stepped: Option<u64>,
}

impl Pose {
    /// The tick `animation::step` last wrote this pose on; none before it has.
    pub fn stepped(&self) -> Option<u64> {
        self.stepped
    }
    /// The two poses a frame of tick `tick` is drawn between: the previous
    /// completed tick's and this one's, where `animation::step` ran on it.
    /// A tick it did not run on (a game held still, a pause) holds the
    /// current pose: blending the last two again would replay the last
    /// step's motion every tick, a jiggle.
    pub fn shown(&self, tick: u64) -> (&[f32], &[f32]) {
        match self.stepped {
            Some(at) if at != tick => (&self.local, &self.local),
            _ => (&self.previous, &self.local),
        }
    }
}

#[cfg(test)]
mod shown_tests {
    use super::*;

    #[test]
    fn a_pose_is_blended_on_the_tick_it_stepped_and_held_after() {
        let p = Pose { previous: vec![0.0; 10], local: vec![1.0; 10], stepped: Some(7), ..Default::default() };
        assert_eq!(p.shown(7), (&[0.0; 10][..], &[1.0; 10][..]), "the tick it stepped: from the last pose to this one");
        assert_eq!(p.shown(8), (&[1.0; 10][..], &[1.0; 10][..]), "a tick held: this pose, still");
        let unstepped = Pose { previous: vec![0.0; 10], local: vec![1.0; 10], ..Default::default() };
        assert_eq!(unstepped.shown(3).0, &[0.0; 10][..], "a pose no step wrote is drawn as it was given");
    }
}

impl Clip {
    pub fn duration(&self) -> f32 {
        self.tracks
            .iter()
            .filter_map(|t| t.times.last())
            .copied()
            .fold(0., f32::max)
    }
}

pub(crate) fn at(p: &[f32]) -> Transform {
    Transform {
        position: Vec3::from_slice(p),
        rotation: Quat::from_xyzw(p[3], p[4], p[5], p[6]),
        scale: Vec3::from_slice(&p[7..]),
    }
}
pub(crate) fn put(p: &mut [f32], t: Transform) {
    p[..3].copy_from_slice(&t.position.to_array());
    p[3..7].copy_from_slice(&t.rotation.to_array());
    p[7..10].copy_from_slice(&t.scale.to_array());
}
pub(crate) fn matrix(p: &[f32]) -> Mat4 {
    let t = at(p);
    Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.position)
}
pub fn bind_pose(model: &Model) -> Vec<f32> {
    let mut out = vec![0.; model.nodes.len() * 10];
    for (n, p) in model.nodes.iter().zip(out.as_chunks_mut::<10>().0) {
        let (scale, rotation, position) =
            Mat4::from_cols_array(&n.transform).to_scale_rotation_translation();
        put(
            p,
            Transform {
                position,
                rotation: rotation.normalize(),
                scale,
            },
        );
    }
    out
}
/// Parent-first traversal used by the GPU loader; joint indices keep glTF's order.
pub fn node_order(model: &Model) -> Vec<u32> {
    let mut order = Vec::with_capacity(model.nodes.len());
    while order.len() < model.nodes.len() {
        let before = order.len();
        for (i, n) in model.nodes.iter().enumerate() {
            if !order.contains(&(i as u32)) && n.parent.is_none_or(|p| order.contains(&p)) {
                order.push(i as u32);
            }
        }
        assert!(order.len() > before, "validated parent graph");
    }
    order
}

pub fn joint_matrix(model: &Model, local: &[f32], node: u32) -> Mat4 {
    let mut out = matrix(&local[node as usize * 10..]);
    let mut parent = model.nodes[node as usize].parent;
    while let Some(p) = parent {
        out = matrix(&local[p as usize * 10..]) * out;
        parent = model.nodes[p as usize].parent;
    }
    out
}

// Conservative reach: maximum sum of local translation lengths along a chain,
// scaled by ancestor scale, plus each influenced vertex's inverse-bind radius.
pub fn animated_bounds(model: &Model) -> [f32; 6] {
    if model.skins.is_empty() && model.clips.is_empty() {
        return model.bounds;
    }
    let rest = bind_pose(model);
    let mut lengths = vec![0.; model.nodes.len()];
    let mut scales = vec![1.; model.nodes.len()];
    for (i, p) in rest.as_chunks::<10>().0.iter().enumerate() {
        lengths[i] = Vec3::from_slice(p).length();
        scales[i] = Vec3::from_slice(&p[7..]).abs().max_element();
    }
    for clip in &model.clips {
        for t in &clip.tracks {
            if matches!(t.path, TrackPath::Translation | TrackPath::Scale) {
                // Cubic tangents can overshoot; include one duration times both tangents.
                let bound = t
                    .values
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|p| Vec3::from_slice(p).length())
                    .fold(0., f32::max)
                    * if matches!(t.interpolation, Interpolation::CubicSpline) {
                        1. + 2. * clip.duration()
                    } else {
                        1.
                    };
                let slot = if matches!(t.path, TrackPath::Translation) {
                    &mut lengths[t.node as usize]
                } else {
                    &mut scales[t.node as usize]
                };
                *slot = slot.max(bound);
            }
        }
    }
    let mut reach = vec![0.; model.nodes.len()];
    let mut global_scale = vec![1.; model.nodes.len()];
    for i in node_order(model) {
        let i = i as usize;
        let (r, s) = model.nodes[i]
            .parent
            .map_or((0., 1.), |p| (reach[p as usize], global_scale[p as usize]));
        reach[i] = r + lengths[i] * s;
        global_scale[i] = s * scales[i];
    }
    let mut radius = reach.iter().copied().fold(0., f32::max);
    for (index, node) in model.nodes.iter().enumerate() {
        if let (Some(mesh), Some(skin)) = (node.mesh, node.skin) {
            let mesh = &model.meshes[mesh as usize];
            let skin = &model.skins[skin as usize];
            for (v, position) in mesh.positions.as_chunks::<3>().0.iter().enumerate() {
                for influence in 0..4 {
                    let j = mesh.joints[v * 4 + influence] as usize;
                    let node = skin.joints[j] as usize;
                    let inverse = Mat4::from_cols_slice(&skin.inverse_binds[j * 16..j * 16 + 16]);
                    radius = radius.max(
                        reach[node]
                            + global_scale[node]
                                * inverse
                                    .transform_point3(Vec3::from_slice(position))
                                    .length(),
                    );
                }
            }
        } else if let Some(mesh) = node.mesh {
            for position in model.meshes[mesh as usize].positions.as_chunks::<3>().0 {
                radius = radius
                    .max(reach[index] + global_scale[index] * Vec3::from_slice(position).length());
            }
        }
    }
    let b = model.bounds;
    [
        b[0] - radius,
        b[1] - radius,
        b[2] - radius,
        b[3] + radius,
        b[4] + radius,
        b[5] + radius,
    ]
}
