use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, SeqAccess, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn into_vec(self) -> Vec<T> {
        match self {
            OneOrMany::One(v) => vec![v],
            OneOrMany::Many(v) => v,
        }
    }

    pub fn as_slice(&self) -> &[T] {
        match self {
            OneOrMany::One(v) => std::slice::from_ref(v),
            OneOrMany::Many(v) => v.as_slice(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            OneOrMany::One(_) => 1,
            OneOrMany::Many(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> OneOrManyIter<'_, T> {
        OneOrManyIter {
            inner: self,
            index: 0,
        }
    }
}

impl<T> IntoIterator for OneOrMany<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a OneOrMany<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

pub struct OneOrManyIter<'a, T> {
    inner: &'a OneOrMany<T>,
    index: usize,
}

impl<'a, T> Iterator for OneOrManyIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let slice = self.inner.as_slice();
        if self.index < slice.len() {
            let item = &slice[self.index];
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.inner.len();
        (len, Some(len))
    }
}

impl<T: Serialize> Serialize for OneOrMany<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            OneOrMany::One(v) => v.serialize(serializer),
            OneOrMany::Many(v) => v.serialize(serializer),
        }
    }
}

struct OneOrManyVisitor<T>(PhantomData<T>);

impl<'de, T: de::DeserializeOwned> Visitor<'de> for OneOrManyVisitor<T> {
    type Value = OneOrMany<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a single value or an array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<T>()? {
            items.push(item);
        }
        Ok(OneOrMany::Many(items))
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        T::deserialize(de::value::BoolDeserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_i8<E: de::Error>(self, v: i8) -> Result<Self::Value, E> {
        T::deserialize(de::value::I8Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_i16<E: de::Error>(self, v: i16) -> Result<Self::Value, E> {
        T::deserialize(de::value::I16Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_i32<E: de::Error>(self, v: i32) -> Result<Self::Value, E> {
        T::deserialize(de::value::I32Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        T::deserialize(de::value::I64Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_u8<E: de::Error>(self, v: u8) -> Result<Self::Value, E> {
        T::deserialize(de::value::U8Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_u16<E: de::Error>(self, v: u16) -> Result<Self::Value, E> {
        T::deserialize(de::value::U16Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_u32<E: de::Error>(self, v: u32) -> Result<Self::Value, E> {
        T::deserialize(de::value::U32Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        T::deserialize(de::value::U64Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_f32<E: de::Error>(self, v: f32) -> Result<Self::Value, E> {
        T::deserialize(de::value::F32Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        T::deserialize(de::value::F64Deserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_char<E: de::Error>(self, v: char) -> Result<Self::Value, E> {
        T::deserialize(de::value::CharDeserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        T::deserialize(de::value::StrDeserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        T::deserialize(de::value::StringDeserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
        T::deserialize(de::value::BytesDeserializer::new(v)).map(OneOrMany::One)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        T::deserialize(de::value::UnitDeserializer::new()).map(OneOrMany::One)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        T::deserialize(de::value::UnitDeserializer::new()).map(OneOrMany::One)
    }
}

impl<'de, T: de::DeserializeOwned> Deserialize<'de> for OneOrMany<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(OneOrManyVisitor::<T>(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_string() {
        let json = "\"https://example.com/file.zip\"";
        let result: OneOrMany<String> = serde_json::from_str(json).unwrap();
        assert_eq!(result.into_vec(), vec!["https://example.com/file.zip"]);
    }

    #[test]
    fn test_array_strings() {
        let json = "[\"a.zip\", \"b.zip\"]";
        let result: OneOrMany<String> = serde_json::from_str(json).unwrap();
        assert_eq!(result.into_vec(), vec!["a.zip", "b.zip"]);
    }

    #[test]
    fn test_single_number() {
        let json = "42";
        let result: OneOrMany<u32> = serde_json::from_str(json).unwrap();
        assert_eq!(result.into_vec(), vec![42]);
    }

    #[test]
    fn test_array_numbers() {
        let json = "[1, 2, 3]";
        let result: OneOrMany<u32> = serde_json::from_str(json).unwrap();
        assert_eq!(result.into_vec(), vec![1, 2, 3]);
    }

    #[test]
    fn test_serialize_single() {
        let val = OneOrMany::One("hello".to_string());
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, "\"hello\"");
    }

    #[test]
    fn test_serialize_array() {
        let val = OneOrMany::Many(vec!["a".to_string(), "b".to_string()]);
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, "[\"a\",\"b\"]");
    }

    #[test]
    fn test_iter() {
        let val = OneOrMany::Many(vec![1, 2, 3]);
        let collected: Vec<i32> = val.iter().copied().collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn test_as_slice() {
        let val = OneOrMany::One(42);
        assert_eq!(val.as_slice(), &[42]);

        let val = OneOrMany::Many(vec![1, 2]);
        assert_eq!(val.as_slice(), &[1, 2]);
    }

    #[test]
    fn test_len() {
        assert_eq!(OneOrMany::One::<i32>(1).len(), 1);
        assert_eq!(OneOrMany::Many::<i32>(vec![]).len(), 0);
        assert_eq!(OneOrMany::Many::<i32>(vec![1, 2]).len(), 2);
    }
}
