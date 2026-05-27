//! Hex-string serde adapters for byte arrays/vectors. Keeps the JSON envelope
//! human-debuggable (`curl /v0/attestation` shows `mrenclave: "abc123..."`,
//! not a 32-element integer array).

use serde::{Deserialize, Deserializer, Serializer};

pub mod array32 {
    use super::*;
    pub fn serialize<S: Serializer>(b: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(b))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(serde::de::Error::custom)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

pub mod array24 {
    use super::*;
    pub fn serialize<S: Serializer>(b: &[u8; 24], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(b))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 24], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(serde::de::Error::custom)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("expected 24 bytes"))
    }
}

pub mod vec {
    use super::*;
    pub fn serialize<S: Serializer>(b: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(b))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

pub mod vec_vec {
    use super::*;
    pub fn serialize<S: Serializer>(b: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        let v: Vec<String> = b.iter().map(hex::encode).collect();
        serde::Serialize::serialize(&v, s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        let v = Vec::<String>::deserialize(d)?;
        v.into_iter()
            .map(|s| hex::decode(&s).map_err(serde::de::Error::custom))
            .collect()
    }
}
