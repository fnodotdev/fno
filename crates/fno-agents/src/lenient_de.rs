//! The shared lenient-de helper set: one source feeds every crate.
//!
//! Payload parsers declare structs whose field deserializers mirror one
//! `Value::as_*` accessor each, so a mistyped field reads None instead of
//! failing the parse, and one bad field cannot blank its neighbors
//! (docs/rust-style.md R1). The helpers lived duplicated per area
//! (`runtime/lead_checkin_de.rs`, `checks/doctor/human_de.rs`); they now
//! live here once. The fno crate compiles a generated copy
//! (`crates/fno/src/lenient_de.rs`, written by `build.rs`
//! `sync_module_copy`; refreshable without a build through
//! `scripts/regen-generated-copies.py`). Payload structs stay with their
//! area; only the helper vocabulary is shared.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;

/// Parse `v` into `T`; any parse failure reads as the all-empty struct.
pub(crate) fn parse<T: DeserializeOwned + Default>(v: &Value) -> T {
    T::deserialize(v).unwrap_or_default()
}

/// Parse a JSON-object map into `T` with no clone: the rules that hold a
/// `&Map<String, Value>` (a `data` block already matched out of an envelope)
/// get the same one-lookahead lenient parse as [`parse`]. A key of the wrong
/// shape reads None, exactly as the map's own `.get()` did.
pub(crate) fn parse_map<T: DeserializeOwned + Default>(m: &serde_json::Map<String, Value>) -> T {
    T::deserialize(serde::de::value::MapDeserializer::new(
        m.iter().map(|(k, v)| (k.as_str(), v)),
    ))
    .unwrap_or_default()
}

pub(crate) fn raw<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    Value::deserialize(d)
}

// The `opt_*` deserializers mirror one `Value::as_*` accessor each: null or
// a mistyped value reads None, never an error.
pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(raw(d)?.as_str().map(str::to_string))
}

pub(crate) fn opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(raw(d)?.as_i64())
}

pub(crate) fn opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(raw(d)?.as_u64())
}

pub(crate) fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(raw(d)?.as_f64())
}

pub(crate) fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(raw(d)?.as_bool())
}

/// Present counts, even when null: mirrors `.get(key).is_some()`. Plain
/// `Option<Value>` maps null to None and cannot tell a null field from an
/// absent one; some reads (`unreadable`) key on presence alone.
pub(crate) fn opt_value_present<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Value>, D::Error> {
    Ok(Some(raw(d)?))
}

/// `.as_array()` semantics: an array reads `Some`, everything else None.
/// Elements that fail their own parse read as the all-empty struct.
pub(crate) fn opt_vec_lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<Option<Vec<T>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .map(|e| T::deserialize(e).unwrap_or_default())
            .collect()
    }))
}

/// Strings of an array, non-strings dropped: the `filter_map(Value::as_str)`
/// walk.
pub(crate) fn opt_vec_string<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect()
    }))
}

/// A nested struct field parsed leniently: null reads None, a scalar or a
/// bad object reads the all-empty struct, never a failed parse.
pub(crate) fn opt_struct<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<Option<T>, D::Error> {
    let v = raw(d)?;
    Ok(if v.is_null() {
        None
    } else {
        Some(T::deserialize(&v).unwrap_or_default())
    })
}

/// A string-keyed map read as ordered pairs: non-string values coerce to
/// "", the `as_str().unwrap_or("")` walk. Pairs, not a map, because the
/// reader iterates the trust table in insertion order.
pub(crate) fn opt_map_string<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<(String, String)>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_object().map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Default)]
    struct Row {
        #[serde(default, deserialize_with = "opt_string")]
        name: Option<String>,
        #[serde(default, deserialize_with = "opt_u64")]
        count: Option<u64>,
        #[serde(default, deserialize_with = "opt_value_present")]
        unreadable: Option<Value>,
    }

    #[test]
    fn a_mistyped_field_reads_none_and_its_neighbors_survive() {
        let row: Row = parse(&serde_json::json!({"name": 7, "count": "3"}));
        assert_eq!(row.name, None);
        assert_eq!(row.count, None);
    }

    #[test]
    fn a_present_null_field_counts_as_present() {
        let row: Row = parse(&serde_json::json!({"unreadable": null}));
        assert!(row.unreadable.is_some());
        let absent: Row = parse(&serde_json::json!({}));
        assert!(absent.unreadable.is_none());
    }

    #[test]
    fn a_non_object_parses_as_the_all_empty_struct() {
        let row: Row = parse(&serde_json::json!("scalar"));
        assert_eq!(row.name, None);
    }
}
