//! Fixed-capacity arrays in recorded physics inputs. Deserialize without
//! accepting an arbitrary vector length from an input-take file.
pub(super) mod array {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer, const N: usize>(
        values: &[T; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        values.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<[T; N], D::Error> {
        struct FixedArray<T, const N: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for FixedArray<T, N> {
            type Value = [T; N];
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "exactly {N} physics input slots")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::with_capacity(N);
                for index in 0..N {
                    values.push(
                        sequence
                            .next_element()?
                            .ok_or_else(|| A::Error::invalid_length(index, &self))?,
                    );
                }
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::invalid_length(N + 1, &self));
                }
                values
                    .try_into()
                    .map_err(|_| A::Error::custom("physics array length changed"))
            }
        }
        deserializer.deserialize_seq(FixedArray(std::marker::PhantomData))
    }
}
