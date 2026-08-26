use std::error::Error;
use std::fmt::{Display, Formatter};

const SINGULARITY_EPSILON: f64 = 1.0e-12;

/// A conventional six-coefficient affine transform:
///
/// ```text
/// x' = a*x + c*y + e
/// y' = b*x + d*y + f
/// ```
///
/// This type deliberately does not implement a Virtual Spread manifest parser. The
/// representation producer remains responsible for authenticating and decoding its
/// schema; InkBridge can then validate the six authoritative coefficients here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTransform {
    coefficients: [f64; 6],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffinePoint {
    pub x: f64,
    pub y: f64,
}

impl AffinePoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AffineError {
    NonFiniteCoefficient,
    SingularTransform,
    NonFinitePoint,
    NonFiniteResult,
    InvalidTolerance,
    RoundTripExceeded {
        expected: AffinePoint,
        actual: AffinePoint,
        tolerance: f64,
    },
}

impl Display for AffineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteCoefficient => {
                write!(
                    formatter,
                    "affine transform contains a non-finite coefficient"
                )
            }
            Self::SingularTransform => write!(formatter, "affine transform is singular"),
            Self::NonFinitePoint => write!(formatter, "affine input point is not finite"),
            Self::NonFiniteResult => write!(formatter, "affine result is not finite"),
            Self::InvalidTolerance => {
                write!(
                    formatter,
                    "affine round-trip tolerance must be finite and nonnegative"
                )
            }
            Self::RoundTripExceeded {
                expected,
                actual,
                tolerance,
            } => write!(
                formatter,
                "affine round trip exceeded tolerance {tolerance}: expected ({}, {}), got ({}, {})",
                expected.x, expected.y, actual.x, actual.y
            ),
        }
    }
}

impl Error for AffineError {}

impl AffineTransform {
    pub fn new(coefficients: [f64; 6]) -> Result<Self, AffineError> {
        if !coefficients.iter().all(|value| value.is_finite()) {
            return Err(AffineError::NonFiniteCoefficient);
        }
        let transform = Self { coefficients };
        transform.inverse()?;
        Ok(transform)
    }

    pub const fn coefficients(self) -> [f64; 6] {
        self.coefficients
    }

    pub fn apply(self, point: AffinePoint) -> Result<AffinePoint, AffineError> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(AffineError::NonFinitePoint);
        }
        let [a, b, c, d, e, f] = self.coefficients;
        let result = AffinePoint {
            x: a * point.x + c * point.y + e,
            y: b * point.x + d * point.y + f,
        };
        if !result.x.is_finite() || !result.y.is_finite() {
            return Err(AffineError::NonFiniteResult);
        }
        Ok(result)
    }

    pub fn inverse(self) -> Result<Self, AffineError> {
        let [a, b, c, d, e, f] = self.coefficients;
        let linear_scale = a.abs().max(b.abs()).max(c.abs()).max(d.abs());
        if linear_scale == 0.0 {
            return Err(AffineError::SingularTransform);
        }

        // Normalize before computing the determinant. This makes the
        // condition check relative to the actual linear coefficients and
        // avoids overflow/underflow when coordinate units differ greatly.
        let normalized = [
            a / linear_scale,
            b / linear_scale,
            c / linear_scale,
            d / linear_scale,
        ];
        let normalized_determinant = normalized[0] * normalized[3] - normalized[1] * normalized[2];
        if !normalized_determinant.is_finite()
            || normalized_determinant.abs() <= SINGULARITY_EPSILON
        {
            return Err(AffineError::SingularTransform);
        }
        let inverse_factor = (1.0 / linear_scale) / normalized_determinant;
        let inverse_a = normalized[3] * inverse_factor;
        let inverse_b = -normalized[1] * inverse_factor;
        let inverse_c = -normalized[2] * inverse_factor;
        let inverse_d = normalized[0] * inverse_factor;
        let inverse = [
            inverse_a,
            inverse_b,
            inverse_c,
            inverse_d,
            -(inverse_a * e + inverse_c * f),
            -(inverse_b * e + inverse_d * f),
        ];
        if !inverse.iter().all(|value| value.is_finite()) {
            return Err(AffineError::NonFiniteResult);
        }
        Ok(Self {
            coefficients: inverse,
        })
    }

    pub fn validate_round_trip(
        self,
        points: &[AffinePoint],
        tolerance: f64,
    ) -> Result<(), AffineError> {
        if !tolerance.is_finite() || tolerance < 0.0 {
            return Err(AffineError::InvalidTolerance);
        }
        let inverse = self.inverse()?;
        for expected in points {
            let actual = inverse.apply(self.apply(*expected)?)?;
            if (actual.x - expected.x).abs() > tolerance
                || (actual.y - expected.y).abs() > tolerance
            {
                return Err(AffineError::RoundTripExceeded {
                    expected: *expected,
                    actual,
                    tolerance,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_inverse_of_a_scaled_translated_mapping() {
        let forward = AffineTransform::new([2.5, 0.0, 0.0, 2.5, 100.0, 20.0]).unwrap();
        let source = AffinePoint::new(12.0, 30.0);
        let virtual_point = forward.apply(source).unwrap();
        assert_eq!(virtual_point, AffinePoint::new(130.0, 95.0));
        let recovered = forward.inverse().unwrap().apply(virtual_point).unwrap();
        assert!((recovered.x - source.x).abs() < 1.0e-12);
        assert!((recovered.y - source.y).abs() < 1.0e-12);
    }

    #[test]
    fn validates_rotated_page_round_trips() {
        let forward = AffineTransform::new([0.0, 0.75, -0.75, 0.0, 612.0, 24.0]).unwrap();
        forward
            .validate_round_trip(
                &[
                    AffinePoint::new(0.0, 0.0),
                    AffinePoint::new(612.0, 792.0),
                    AffinePoint::new(143.25, 401.5),
                ],
                1.0e-9,
            )
            .unwrap();
    }

    #[test]
    fn rejects_non_finite_and_nearly_singular_mappings() {
        assert_eq!(
            AffineTransform::new([f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]),
            Err(AffineError::NonFiniteCoefficient)
        );
        assert_eq!(
            AffineTransform::new([1.0, 1.0, 1.0, 1.0 + 1.0e-14, 0.0, 0.0]),
            Err(AffineError::SingularTransform)
        );
    }

    #[test]
    fn accepts_well_conditioned_mappings_across_coordinate_units() {
        for scale in [1.0e-100, 1.0e-7, 1.0e100] {
            let forward = AffineTransform::new([scale, 0.0, 0.0, scale, 0.0, 0.0]).unwrap();
            let source = AffinePoint::new(2.0, 3.0);
            let recovered = forward
                .inverse()
                .unwrap()
                .apply(forward.apply(source).unwrap())
                .unwrap();
            assert!((recovered.x - source.x).abs() < 1.0e-12);
            assert!((recovered.y - source.y).abs() < 1.0e-12);
        }
    }

    #[test]
    fn rejects_non_finite_results_and_bad_tolerances() {
        let forward = AffineTransform::new([1.0e150, 0.0, 0.0, 1.0e150, 0.0, 0.0]).unwrap();
        assert_eq!(
            forward.apply(AffinePoint::new(1.0e200, 1.0)),
            Err(AffineError::NonFiniteResult)
        );
        assert_eq!(
            forward.validate_round_trip(&[], -1.0),
            Err(AffineError::InvalidTolerance)
        );
    }
}
