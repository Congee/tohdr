//! Exif `Orientation` as a HEIF container transform.
//!
//! Nothing in this pipeline rotates pixels: a decoded image is handed to an
//! encoder and comes back the same way up. So a rotated source can only stay
//! correct if the *container* says how to display it, and that has to agree with
//! the Exif tag the same file carries — an Exif reader and a HEIF reader consult
//! different fields, and a file where those disagree is a file where two
//! conformant viewers show the photo different ways up.
//!
//! # The derivation
//!
//! Exif defines each orientation by where the stored data's 0th row and 0th
//! column land in the displayed image (Exif 2.32, tag `0x0112`). Writing that as
//! a coordinate map on a stored `W x H` image, with `x` rightward and `y`
//! downward:
//!
//! | value | Exif's words | maps `(x, y)` to |
//! |---|---|---|
//! | 1 | top/left | `(x, y)` |
//! | 2 | top/right | `(W-1-x, y)` |
//! | 3 | bottom/right | `(W-1-x, H-1-y)` |
//! | 4 | bottom/left | `(x, H-1-y)` |
//! | 5 | left/top | `(y, x)` |
//! | 6 | right/top | `(H-1-y, x)` |
//! | 7 | right/bottom | `(H-1-y, W-1-x)` |
//! | 8 | left/bottom | `(y, W-1-x)` |
//!
//! HEIF offers `irot` (anti-clockwise, in quarter turns) and `imir` (a mirror),
//! applied rotation-then-mirror. Composing those two and solving for each row
//! above gives [`heif_transform`]; the interesting cases are 5 and 7, which are
//! reflections about a diagonal and so need both boxes.
//! `every_orientation_composes_to_the_exif_mapping` checks all eight by applying
//! both definitions to actual coordinates rather than trusting the arithmetic
//! above.
//!
//! # What the algebra could not settle
//!
//! Two things: the order the boxes compose in, and which way round `imir`'s
//! `axis` field reads. Both were measured rather than reasoned, by
//! `tohdr-cli/examples/probe_orientation.rs`, which writes all eight
//! orientations through both engines and has ImageIO resolve each file's boxes
//! back to a number.
//!
//! The first reading of the spec's "a vertical (axis = 0) or horizontal
//! (axis = 1) axis for the mirroring operation" was that `axis = 0` reflects
//! *about* a vertical axis, i.e. swaps left and right. It does not: the probe
//! reported every mirrored orientation coming back as its opposite — 2 read as 4,
//! 4 as 2, 5 as 7, 7 as 5 — with the four pure rotations already correct. The
//! field names the *direction the image is flipped in*, so `axis = 0` swaps top
//! and bottom. Four of eight orientations were wrong in a way no amount of
//! re-reading the sentence would have shown.
//!
//! Engine A agreed with the source on all eight throughout, because it hands
//! ImageIO the orientation number and lets ImageIO write the boxes. That is what
//! makes it a usable oracle here: the disagreement could only be in our muxer.

/// A HEIF rotation-and-mirror pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeifTransform {
    /// `irot`'s angle field: quarter turns anti-clockwise, `0..=3`.
    pub rotate_ccw_quarters: u8,
    /// `imir`'s axis field, when a mirror is needed at all: `0` flips the image
    /// vertically (top and bottom swap), `1` flips it horizontally (left and
    /// right swap).
    ///
    /// That is the opposite of what the spec's wording first suggests, and it was
    /// measured — see the module docs.
    pub mirror_axis: Option<u8>,
}

impl HeifTransform {
    /// Whether this is the identity, i.e. what an unrotated capture needs.
    pub fn is_identity(self) -> bool {
        self.rotate_ccw_quarters == 0 && self.mirror_axis.is_none()
    }
}

/// The container transform equivalent to an Exif `Orientation`.
///
/// Anything outside `1..=8` is the identity: an out-of-range tag is a damaged
/// one, and declaring no transform matches what the pixels actually are.
pub fn heif_transform(exif_orientation: u8) -> HeifTransform {
    let (rotate_ccw_quarters, mirror_axis) = match exif_orientation {
        2 => (0, Some(1)),
        3 => (2, None),
        4 => (0, Some(0)),
        5 => (1, Some(0)),
        6 => (3, None),
        7 => (1, Some(1)),
        8 => (1, None),
        // 1, and anything a damaged file put there.
        _ => (0, None),
    };
    HeifTransform {
        rotate_ccw_quarters,
        mirror_axis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exif's own definition of an orientation, as a coordinate map.
    fn exif_maps(orientation: u8, w: i32, h: i32, x: i32, y: i32) -> (i32, i32) {
        match orientation {
            1 => (x, y),
            2 => (w - 1 - x, y),
            3 => (w - 1 - x, h - 1 - y),
            4 => (x, h - 1 - y),
            5 => (y, x),
            6 => (h - 1 - y, x),
            7 => (h - 1 - y, w - 1 - x),
            8 => (y, w - 1 - x),
            _ => (x, y),
        }
    }

    /// HEIF's definition, composed: rotate anti-clockwise, then mirror.
    ///
    /// Each step also carries the dimensions forward, because a quarter turn
    /// swaps them and the mirror that follows is about the *rotated* image's
    /// axis — which is exactly the detail that makes 5 and 7 easy to get wrong.
    fn heif_maps(t: HeifTransform, w: i32, h: i32, x: i32, y: i32) -> (i32, i32) {
        let (mut w, mut h, mut x, mut y) = (w, h, x, y);
        for _ in 0..t.rotate_ccw_quarters {
            // (x, y) -> (y, w-1-x), and the image becomes h x w.
            let (nx, ny) = (y, w - 1 - x);
            (w, h, x, y) = (h, w, nx, ny);
        }
        // axis 0 flips vertically, axis 1 horizontally — the measured reading,
        // not the one the spec's phrasing first suggests.
        match t.mirror_axis {
            Some(0) => y = h - 1 - y,
            Some(1) => x = w - 1 - x,
            _ => {}
        }
        (x, y)
    }

    /// The property the whole module exists for, checked on coordinates instead
    /// of by reading the arithmetic: for all eight orientations and every pixel
    /// of a deliberately non-square image, the boxes we emit land each pixel
    /// exactly where Exif says it goes.
    #[test]
    fn every_orientation_composes_to_the_exif_mapping() {
        // 5x3, so a transposed result is 3x5 and a wrong dimension swap shows up
        // as an out-of-range coordinate rather than a coincidence.
        const W: i32 = 5;
        const H: i32 = 3;
        for o in 1u8..=8 {
            let t = heif_transform(o);
            for y in 0..H {
                for x in 0..W {
                    assert_eq!(
                        heif_maps(t, W, H, x, y),
                        exif_maps(o, W, H, x, y),
                        "orientation {o} disagrees at ({x}, {y}) with {t:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn upright_and_nonsense_both_mean_no_transform() {
        assert!(heif_transform(1).is_identity());
        assert!(heif_transform(0).is_identity());
        assert!(heif_transform(9).is_identity());
        assert!(heif_transform(255).is_identity());
    }

    /// The four values that need a mirror are exactly the ones Exif defines with
    /// a reflection; a rotation-only file must not gain an `imir`.
    #[test]
    fn only_the_reflecting_orientations_get_a_mirror() {
        for o in [1u8, 3, 6, 8] {
            assert_eq!(heif_transform(o).mirror_axis, None, "orientation {o}");
        }
        for o in [2u8, 4, 5, 7] {
            assert!(heif_transform(o).mirror_axis.is_some(), "orientation {o}");
        }
    }

    /// A quarter turn is a quarter turn: the two pure rotations must not be the
    /// same box, which a sign error in the anti-clockwise convention would make
    /// them.
    #[test]
    fn the_two_quarter_turns_are_opposites() {
        assert_eq!(heif_transform(6).rotate_ccw_quarters, 3);
        assert_eq!(heif_transform(8).rotate_ccw_quarters, 1);
        assert_eq!(heif_transform(3).rotate_ccw_quarters, 2);
    }
}
