use std::{borrow::Cow, sync::Arc};

use serde::de::{self, Deserialize, Deserializer, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeSeq, SerializeStruct, Serializer};
use serde::{Deserialize as DeriveDeserialize, Serialize as DeriveSerialize};

use super::{FieldValue, MAX_OPERATIONS, Operation};
use crate::{RadialField, SampledField, UniformField, VortexField};

#[derive(Debug, DeriveSerialize, DeriveDeserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum OperationRecord<'a> {
    Uniform {
        vector: [f32; 3],
    },
    Radial {
        center: [f32; 3],
        radius: f32,
        falloff: f32,
    },
    Vortex {
        center: [f32; 3],
        axis: [f32; 3],
        radius: f32,
        falloff: f32,
    },
    Sampled {
        origin: [f32; 3],
        #[serde(rename = "cellSize")]
        cell_size: f32,
        dimensions: [u32; 3],
        values: Cow<'a, [[f32; 3]]>,
    },
    Add,
    Multiply,
    Scale {
        strength: f32,
    },
}

#[derive(Debug, DeriveSerialize, DeriveDeserialize)]
#[serde(rename_all = "camelCase")]
struct FieldValueRecord {
    operations: BoundedOperations<'static>,
}

#[derive(Debug)]
struct BoundedOperations<'a> {
    entries: [Option<OperationRecord<'a>>; MAX_OPERATIONS],
    len: usize,
}

impl Serialize for BoundedOperations<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for operation in self.entries[..self.len].iter().flatten() {
            sequence.serialize_element(operation)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for BoundedOperations<'static> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OperationsVisitor;

        impl<'de> Visitor<'de> for OperationsVisitor {
            type Value = BoundedOperations<'static>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array containing between 1 and 32 field operations")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                if sequence
                    .size_hint()
                    .is_some_and(|size| size > MAX_OPERATIONS)
                {
                    return Err(de::Error::custom("field value exceeds 32 operations"));
                }
                let mut entries: [Option<OperationRecord<'static>>; MAX_OPERATIONS] =
                    std::array::from_fn(|_| None);
                let mut len = 0;
                while len < MAX_OPERATIONS {
                    let Some(operation) = sequence.next_element::<OperationRecord<'static>>()?
                    else {
                        break;
                    };
                    entries[len] = Some(operation);
                    len += 1;
                }
                if len == MAX_OPERATIONS && sequence.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom("field value exceeds 32 operations"));
                }
                if len == 0 {
                    return Err(de::Error::custom(
                        "field value requires at least one operation",
                    ));
                }
                Ok(BoundedOperations { entries, len })
            }
        }

        deserializer.deserialize_seq(OperationsVisitor)
    }
}

impl Serialize for FieldValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let operations = BoundedOperations {
            entries: std::array::from_fn(|index| {
                if index >= self.len {
                    return None;
                }
                Some(match &self.operations[index] {
                    Operation::Uniform(field) => OperationRecord::Uniform {
                        vector: field.vector(),
                    },
                    Operation::Radial(field) => OperationRecord::Radial {
                        center: field.center(),
                        radius: field.radius(),
                        falloff: field.falloff(),
                    },
                    Operation::Vortex(field) => OperationRecord::Vortex {
                        center: field.center(),
                        axis: field.axis(),
                        radius: field.radius(),
                        falloff: field.falloff(),
                    },
                    Operation::Sampled(field) => OperationRecord::Sampled {
                        origin: field.origin(),
                        cell_size: field.cell_size(),
                        dimensions: field.dimensions(),
                        values: Cow::Borrowed(field.values()),
                    },
                    Operation::Add => OperationRecord::Add,
                    Operation::Multiply => OperationRecord::Multiply,
                    Operation::Scale(strength) => OperationRecord::Scale {
                        strength: *strength,
                    },
                    Operation::Unused => unreachable!("field program is privately constructed"),
                })
            }),
            len: self.len,
        };
        let mut record = serializer.serialize_struct("FieldValue", 1)?;
        record.serialize_field("operations", &operations)?;
        record.end()
    }
}

impl<'de> Deserialize<'de> for FieldValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut record = FieldValueRecord::deserialize(deserializer)?;
        let mut operations: [Operation; MAX_OPERATIONS] =
            std::array::from_fn(|_| Operation::Unused);
        let mut depth = 0usize;
        for index in 0..record.operations.len {
            let operation = record.operations.entries[index]
                .take()
                .expect("bounded operation entry is populated");
            let operation = match operation {
                OperationRecord::Uniform { vector } => {
                    depth += 1;
                    UniformField::new(vector)
                        .map(Operation::Uniform)
                        .map_err(invalid::<D::Error>)?
                }
                OperationRecord::Radial {
                    center,
                    radius,
                    falloff,
                } => {
                    depth += 1;
                    RadialField::new(center, radius, falloff)
                        .map(Operation::Radial)
                        .map_err(invalid::<D::Error>)?
                }
                OperationRecord::Vortex {
                    center,
                    axis,
                    radius,
                    falloff,
                } => {
                    depth += 1;
                    VortexField::new_normalized(center, axis, radius, falloff)
                        .map(Operation::Vortex)
                        .map_err(invalid::<D::Error>)?
                }
                OperationRecord::Sampled {
                    origin,
                    cell_size,
                    dimensions,
                    values,
                } => {
                    depth += 1;
                    SampledField::new(origin, cell_size, dimensions, values.into_owned())
                        .map(|field| Operation::Sampled(Arc::new(field)))
                        .map_err(invalid::<D::Error>)?
                }
                OperationRecord::Add => {
                    require_stack(&mut depth, 2).map_err(invalid_message::<D::Error>)?;
                    depth -= 1;
                    Operation::Add
                }
                OperationRecord::Multiply => {
                    require_stack(&mut depth, 2).map_err(invalid_message::<D::Error>)?;
                    depth -= 1;
                    Operation::Multiply
                }
                OperationRecord::Scale { strength } => {
                    if !strength.is_finite() {
                        return Err(de::Error::custom("vector field scale must be finite"));
                    }
                    require_stack(&mut depth, 1).map_err(invalid_message::<D::Error>)?;
                    Operation::Scale(strength)
                }
            };
            operations[index] = operation;
        }
        if depth != 1 {
            return Err(de::Error::custom(
                "field value postfix program must end at stack depth one",
            ));
        }
        Ok(FieldValue {
            operations,
            len: record.operations.len,
        })
    }
}

fn require_stack(depth: &mut usize, required: usize) -> Result<(), &'static str> {
    if *depth < required {
        Err("field value postfix operation underflows the stack")
    } else {
        Ok(())
    }
}

fn invalid<E>(error: crate::PhysicsError) -> E
where
    E: de::Error,
{
    E::custom(error.to_string())
}

fn invalid_message<E>(message: &'static str) -> E
where
    E: de::Error,
{
    E::custom(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VectorField;

    fn round_trip(field: FieldValue) {
        let json = serde_json::to_string(&field).unwrap();
        let restored: FieldValue = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, field, "{json}");
        for position in [[0.0, 0.0, 0.0], [0.25, 0.5, 0.75], [3.0, -1.0, 2.0]] {
            assert_eq!(restored.sample(position), field.sample(position));
        }
    }

    #[test]
    fn field_value_serde_round_trips_composed_leaf_program() {
        let grid = Arc::new(
            SampledField::new(
                [-1.0, -1.0, -1.0],
                0.5,
                [2, 2, 2],
                vec![[0.25, 0.5, -0.75]; 8],
            )
            .unwrap(),
        );
        let field = FieldValue::uniform([1.0, -2.0, 0.5])
            .unwrap()
            .sum(&FieldValue::radial([0.0; 3], 3.0, 1.5).unwrap())
            .unwrap()
            .multiply(&FieldValue::vortex([0.0, 0.25, 0.0], [1.0, 2.0, 3.0], 4.0, 0.5).unwrap())
            .unwrap()
            .sum(&FieldValue::sampled(grid))
            .unwrap()
            .scaled(-0.75)
            .unwrap();
        let json = serde_json::to_value(&field).unwrap();
        assert!(
            json["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation.get("cellSize").is_some())
        );
        round_trip(field);
    }

    #[test]
    fn field_value_serde_round_trips_single_operation() {
        let field = FieldValue::uniform([1.0, -2.0, 0.5]).unwrap();
        round_trip(field);
    }

    #[test]
    fn field_value_serde_preserves_normalized_vortex_axis_bits() {
        let field = FieldValue::vortex([1.0, 2.0, 3.0], [1.0, 2.0, 3.0], 4.0, 0.5).unwrap();
        let json = serde_json::to_string(&field).unwrap();
        let restored: FieldValue = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, field);
    }

    #[test]
    fn field_value_serde_rejects_malformed_programs_and_leaves() {
        for json in [
            r#"{"operations":[]}"#,
            r#"{"operations":[{"type":"add"}]}"#,
            r#"{"operations":[{"type":"uniform","vector":[1.0,2.0,3.0]},{"type":"add"}]}"#,
            r#"{"operations":[{"type":"uniform","vector":[1.0,2.0,3.0]},{"type":"uniform","vector":[1.0,2.0,3.0]}]}"#,
            r#"{"operations":[{"type":"scale","strength":null}]}"#,
            r#"{"operations":[{"type":"unknown"}]}"#,
            r#"{"operations":[{"type":"radial","center":[0.0,0.0,0.0],"radius":0.0,"falloff":1.0}]}"#,
            r#"{"operations":[{"type":"vortex","center":[0.0,0.0,0.0],"axis":[2.0,0.0,0.0],"radius":1.0,"falloff":1.0}]}"#,
        ] {
            assert!(serde_json::from_str::<FieldValue>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn field_value_serde_rejects_oversized_program_while_reading_operations() {
        let operations = (0..=MAX_OPERATIONS)
            .map(|_| serde_json::json!({ "type": "uniform", "vector": [1.0, 0.0, 0.0] }))
            .collect::<Vec<_>>();
        let json = serde_json::json!({ "operations": operations });
        assert!(serde_json::from_value::<FieldValue>(json).is_err());
    }

    #[test]
    fn field_value_serde_rejects_bad_sampled_grid() {
        let json = r#"{"operations":[{"type":"sampled","origin":[0.0,0.0,0.0],"cellSize":1.0,"dimensions":[2,2,2],"values":[] }]}"#;
        assert!(serde_json::from_str::<FieldValue>(json).is_err());
    }
}
