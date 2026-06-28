//! Bridge between the interpreter's runtime [`Value`] and the `hitch` value
//! model. It lives here, not in `hitch`: `hitch` is a leaf that knows nothing
//! about Stitch, and `stitch` owns `Value`, so the conversions are ours to
//! define. The orphan rule forbids implementing `From`/`TryFrom` *on*
//! `hitch::Value` from this crate, so the conversions are free functions.

use crate::prelude::*;
use crate::value::{DataValue, RuntimeError, Value};

/// Convert a Stitch runtime value into a [`hitch::Value`]. Fails for values that
/// aren't plain data — closures, constructors, modules, native functions, lazy
/// sequences, and (not yet modelled) maps, tuples, and unit.
///
/// A `Data` maps by its kind: a **prod** (`variant == type_name`) becomes a
/// [`hitch::Value::Product`], matching the equivalent Rust struct; a **sum**
/// variant becomes a [`hitch::Value::Sum`] whose payload is the `Product` of the
/// variant's fields.
pub fn to_hitch(value: &Value) -> Result<hitch::Value, RuntimeError> {
    match value {
        Value::Int(n) => Ok(hitch::Value::I64(*n)),
        Value::Float(n) => Ok(hitch::Value::F64(*n)),
        Value::Bool(b) => Ok(hitch::Value::Bool(*b)),
        Value::Str(s) => Ok(hitch::Value::Str(s.to_string())),
        Value::List(items) => {
            let mapped = items.iter().map(to_hitch).collect::<Result<Vec<_>, _>>()?;
            Ok(hitch::Value::Seq(mapped))
        }
        Value::Data(data) => data_to_hitch(data),
        other => Err(RuntimeError::new(format!("cannot hitch a {}", other.kind()))),
    }
}

fn data_to_hitch(data: &DataValue) -> Result<hitch::Value, RuntimeError> {
    let fields = data
        .fields
        .iter()
        .map(|(name, value)| Ok((name.clone(), to_hitch(value)?)))
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    let product = hitch::Value::Product {
        type_name: data.type_name.clone(),
        fields,
    };
    if data.variant == data.type_name {
        Ok(product)
    } else {
        Ok(hitch::Value::Sum {
            type_name: data.type_name.clone(),
            variant: data.variant.clone(),
            payload: Box::new(product),
        })
    }
}

/// Convert a [`hitch::Value`] back into a Stitch runtime value. The inverse of
/// [`to_hitch`] over the data subset: a `Product` becomes a prod `Data`, a `Sum`
/// becomes a sum-variant `Data`. Fails on shapes Stitch can't represent (`Bytes`,
/// or a `U64` outside `i64`).
pub fn from_hitch(value: &hitch::Value) -> Result<Value, RuntimeError> {
    match value {
        hitch::Value::Bool(b) => Ok(Value::Bool(*b)),
        hitch::Value::I64(n) => Ok(Value::Int(*n)),
        hitch::Value::U64(n) => {
            let n = i64::try_from(*n)
                .map_err(|_| RuntimeError::new("hitch U64 does not fit a Stitch Int"))?;
            Ok(Value::Int(n))
        }
        hitch::Value::F64(n) => Ok(Value::Float(*n)),
        hitch::Value::Str(s) => Ok(Value::Str(Rc::from(s.as_str()))),
        hitch::Value::Bytes(_) => Err(RuntimeError::new("Stitch has no bytes value")),
        hitch::Value::Seq(items) => {
            let mapped = items.iter().map(from_hitch).collect::<Result<Vec<_>, _>>()?;
            Ok(Value::List(Rc::from(mapped)))
        }
        hitch::Value::Product { type_name, fields } => Ok(Value::Data(Rc::new(DataValue {
            type_name: type_name.clone(),
            // A prod's variant is its type name (registry mirrors this).
            variant: type_name.clone(),
            fields: from_hitch_fields(fields)?,
        }))),
        hitch::Value::Sum {
            type_name,
            variant,
            payload,
        } => {
            let hitch::Value::Product { fields, .. } = payload.as_ref() else {
                return Err(RuntimeError::new("hitch Sum payload was not a Product"));
            };
            Ok(Value::Data(Rc::new(DataValue {
                type_name: type_name.clone(),
                variant: variant.clone(),
                fields: from_hitch_fields(fields)?,
            })))
        }
    }
}

fn from_hitch_fields(
    fields: &[(Option<String>, hitch::Value)],
) -> Result<Vec<(Option<String>, Value)>, RuntimeError> {
    fields
        .iter()
        .map(|(name, value)| Ok((name.clone(), from_hitch(value)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{from_hitch, to_hitch};
    use crate::prelude::*;
    use crate::value::{DataValue, Value};

    fn data(type_name: &str, variant: &str, fields: Vec<(Option<String>, Value)>) -> Value {
        Value::Data(Rc::new(DataValue {
            type_name: type_name.to_string(),
            variant: variant.to_string(),
            fields,
        }))
    }

    #[test]
    fn top_level_scalars_round_trip() {
        for value in [
            Value::Int(-3),
            Value::Float(1.5),
            Value::Bool(true),
            Value::Str(Rc::from("hot.avg")),
        ] {
            let back = from_hitch(&to_hitch(&value).expect("to hitch")).expect("from hitch");
            assert_eq!(back, value);
        }
    }

    #[test]
    fn a_prod_record_round_trips_and_maps_to_a_product() {
        // A prod has `variant == type_name`; it must become a Hitch Product (so
        // it matches the equivalent Rust struct) and survive the round trip.
        let original = data(
            "Point",
            "Point",
            vec![
                (Some("x".to_string()), Value::Int(1)),
                (
                    Some("tags".to_string()),
                    Value::List(Rc::from([Value::Str(Rc::from("a")), Value::Bool(false)])),
                ),
            ],
        );
        let h = to_hitch(&original).expect("to hitch");
        assert!(matches!(h, hitch::Value::Product { .. }), "a prod is a Product");
        assert_eq!(from_hitch(&h).expect("from hitch"), original);
    }

    #[test]
    fn a_sum_variant_round_trips_and_maps_to_a_sum() {
        // A sum variant has `variant != type_name`; it must become a Hitch Sum.
        let original = data("Status", "Ok", vec![(None, Value::Int(0))]);
        let h = to_hitch(&original).expect("to hitch");
        assert!(matches!(h, hitch::Value::Sum { .. }), "a sum variant is a Sum");
        assert_eq!(from_hitch(&h).expect("from hitch"), original);
    }

    #[test]
    fn a_map_cannot_be_hitched() {
        // Maps aren't in the v1 model; converting one must error, not silently
        // drop data.
        let map = Value::Map(Rc::new(Vec::new()));
        assert!(to_hitch(&map).is_err());
    }
}
