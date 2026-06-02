//! Transform-gizmo primitives: axis / plane handles, their colors and sizes.
//!
//! Pure value types extracted from the editor UI module. No `App` state or
//! panel logic lives here — only geometry/color helpers that depend on the
//! `egui` space and color types.

use egui::{Color32, Pos2};

use crate::lerp_u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PrimitiveGizmoAxis {
    X,
    Y,
    Z,
}

impl PrimitiveGizmoAxis {
    pub(crate) fn color(self) -> Color32 {
        match self {
            Self::X => Color32::from_rgb(255, 84, 76),
            Self::Y => Color32::from_rgb(98, 236, 112),
            Self::Z => Color32::from_rgb(86, 156, 255),
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }

    pub(crate) fn world_delta(self, sector_size: i32) -> [f32; 3] {
        let sector_size = sector_size as f32;
        match self {
            Self::X => [sector_size, 0.0, 0.0],
            Self::Y => [0.0, sector_size, 0.0],
            Self::Z => [0.0, 0.0, sector_size],
        }
    }

    pub(crate) const fn cell_delta(self, steps: i32) -> [i32; 2] {
        match self {
            Self::X => [steps, 0],
            Self::Y => [0, 0],
            Self::Z => [0, steps],
        }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrimitiveGizmoScreenAxis {
    pub(crate) axis: PrimitiveGizmoAxis,
    pub(crate) start: Pos2,
    pub(crate) end: Pos2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NodeGizmoPlane {
    XY,
    XZ,
    YZ,
}

impl NodeGizmoPlane {
    pub(crate) const ALL: [Self; 3] = [Self::XY, Self::XZ, Self::YZ];

    pub(crate) const fn axes(self) -> [PrimitiveGizmoAxis; 2] {
        match self {
            Self::XY => [PrimitiveGizmoAxis::X, PrimitiveGizmoAxis::Y],
            Self::XZ => [PrimitiveGizmoAxis::X, PrimitiveGizmoAxis::Z],
            Self::YZ => [PrimitiveGizmoAxis::Y, PrimitiveGizmoAxis::Z],
        }
    }

    pub(crate) const fn normal_axis(self) -> PrimitiveGizmoAxis {
        match self {
            Self::XY => PrimitiveGizmoAxis::Z,
            Self::XZ => PrimitiveGizmoAxis::Y,
            Self::YZ => PrimitiveGizmoAxis::X,
        }
    }

    pub(crate) fn color(self) -> Color32 {
        self.normal_axis().color()
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::XY => "XY",
            Self::XZ => "XZ",
            Self::YZ => "YZ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NodeGizmoHandle {
    Axis(PrimitiveGizmoAxis),
    Plane(NodeGizmoPlane),
}

impl NodeGizmoHandle {
    pub(crate) const fn axis(self) -> Option<PrimitiveGizmoAxis> {
        match self {
            Self::Axis(axis) => Some(axis),
            Self::Plane(_) => None,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Axis(axis) => axis.label(),
            Self::Plane(plane) => plane.label(),
        }
    }
}

pub(crate) fn gizmo_axis_color(axis: PrimitiveGizmoAxis, highlighted: bool) -> Color32 {
    gizmo_highlight_color(axis.color(), highlighted)
}

pub(crate) fn gizmo_highlight_color(color: Color32, highlighted: bool) -> Color32 {
    if highlighted {
        Color32::from_rgb(
            lerp_u8(color.r(), 255, 96),
            lerp_u8(color.g(), 255, 96),
            lerp_u8(color.b(), 255, 96),
        )
    } else {
        color
    }
}

pub(crate) fn gizmo_axis_stroke_width(highlighted: bool) -> f32 {
    if highlighted {
        4.25
    } else {
        2.5
    }
}

pub(crate) fn gizmo_axis_handle_radius(highlighted: bool) -> f32 {
    if highlighted {
        6.5
    } else {
        5.0
    }
}
