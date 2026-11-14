// variation: a face's design axes and named instances, read through skrifa,
// which also applies `avar`, `gvar`, `HVAR`, `MVAR`, and `CFF2` blending when
// a face is drawn at a location (see `Face::instance`).

use crate::text::font::Face;

/// One variation axis, in user units.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Axis {
    pub tag: String,
    pub min: f64,
    pub default: f64,
    pub max: f64,
}

/// A named instance: the `name` table id of its subfamily and its user
/// coordinates, one per axis.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Instance {
    pub name_id: i64,
    pub coords: Vec<f64>,
}

/// The face's variation axes, in `fvar` order; empty for a static face.
pub fn axes(face: &Face) -> Vec<Axis> {
    let Some(model) = face.model() else {
        return vec![];
    };
    model
        .axes()
        .iter()
        .map(|a| Axis {
            tag: a.tag().to_string(),
            min: f64::from(a.min_value()),
            default: f64::from(a.default_value()),
            max: f64::from(a.max_value()),
        })
        .collect()
}

/// The face's named instances, in `fvar` order.
pub fn instances(face: &Face) -> Vec<Instance> {
    let Some(model) = face.model() else {
        return vec![];
    };
    model
        .named_instances()
        .iter()
        .map(|i| Instance {
            name_id: i64::from(i.subfamily_name().to_u16()),
            coords: i.user_coords().map(f64::from).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{axes, instances};
    use crate::text::font::{Face, variable_test_face};

    #[test]
    fn a_static_face_has_no_axes_or_instances() {
        let face = Face::parse(include_bytes!("../../assets/fonts/DejaVuSans.ttf"), 0).unwrap();
        assert!(axes(&face).is_empty());
        assert!(instances(&face).is_empty());
    }

    #[test]
    fn a_variable_face_lists_its_axis_in_user_units() {
        let face = Face::parse(&variable_test_face(false), 0).unwrap();
        assert_eq!(
            axes(&face)
                .iter()
                .map(|a| (a.tag.as_str(), a.min, a.default, a.max))
                .collect::<Vec<_>>(),
            vec![("wght", 100.0, 400.0, 900.0)]
        );
    }
}
