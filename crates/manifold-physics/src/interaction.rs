use super::PhysicsError;

/// A dimensionless world-space vector evaluated at a world-space position.
pub trait VectorField: Send + Sync {
    fn sample(&self, position: [f32; 3]) -> [f32; 3];
}

/// A field and the two inputs consumed by a physics tick.
#[derive(Clone, Copy)]
pub struct FieldInput<'a> {
    pub field: &'a dyn VectorField,
    pub acceleration: f32,
    pub delta_velocity: f32,
}

/// Identifies a published simulation tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickStamp {
    pub epoch: u64,
    pub tick: u64,
}

/// A constant vector field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniformField {
    vector: [f32; 3],
}

impl UniformField {
    pub fn new(vector: [f32; 3]) -> Result<Self, PhysicsError> {
        if vector.iter().all(|component| component.is_finite()) {
            Ok(Self { vector })
        } else {
            Err(PhysicsError::InvalidInput("uniform field must be finite"))
        }
    }

    pub fn vector(&self) -> [f32; 3] {
        self.vector
    }
}

impl VectorField for UniformField {
    fn sample(&self, _position: [f32; 3]) -> [f32; 3] {
        self.vector
    }
}

/// An outward-pointing field centred at a position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialField {
    center: [f32; 3],
    radius: f32,
    falloff: f32,
}

impl RadialField {
    pub fn new(center: [f32; 3], radius: f32, falloff: f32) -> Result<Self, PhysicsError> {
        if !center.iter().all(|component| component.is_finite()) {
            return Err(PhysicsError::InvalidInput(
                "radial field center must be finite",
            ));
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "radial field radius must be finite and positive",
            ));
        }
        if !falloff.is_finite() || falloff < 0.0 {
            return Err(PhysicsError::InvalidInput(
                "radial field falloff must be finite and non-negative",
            ));
        }
        Ok(Self {
            center,
            radius,
            falloff,
        })
    }

    pub fn center(&self) -> [f32; 3] {
        self.center
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    pub fn falloff(&self) -> f32 {
        self.falloff
    }
}

impl VectorField for RadialField {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        if !position.iter().all(|component| component.is_finite()) {
            return [0.0; 3];
        }
        let offset = subtract(position, self.center);
        let distance = length(offset);
        if !distance.is_finite() || distance <= f32::EPSILON || distance >= self.radius {
            return [0.0; 3];
        }
        let magnitude = (1.0 - distance / self.radius).powf(self.falloff);
        scale(offset, magnitude / distance)
    }
}

/// A tangential field around an axis, with cylindrical radial falloff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VortexField {
    center: [f32; 3],
    axis: [f32; 3],
    radius: f32,
    falloff: f32,
}

impl VortexField {
    pub fn new(
        center: [f32; 3],
        axis: [f32; 3],
        radius: f32,
        falloff: f32,
    ) -> Result<Self, PhysicsError> {
        if !center.iter().all(|component| component.is_finite()) {
            return Err(PhysicsError::InvalidInput(
                "vortex field center must be finite",
            ));
        }
        let axis = normalize(axis).ok_or(PhysicsError::InvalidInput(
            "vortex field axis must be finite and non-zero",
        ))?;
        if !radius.is_finite() || radius <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "vortex field radius must be finite and positive",
            ));
        }
        if !falloff.is_finite() || falloff < 0.0 {
            return Err(PhysicsError::InvalidInput(
                "vortex field falloff must be finite and non-negative",
            ));
        }
        Ok(Self {
            center,
            axis,
            radius,
            falloff,
        })
    }

    pub fn center(&self) -> [f32; 3] {
        self.center
    }

    pub fn axis(&self) -> [f32; 3] {
        self.axis
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    pub fn falloff(&self) -> f32 {
        self.falloff
    }
}

impl VectorField for VortexField {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        if !position.iter().all(|component| component.is_finite()) {
            return [0.0; 3];
        }
        let offset = subtract(position, self.center);
        let axial = dot(offset, self.axis);
        let radial = subtract(offset, scale(self.axis, axial));
        let distance = length(radial);
        if !distance.is_finite() || distance <= f32::EPSILON || distance >= self.radius {
            return [0.0; 3];
        }
        let magnitude = (1.0 - distance / self.radius).powf(self.falloff);
        let tangent = scale(cross(self.axis, radial), magnitude / distance);
        if tangent.iter().all(|component| component.is_finite()) {
            tangent
        } else {
            [0.0; 3]
        }
    }
}

/// A uniformly spaced, x-fastest vector grid.
#[derive(Clone, Debug, PartialEq)]
pub struct SampledField {
    origin: [f32; 3],
    cell_size: f32,
    dimensions: [u32; 3],
    values: Vec<[f32; 3]>,
}

impl SampledField {
    pub fn new(
        origin: [f32; 3],
        cell_size: f32,
        dimensions: [u32; 3],
        values: Vec<[f32; 3]>,
    ) -> Result<Self, PhysicsError> {
        if !origin.iter().all(|component| component.is_finite()) {
            return Err(PhysicsError::InvalidInput(
                "sampled field origin must be finite",
            ));
        }
        if !cell_size.is_finite() || cell_size <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "sampled field cell size must be finite and positive",
            ));
        }
        if dimensions.iter().any(|&dimension| dimension < 2) {
            return Err(PhysicsError::InvalidInput(
                "sampled field dimensions must be at least two",
            ));
        }
        let expected = (dimensions[0] as usize)
            .checked_mul(dimensions[1] as usize)
            .and_then(|count| count.checked_mul(dimensions[2] as usize))
            .ok_or(PhysicsError::InvalidInput(
                "sampled field dimensions are too large",
            ))?;
        if values.len() != expected {
            return Err(PhysicsError::InvalidInput(
                "sampled field value count does not match dimensions",
            ));
        }
        if values
            .iter()
            .any(|value| value.iter().any(|component| !component.is_finite()))
        {
            return Err(PhysicsError::InvalidInput(
                "sampled field values must be finite",
            ));
        }
        for (axis, &dimension) in dimensions.iter().enumerate() {
            let extent = cell_size as f64 * (dimension - 1) as f64;
            let upper_bound = origin[axis] as f64 + extent;
            if !extent.is_finite()
                || extent > f32::MAX as f64
                || !upper_bound.is_finite()
                || upper_bound > f32::MAX as f64
            {
                return Err(PhysicsError::InvalidInput(
                    "sampled field bounds must be finite",
                ));
            }
        }
        Ok(Self {
            origin,
            cell_size,
            dimensions,
            values,
        })
    }

    pub fn origin(&self) -> [f32; 3] {
        self.origin
    }

    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    pub fn dimensions(&self) -> [u32; 3] {
        self.dimensions
    }
}

impl VectorField for SampledField {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        if !position.iter().all(|component| component.is_finite()) {
            return [0.0; 3];
        }

        let mut lower = [0usize; 3];
        let mut fraction = [0.0f32; 3];
        for axis in 0..3 {
            let coordinate = (position[axis] - self.origin[axis]) / self.cell_size;
            let maximum = (self.dimensions[axis] - 1) as f32;
            if !coordinate.is_finite() || coordinate < 0.0 || coordinate > maximum {
                return [0.0; 3];
            }
            if coordinate >= maximum {
                lower[axis] = self.dimensions[axis] as usize - 2;
                fraction[axis] = 1.0;
            } else {
                lower[axis] = coordinate.floor() as usize;
                fraction[axis] = coordinate - lower[axis] as f32;
            }
        }

        let x = lower[0];
        let y = lower[1];
        let z = lower[2];
        let nx = self.dimensions[0] as usize;
        let ny = self.dimensions[1] as usize;
        let index = |ix: usize, iy: usize, iz: usize| ix + nx * (iy + ny * iz);
        let c000 = self.values[index(x, y, z)];
        let c100 = self.values[index(x + 1, y, z)];
        let c010 = self.values[index(x, y + 1, z)];
        let c110 = self.values[index(x + 1, y + 1, z)];
        let c001 = self.values[index(x, y, z + 1)];
        let c101 = self.values[index(x + 1, y, z + 1)];
        let c011 = self.values[index(x, y + 1, z + 1)];
        let c111 = self.values[index(x + 1, y + 1, z + 1)];
        let c00 = lerp(c000, c100, fraction[0]);
        let c10 = lerp(c010, c110, fraction[0]);
        let c01 = lerp(c001, c101, fraction[0]);
        let c11 = lerp(c011, c111, fraction[0]);
        let c0 = lerp(c00, c10, fraction[1]);
        let c1 = lerp(c01, c11, fraction[1]);
        lerp(c0, c1, fraction[2])
    }
}

/// A scalar multiple of a borrowed field.
pub struct ScaledField<'a> {
    field: &'a dyn VectorField,
    multiplier: f32,
}

impl<'a> ScaledField<'a> {
    pub fn new(field: &'a dyn VectorField, multiplier: f32) -> Result<Self, PhysicsError> {
        if !multiplier.is_finite() {
            return Err(PhysicsError::InvalidInput(
                "scaled field multiplier must be finite",
            ));
        }
        Ok(Self { field, multiplier })
    }

    pub fn multiplier(&self) -> f32 {
        self.multiplier
    }
}

impl VectorField for ScaledField<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        scale(self.field.sample(position), self.multiplier)
    }
}

/// The component-wise sum of borrowed fields.
pub struct SumField<'a> {
    fields: &'a [&'a dyn VectorField],
}

impl<'a> SumField<'a> {
    pub fn new(fields: &'a [&'a dyn VectorField]) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[&'a dyn VectorField] {
        self.fields
    }
}

impl VectorField for SumField<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        self.fields
            .iter()
            .fold([0.0; 3], |sum, field| add(sum, field.sample(position)))
    }
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(value: [f32; 3], scalar: f32) -> [f32; 3] {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length(value: [f32; 3]) -> f32 {
    dot(value, value).sqrt()
}

fn normalize(value: [f32; 3]) -> Option<[f32; 3]> {
    if !value.iter().all(|component| component.is_finite()) {
        return None;
    }
    let magnitude = length(value);
    if !magnitude.is_finite() || magnitude <= 0.0 {
        None
    } else {
        Some(scale(value, 1.0 / magnitude))
    }
}

fn lerp(left: [f32; 3], right: [f32; 3], amount: f32) -> [f32; 3] {
    [
        left[0] + (right[0] - left[0]) * amount,
        left[1] + (right[1] - left[1]) * amount,
        left[2] + (right[2] - left[2]) * amount,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-5, "{actual} != {expected}");
        }
    }

    #[test]
    fn scene_physics_uniform_field_samples_constant_vector() {
        let field = UniformField::new([1.0, -2.0, 0.5]).unwrap();
        assert_eq!(field.sample([4.0, 5.0, 6.0]), [1.0, -2.0, 0.5]);
        assert!(UniformField::new([f32::NAN, 0.0, 0.0]).is_err());
    }

    #[test]
    fn scene_physics_radial_field_has_falloff_and_zero_centre_and_outside() {
        let field = RadialField::new([1.0, 2.0, 3.0], 2.0, 1.0).unwrap();
        assert_eq!(field.sample([1.0, 2.0, 3.0]), [0.0; 3]);
        assert_vec3_close(field.sample([2.0, 2.0, 3.0]), [0.5, 0.0, 0.0]);
        assert_eq!(field.sample([4.0, 2.0, 3.0]), [0.0; 3]);
        assert!(RadialField::new([0.0; 3], 0.0, 1.0).is_err());
        assert!(RadialField::new([0.0; 3], 1.0, -1.0).is_err());
    }

    #[test]
    fn scene_physics_vortex_field_uses_arbitrary_axis_and_zero_axis_and_outside() {
        let axis_component = 2.0_f32.sqrt().recip();
        let field =
            VortexField::new([0.0; 3], [axis_component, axis_component, 0.0], 2.0, 1.0).unwrap();
        let radial = [axis_component, -axis_component, 0.0];
        assert_vec3_close(
            field.sample([0.5 * radial[0], 0.5 * radial[1], 0.0]),
            [0.0, 0.0, -0.75],
        );
        assert_vec3_close(
            field.sample([axis_component, axis_component, 0.0]),
            [0.0; 3]
        );
        assert_vec3_close(
            field.sample([2.0 * radial[0], 2.0 * radial[1], 0.0]),
            [0.0; 3]
        );
        assert!(VortexField::new([0.0; 3], [0.0; 3], 1.0, 1.0).is_err());
    }

    #[test]
    fn scene_physics_scaled_and_sum_fields_support_negative_and_composed_values() {
        let first = UniformField::new([1.0, 2.0, 3.0]).unwrap();
        let second = UniformField::new([-2.0, 1.0, 0.5]).unwrap();
        let scaled = ScaledField::new(&first, -2.0).unwrap();
        let fields: [&dyn VectorField; 2] = [&scaled, &second];
        let sum = SumField::new(&fields);
        assert_eq!(scaled.sample([0.0; 3]), [-2.0, -4.0, -6.0]);
        assert_eq!(sum.sample([0.0; 3]), [-4.0, -3.0, -5.5]);
        assert!(ScaledField::new(&first, f32::INFINITY).is_err());
    }

    #[test]
    fn scene_physics_sampled_field_trilinearly_interpolates_x_fastest_grid() {
        let values = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
        ];
        let field = SampledField::new([10.0, 20.0, 30.0], 2.0, [2, 2, 2], values).unwrap();
        assert_vec3_close(field.sample([11.0, 21.0, 31.0]), [0.5, 0.5, 0.5]);
        assert_eq!(field.sample([12.0, 22.0, 32.0]), [1.0, 1.0, 1.0]);
        assert_eq!(field.sample([9.99, 21.0, 31.0]), [0.0; 3]);
        assert_eq!(field.sample([12.01, 21.0, 31.0]), [0.0; 3]);
    }

    #[test]
    fn scene_physics_sampled_field_rejects_malformed_grids_and_nonfinite_inputs() {
        assert!(SampledField::new([0.0; 3], 1.0, [1, 2, 2], vec![]).is_err());
        assert!(SampledField::new([0.0; 3], 1.0, [2, 2, 2], vec![]).is_err());
        assert!(
            SampledField::new([0.0; 3], 1.0, [2, 2, 2], vec![[0.0, 0.0, f32::NAN]; 8],).is_err()
        );
        assert!(SampledField::new([0.0; 3], f32::NAN, [2, 2, 2], vec![[0.0; 3]; 8]).is_err());
    }
}
