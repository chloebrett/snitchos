//! Bridge between the interpreter's runtime [`Value`] and the `hitch` value
//! model. It lives here, not in `hitch`: `hitch` is a leaf that knows nothing
//! about Stitch, and `stitch` owns `Value`, so the conversions are ours to
//! define. The orphan rule forbids implementing `From`/`TryFrom` *on*
//! `hitch::Value` from this crate, so the conversions are free functions.

use alloc::collections::{BTreeMap, BTreeSet};

use crate::ast::{Field, Item, Type};
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
            native: false,
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
                native: false,
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

/// The `prod`/`sum` type declarations a program defines, indexed by name — what
/// the type bridge resolves a named user type against. Parse-on-demand: built from
/// the program's own `Item`s, not the eval-time registry.
pub struct TypeDefs<'a> {
    by_name: BTreeMap<&'a str, &'a Item>,
}

impl<'a> TypeDefs<'a> {
    /// Index the `prod`/`sum` declarations among `items`.
    #[must_use]
    pub fn from_items(items: &'a [Item]) -> Self {
        let by_name = items
            .iter()
            .filter_map(|item| match item {
                Item::Prod { name, .. } | Item::Sum { name, .. } => Some((name.as_str(), item)),
                _ => None,
            })
            .collect();
        Self { by_name }
    }
}

/// Bridge a Stitch type annotation into a [`hitch::TypeSchema`] — the type-level
/// twin of [`to_hitch`]. A typed-process manifest's `in`/`out` schemas come from
/// this (a stage's `main(x: T) -> U`). Maps scalars, `List<T>`, and the program's
/// own monomorphic `prod`/`sum` types (resolved through `defs`). **Not
/// marshallable** (an `Err`, never a silent wrong shape): `Func`, the self-type
/// `@`, tuples, generic types, recursive types, and unknown names.
pub fn type_to_schema(ty: &Type, defs: &TypeDefs) -> Result<hitch::TypeSchema, RuntimeError> {
    schema_of(ty, defs, &mut BTreeSet::new())
}

fn schema_of(
    ty: &Type,
    defs: &TypeDefs,
    visiting: &mut BTreeSet<String>,
) -> Result<hitch::TypeSchema, RuntimeError> {
    use hitch::TypeSchema;
    match ty {
        Type::Name { name, args } => match (name.as_str(), args.as_slice()) {
            ("Int", []) => Ok(TypeSchema::I64),
            ("Float", []) => Ok(TypeSchema::F64),
            ("Bool", []) => Ok(TypeSchema::Bool),
            ("Str", []) => Ok(TypeSchema::Str),
            ("List", [elem]) => Ok(TypeSchema::Seq(Box::new(schema_of(elem, defs, visiting)?))),
            (user, []) => user_schema(user, defs, visiting),
            _ => Err(RuntimeError::new(format!("type `{name}` is not marshallable"))),
        },
        Type::Func { .. } => {
            Err(RuntimeError::new("a function type cannot cross a process boundary"))
        }
        Type::Tuple(_) => Err(RuntimeError::new("a tuple type is not yet marshallable")),
        Type::SelfType => Err(RuntimeError::new("the self-type `@` cannot be marshalled")),
    }
}

/// Resolve a named user type: a `prod` → a [`hitch::TypeSchema::Product`], a `sum`
/// → a `Sum` (each variant a `Product` of its fields). Generic types are rejected
/// (v1 is monomorphic); the `visiting` set breaks cycles so a recursive type
/// errors rather than looping forever.
fn user_schema(
    name: &str,
    defs: &TypeDefs,
    visiting: &mut BTreeSet<String>,
) -> Result<hitch::TypeSchema, RuntimeError> {
    use hitch::TypeSchema;
    let item = defs
        .by_name
        .get(name)
        .copied()
        .ok_or_else(|| RuntimeError::new(format!("type `{name}` is not marshallable")))?;
    if !visiting.insert(name.into()) {
        return Err(RuntimeError::new(format!(
            "recursive type `{name}` is not marshallable"
        )));
    }
    let schema = match item {
        Item::Prod { name: tn, generics, fields, .. } if generics.is_empty() => {
            Ok(TypeSchema::Product {
                type_name: tn.clone(),
                fields: fields_schema(fields, defs, visiting)?,
            })
        }
        Item::Sum { name: tn, generics, variants, .. } if generics.is_empty() => {
            let variants = variants
                .iter()
                .map(|variant| {
                    Ok((
                        variant.name.clone(),
                        TypeSchema::Product {
                            type_name: tn.clone(),
                            fields: fields_schema(&variant.fields, defs, visiting)?,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, RuntimeError>>()?;
            Ok(TypeSchema::Sum { type_name: tn.clone(), variants })
        }
        _ => Err(RuntimeError::new(format!(
            "generic type `{name}` is not marshallable"
        ))),
    };
    visiting.remove(name);
    schema
}

fn fields_schema(
    fields: &[Field],
    defs: &TypeDefs,
    visiting: &mut BTreeSet<String>,
) -> Result<Vec<(Option<String>, hitch::TypeSchema)>, RuntimeError> {
    fields
        .iter()
        .map(|field| Ok((field.name.clone(), schema_of(&field.ty, defs, visiting)?)))
        .collect()
}

/// Derive a [`hitch::Manifest`] from a program's `main` — its typed-process
/// interface `(in, out, uses)`. A stage's `main` is `main(x: T) -> U uses C`: the
/// single parameter is the input (`None` for a zero-param **source**), the return
/// type is the output, and `uses` the declared capabilities. This is the
/// **parse-on-demand** producer: the shell runs it on the `.st` it's about to
/// spawn, so no stored manifest is needed (phase 1 of the typed-processes plan).
///
/// # Errors
/// No `main`; more than one input parameter; an untyped input; a missing return
/// type; or an input/output type that is not marshallable ([`type_to_schema`]).
pub fn manifest_of_main(items: &[Item]) -> Result<hitch::Manifest, RuntimeError> {
    let (params, ret, uses) = items
        .iter()
        .find_map(|item| match item {
            Item::Func { name, params, ret, uses, .. } if name == "main" => {
                Some((params, ret, uses))
            }
            _ => None,
        })
        .ok_or_else(|| RuntimeError::new("no `main` function to derive a manifest from"))?;

    // Resolve `main`'s types against the program's own `prod`/`sum` declarations.
    let defs = TypeDefs::from_items(items);

    let input = match params.as_slice() {
        [] => None,
        [param] => {
            let ty = param
                .ty
                .as_ref()
                .ok_or_else(|| RuntimeError::new("a stage `main`'s input parameter must be typed"))?;
            Some(type_to_schema(ty, &defs)?)
        }
        _ => {
            return Err(RuntimeError::new(
                "a stage `main` takes at most one input (the upstream value)",
            ));
        }
    };

    let ret = ret
        .as_ref()
        .ok_or_else(|| RuntimeError::new("a stage `main` must declare its return type"))?;
    let output = type_to_schema(ret, &defs)?;

    // Stitch effect names (`uses C`) become **name-only** authority slots: the
    // effect → typed-capability (object/rights) mapping is deferred to the
    // manifest-v2 vocabulary (Q5), so `object`/`rights` are `0` placeholders and the
    // interface carries just the declared role names. Language-side authority is
    // still enforced via `method.uses` / `with_authority`.
    let needs = uses
        .iter()
        .map(|effect| hitch::Slot { name: effect.name.clone(), object: 0, rights: 0 })
        .collect();
    // A Stitch stage always has an output (a `main` return type is required); the
    // manifest models `output` as optional only because non-stage programs omit it.
    Ok(hitch::Manifest { input, output: Some(output), needs })
}

#[cfg(test)]
mod tests {
    use super::{from_hitch, to_hitch, type_to_schema, TypeDefs};
    use crate::ast::Type;
    use crate::prelude::*;
    use crate::value::{DataValue, Value};

    fn named(name: &str, args: Vec<Type>) -> Type {
        Type::Name { name: name.to_string(), args }
    }

    /// Bridge `ty` with no user types in scope (scalars / `List` / error cases).
    fn schema(ty: &Type) -> Result<hitch::TypeSchema, crate::value::RuntimeError> {
        type_to_schema(ty, &TypeDefs::from_items(&[]))
    }

    /// Bridge `ty` against the `prod`/`sum` declarations in `src`.
    fn schema_in(src: &str, ty: &Type) -> Result<hitch::TypeSchema, crate::value::RuntimeError> {
        let items = crate::parser::parse_program(src).expect("test program parses");
        type_to_schema(ty, &TypeDefs::from_items(&items))
    }

    #[test]
    fn scalar_types_bridge_to_their_hitch_shape() {
        assert_eq!(schema(&named("Int", vec![])).unwrap(), hitch::TypeSchema::I64);
        assert_eq!(schema(&named("Float", vec![])).unwrap(), hitch::TypeSchema::F64);
        assert_eq!(schema(&named("Bool", vec![])).unwrap(), hitch::TypeSchema::Bool);
        assert_eq!(schema(&named("Str", vec![])).unwrap(), hitch::TypeSchema::Str);
    }

    #[test]
    fn a_list_type_bridges_to_a_seq_of_the_element_shape() {
        let list = named("List", vec![named("Int", vec![])]);
        assert_eq!(
            schema(&list).unwrap(),
            hitch::TypeSchema::Seq(Box::new(hitch::TypeSchema::I64))
        );
    }

    #[test]
    fn a_function_type_is_not_marshallable() {
        let int = named("Int", vec![]);
        let f = Type::Func { param: Box::new(int.clone()), ret: Box::new(int) };
        assert!(schema(&f).is_err());
    }

    #[test]
    fn an_unknown_type_name_is_not_marshallable_yet() {
        assert!(schema(&named("Widget", vec![])).is_err());
    }

    #[test]
    fn a_prod_type_resolves_to_a_product_of_its_fields() {
        let s = schema_in("prod Point(x: Int, y: Int)", &named("Point", vec![])).expect("schema");
        assert_eq!(
            s,
            hitch::TypeSchema::Product {
                type_name: "Point".into(),
                fields: vec![
                    (Some("x".into()), hitch::TypeSchema::I64),
                    (Some("y".into()), hitch::TypeSchema::I64),
                ],
            }
        );
    }

    #[test]
    fn a_sum_type_resolves_to_a_sum_of_variant_products() {
        let s = schema_in("sum Shape = Circle(r: Int) | Empty", &named("Shape", vec![]))
            .expect("schema");
        assert_eq!(
            s,
            hitch::TypeSchema::Sum {
                type_name: "Shape".into(),
                variants: vec![
                    (
                        "Circle".into(),
                        hitch::TypeSchema::Product {
                            type_name: "Shape".into(),
                            fields: vec![(Some("r".into()), hitch::TypeSchema::I64)],
                        },
                    ),
                    (
                        "Empty".into(),
                        hitch::TypeSchema::Product { type_name: "Shape".into(), fields: vec![] },
                    ),
                ],
            }
        );
    }

    #[test]
    fn a_recursive_type_is_not_marshallable() {
        assert!(schema_in("prod Node(next: Node)", &named("Node", vec![])).is_err());
    }

    #[test]
    fn a_generic_prod_is_not_marshallable() {
        // A *concrete* field, so the only reason to reject is the generic parameter
        // (a generic field like `value: T` would fail to resolve anyway, not
        // isolating the generics check).
        assert!(schema_in("prod Box<T>(value: Int)", &named("Box", vec![])).is_err());
    }

    #[test]
    fn a_generic_sum_is_not_marshallable() {
        assert!(schema_in("sum Opt<T> = Has(v: Int) | Nope", &named("Opt", vec![])).is_err());
    }

    fn manifest(src: &str) -> Result<hitch::Manifest, crate::value::RuntimeError> {
        let items = crate::parser::parse_program(src).expect("test program parses");
        super::manifest_of_main(&items)
    }

    #[test]
    fn manifest_of_main_reads_input_output_and_needs() {
        let m = manifest(r"main(x: Int) -> List<Str> uses FsRead, ConsoleOut = []").expect("manifest");
        assert_eq!(m.input, Some(hitch::TypeSchema::I64));
        assert_eq!(m.output, Some(hitch::TypeSchema::Seq(Box::new(hitch::TypeSchema::Str))));
        // Effect names become name-only slots (object/rights unresolved → 0).
        assert_eq!(
            m.needs,
            vec![
                hitch::Slot { name: "FsRead".into(), object: 0, rights: 0 },
                hitch::Slot { name: "ConsoleOut".into(), object: 0, rights: 0 },
            ],
        );
    }

    #[test]
    fn a_zero_param_main_is_a_source_with_no_input() {
        let m = manifest(r"main() -> Int = 0").expect("manifest");
        assert_eq!(m.input, None);
        assert_eq!(m.output, Some(hitch::TypeSchema::I64));
        assert!(m.needs.is_empty());
    }

    #[test]
    fn a_stage_main_must_declare_a_return_type() {
        assert!(manifest(r"main(x: Int) = x").is_err());
    }

    #[test]
    fn a_typeless_input_param_is_not_a_stage() {
        assert!(manifest(r"main(x) -> Int = 0").is_err());
    }

    #[test]
    fn a_program_without_main_has_no_manifest() {
        assert!(manifest(r"helper() = 1").is_err());
    }

    #[test]
    fn the_manifest_is_mains_signature_not_another_functions() {
        // `helper` comes first and would yield a *different* manifest; the extractor
        // must pick `main`, not merely the first function.
        let m = manifest(r#"helper(x: Bool) -> Bool = x  main(x: Int) -> Str = """#)
            .expect("manifest");
        assert_eq!(m.input, Some(hitch::TypeSchema::I64));
        assert_eq!(m.output, Some(hitch::TypeSchema::Str));
    }

    #[test]
    fn the_manifest_resolves_a_user_type_declared_in_the_program() {
        // `main`'s input is a `prod` the same program defines — the extractor must
        // resolve it through the program's own declarations.
        let m = manifest("prod Pt(x: Int)  main(p: Pt) -> Int = 0").expect("manifest");
        assert_eq!(
            m.input,
            Some(hitch::TypeSchema::Product {
                type_name: "Pt".into(),
                fields: vec![(Some("x".into()), hitch::TypeSchema::I64)],
            })
        );
    }

    fn data(type_name: &str, variant: &str, fields: Vec<(Option<String>, Value)>) -> Value {
        Value::Data(Rc::new(DataValue {
            type_name: type_name.to_string(),
            variant: variant.to_string(),
            fields,
            native: false,
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
