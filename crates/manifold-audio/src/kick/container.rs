//! The kick model container: named little-endian arrays (docs/KICK_REALTIME_DESIGN.md
//! section 2). Python writer: `tools/audio_analysis/kick_container.py`; both sides
//! must agree byte for byte.

use std::collections::HashMap;

const MAGIC: &[u8; 8] = b"MKICK001";

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    F64(Vec<f64>),
    F32(Vec<f32>),
    I32(Vec<i32>),
    I16(Vec<i16>),
    I64(Vec<i64>),
    U8(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Array {
    pub shape: Vec<usize>,
    pub data: Data,
}

impl Array {
    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct Container {
    entries: HashMap<String, Array>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContainerError {
    BadMagic,
    Truncated,
    BadName,
    BadDtype(u8),
    Missing(String),
    WrongType { name: String, want: &'static str },
    WrongShape { name: String, want: Vec<usize>, got: Vec<usize> },
}

impl std::fmt::Display for ContainerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "kick container: {self:?}")
    }
}

impl std::error::Error for ContainerError {}

struct Reader<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ContainerError> {
        let end = self.o.checked_add(n).ok_or(ContainerError::Truncated)?;
        let s = self.b.get(self.o..end).ok_or(ContainerError::Truncated)?;
        self.o = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], ContainerError> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }
}

fn decode<T, const N: usize>(raw: &[u8], f: fn([u8; N]) -> T) -> Vec<T> {
    raw.chunks_exact(N).map(|c| f(c.try_into().expect("chunk of N bytes"))).collect()
}

impl Container {
    pub fn parse(bytes: &[u8]) -> Result<Self, ContainerError> {
        let mut r = Reader { b: bytes, o: 0 };
        if r.take(8)? != MAGIC {
            return Err(ContainerError::BadMagic);
        }
        let count = u32::from_le_bytes(r.arr()?);
        let mut entries = HashMap::with_capacity(count as usize);
        for _ in 0..count {
            let n = u16::from_le_bytes(r.arr()?) as usize;
            let name = std::str::from_utf8(r.take(n)?).map_err(|_| ContainerError::BadName)?.to_owned();
            let [code, ndim] = r.arr::<2>()?;
            let mut shape = Vec::with_capacity(ndim as usize);
            for _ in 0..ndim {
                shape.push(u64::from_le_bytes(r.arr()?) as usize);
            }
            let len: usize = shape.iter().product();
            let width = match code {
                0 | 4 => 8,
                1 | 2 => 4,
                3 => 2,
                5 => 1,
                c => return Err(ContainerError::BadDtype(c)),
            };
            let raw = r.take(len.checked_mul(width).ok_or(ContainerError::Truncated)?)?;
            let data = match code {
                0 => Data::F64(decode(raw, f64::from_le_bytes)),
                1 => Data::F32(decode(raw, f32::from_le_bytes)),
                2 => Data::I32(decode(raw, i32::from_le_bytes)),
                3 => Data::I16(decode(raw, i16::from_le_bytes)),
                4 => Data::I64(decode(raw, i64::from_le_bytes)),
                _ => Data::U8(raw.to_vec()),
            };
            entries.insert(name, Array { shape, data });
        }
        Ok(Self { entries })
    }

    pub fn get(&self, name: &str) -> Result<&Array, ContainerError> {
        self.entries.get(name).ok_or_else(|| ContainerError::Missing(name.to_owned()))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// An f64 array whose shape must equal `shape` (empty slice = scalar).
    pub fn f64s(&self, name: &str, shape: &[usize]) -> Result<&[f64], ContainerError> {
        let a = self.shaped(name, shape)?;
        match &a.data {
            Data::F64(v) => Ok(v),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "f64" }),
        }
    }

    pub fn f32s(&self, name: &str, shape: &[usize]) -> Result<&[f32], ContainerError> {
        let a = self.shaped(name, shape)?;
        match &a.data {
            Data::F32(v) => Ok(v),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "f32" }),
        }
    }

    pub fn i64s(&self, name: &str, shape: &[usize]) -> Result<&[i64], ContainerError> {
        let a = self.shaped(name, shape)?;
        match &a.data {
            Data::I64(v) => Ok(v),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "i64" }),
        }
    }

    pub fn i32s(&self, name: &str, shape: &[usize]) -> Result<&[i32], ContainerError> {
        let a = self.shaped(name, shape)?;
        match &a.data {
            Data::I32(v) => Ok(v),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "i32" }),
        }
    }

    pub fn i16s(&self, name: &str, shape: &[usize]) -> Result<&[i16], ContainerError> {
        let a = self.shaped(name, shape)?;
        match &a.data {
            Data::I16(v) => Ok(v),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "i16" }),
        }
    }

    pub fn text(&self, name: &str) -> Result<&str, ContainerError> {
        match &self.get(name)?.data {
            Data::U8(v) => std::str::from_utf8(v).map_err(|_| ContainerError::BadName),
            _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "u8 text" }),
        }
    }

    /// Any array's shape, for entries whose size the model decides (tree node counts).
    pub fn shape(&self, name: &str) -> Result<&[usize], ContainerError> {
        Ok(&self.get(name)?.shape)
    }

    fn shaped(&self, name: &str, shape: &[usize]) -> Result<&Array, ContainerError> {
        let a = self.get(name)?;
        if a.shape != shape {
            return Err(ContainerError::WrongShape { name: name.to_owned(), want: shape.to_vec(), got: a.shape.clone() });
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(out: &mut Vec<u8>, name: &str, code: u8, shape: &[u64], raw: &[u8]) {
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(name.as_bytes());
        out.extend([code, shape.len() as u8]);
        for d in shape {
            out.extend(d.to_le_bytes());
        }
        out.extend(raw);
    }

    #[test]
    fn parses_typed_entries_and_checks_shapes() {
        let mut b = MAGIC.to_vec();
        b.extend(3u32.to_le_bytes());
        let w: Vec<u8> = [1.5f64, -2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        entry(&mut b, "w", 0, &[2], &w);
        entry(&mut b, "recipe_version", 2, &[], &7i32.to_le_bytes());
        entry(&mut b, "train_id", 5, &[3], b"abc");
        let c = Container::parse(&b).unwrap();
        assert_eq!(c.f64s("w", &[2]).unwrap(), &[1.5, -2.0]);
        assert_eq!(c.i32s("recipe_version", &[]).unwrap(), &[7]);
        assert_eq!(c.text("train_id").unwrap(), "abc");
        assert!(matches!(c.f64s("w", &[3]), Err(ContainerError::WrongShape { .. })));
        assert!(matches!(c.f32s("w", &[2]), Err(ContainerError::WrongType { .. })));
        assert!(matches!(c.get("nope"), Err(ContainerError::Missing(_))));
        assert_eq!(Container::parse(&b[..b.len() - 1]).unwrap_err(), ContainerError::Truncated);
        assert_eq!(Container::parse(b"NOTKICK!").unwrap_err(), ContainerError::BadMagic);
    }
}
