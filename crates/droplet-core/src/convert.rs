//! The MontyObject ↔ Rust conversion seam. `#[droplet_tool]`-generated thunks read arguments via
//! `FromArg` and pack return values via `IntoRet` (the cx-aware traits), so the macro never bakes in
//! type knowledge. `FromArg`/`IntoRet` bridge to the cx-free leaf conversions `FromMonty`/`IntoMonty`
//! for scalars/compounds; `Dataset` handles cross as opaque integers resolved against the session
//! handle registry (`ToolCx`), keeping rows host-side (invariant #6).

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::{Dataset, Value};
use crate::tool::ToolCx;

/// Rust value → `MontyObject` (a tool's return value crossing back into the sandbox).
pub trait IntoMonty {
    fn into_monty(self) -> MontyObject;
}

/// `MontyObject` → Rust value (a tool argument coming from sandbox code). Borrows the argument;
/// a type mismatch is a `DropletError::BadArg` (surfaces to the agent as a retryable error).
pub trait FromMonty: Sized {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError>;
}

impl IntoMonty for String {
    fn into_monty(self) -> MontyObject {
        MontyObject::String(self)
    }
}
impl IntoMonty for i64 {
    fn into_monty(self) -> MontyObject {
        MontyObject::Int(self)
    }
}
impl IntoMonty for f64 {
    fn into_monty(self) -> MontyObject {
        MontyObject::Float(self)
    }
}
impl IntoMonty for bool {
    fn into_monty(self) -> MontyObject {
        MontyObject::Bool(self)
    }
}

impl FromMonty for String {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::String(s) => Ok(s.clone()),
            other => Err(DropletError::BadArg(format!("expected str, got {other:?}"))),
        }
    }
}
impl FromMonty for i64 {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Int(n) => Ok(*n),
            other => Err(DropletError::BadArg(format!("expected int, got {other:?}"))),
        }
    }
}
impl FromMonty for f64 {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Float(f) => Ok(*f),
            other => Err(DropletError::BadArg(format!(
                "expected float, got {other:?}"
            ))),
        }
    }
}
impl FromMonty for bool {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Bool(b) => Ok(*b),
            other => Err(DropletError::BadArg(format!(
                "expected bool, got {other:?}"
            ))),
        }
    }
}

/// One capped read-out as plain typed rows (column order preserved). The agent-facing shape of a
/// tool result that returns table rows: `IntoMonty` turns it into `list[dict]` (invariant #6 keeps
/// it small — the engine cap already bounds the row count before it gets here).
pub struct Rows(pub Vec<Vec<(String, Value)>>);

impl IntoMonty for Value {
    fn into_monty(self) -> MontyObject {
        match self {
            Value::Null => MontyObject::None,
            Value::Bool(b) => MontyObject::Bool(b),
            Value::Int(i) => MontyObject::Int(i),
            Value::Float(f) => MontyObject::Float(f),
            Value::Str(s) => MontyObject::String(s),
        }
    }
}

impl IntoMonty for Rows {
    fn into_monty(self) -> MontyObject {
        let list = self
            .0
            .into_iter()
            .map(|row| {
                let pairs: Vec<(MontyObject, MontyObject)> = row
                    .into_iter()
                    .map(|(col, v)| (MontyObject::String(col), v.into_monty()))
                    .collect();
                MontyObject::Dict(pairs.into()) // Vec<(MontyObject,MontyObject)> -> DictPairs
            })
            .collect();
        MontyObject::List(list)
    }
}

/// A Python `list`/`tuple` argument as a slice of elements; anything else is a `BadArg`.
fn as_seq<'a>(o: &'a MontyObject, what: &str) -> Result<&'a [MontyObject], DropletError> {
    match o {
        MontyObject::List(v) | MontyObject::Tuple(v) => Ok(v),
        other => Err(DropletError::BadArg(format!(
            "expected {what}, got {other:?}"
        ))),
    }
}

impl FromMonty for Vec<String> {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        as_seq(o, "list[str]")?
            .iter()
            .map(String::from_monty)
            .collect()
    }
}

impl FromMonty for Vec<(String, String)> {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        as_seq(o, "list[tuple[str, str]]")?
            .iter()
            .map(|it| {
                let pair = as_seq(it, "tuple[str, str]")?;
                let [a, b] = pair else {
                    return Err(DropletError::BadArg("expected a 2-tuple".into()));
                };
                Ok((String::from_monty(a)?, String::from_monty(b)?))
            })
            .collect()
    }
}

// --- cx-aware conversions: what `#[droplet_tool]` calls --------------------------------------------
//
// `Dataset` and lists of datasets cross the boundary as opaque integer handles, so they need the
// session handle registry (`cx.handles`). Leaf/cx-free types bridge to `FromMonty`/`IntoMonty`.
// (No blanket `impl<T: FromMonty>` — that would collide with the `Dataset` impl under coherence;
// each type bridges explicitly via the macros below.)

/// `MontyObject` -> Rust tool argument, with access to the handle registry.
pub trait FromArg: Sized {
    fn from_arg(cx: &mut ToolCx, o: &MontyObject) -> Result<Self, DropletError>;
}

/// Rust tool return -> `MontyObject`, with access to the handle registry (a returned `Dataset` is
/// inserted and crosses back as its integer handle).
pub trait IntoRet {
    fn into_ret(self, cx: &mut ToolCx) -> MontyObject;
}

macro_rules! from_arg_via_from_monty {
    ($($t:ty),* $(,)?) => {$(
        impl FromArg for $t {
            fn from_arg(_cx: &mut ToolCx, o: &MontyObject) -> Result<Self, DropletError> {
                <$t as FromMonty>::from_monty(o)
            }
        }
    )*};
}
from_arg_via_from_monty!(String, i64, f64, bool, Vec<String>, Vec<(String, String)>);

macro_rules! into_ret_via_into_monty {
    ($($t:ty),* $(,)?) => {$(
        impl IntoRet for $t {
            fn into_ret(self, _cx: &mut ToolCx) -> MontyObject {
                <$t as IntoMonty>::into_monty(self)
            }
        }
    )*};
}
into_ret_via_into_monty!(String, i64, f64, bool, Rows);

impl FromArg for Dataset {
    fn from_arg(cx: &mut ToolCx, o: &MontyObject) -> Result<Self, DropletError> {
        let handle = u64::try_from(i64::from_monty(o)?)
            .map_err(|_| DropletError::BadArg("dataset handle must be non-negative".into()))?;
        Ok(cx.handles.require(handle)?.clone())
    }
}

impl IntoRet for Dataset {
    fn into_ret(self, cx: &mut ToolCx) -> MontyObject {
        // Keep the Dataset host-side (invariant #6); the sandbox gets only the opaque handle.
        MontyObject::Int(cx.handles.insert(self) as i64)
    }
}

impl FromArg for Vec<(String, Dataset)> {
    fn from_arg(cx: &mut ToolCx, o: &MontyObject) -> Result<Self, DropletError> {
        let items = as_seq(o, "list[tuple[str, Dataset]]")?;
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            let pair = as_seq(it, "tuple[str, Dataset]")?;
            let [alias, handle] = pair else {
                return Err(DropletError::BadArg("expected a 2-tuple".into()));
            };
            out.push((String::from_monty(alias)?, Dataset::from_arg(cx, handle)?));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_round_trips() {
        let o = "hi".to_string().into_monty();
        assert_eq!(o, MontyObject::String("hi".into()));
        assert_eq!(String::from_monty(&o).unwrap(), "hi");
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(42i64.into_monty(), MontyObject::Int(42));
        assert_eq!(i64::from_monty(&MontyObject::Int(42)).unwrap(), 42);
        assert_eq!(2.5f64.into_monty(), MontyObject::Float(2.5));
        assert_eq!(f64::from_monty(&MontyObject::Float(2.5)).unwrap(), 2.5);
        assert_eq!(true.into_monty(), MontyObject::Bool(true));
        assert!(bool::from_monty(&MontyObject::Bool(true)).unwrap());
    }

    #[test]
    fn wrong_type_is_bad_arg() {
        assert!(matches!(
            String::from_monty(&MontyObject::Int(1)),
            Err(DropletError::BadArg(_))
        ));
    }

    #[test]
    fn value_maps_to_monty() {
        assert_eq!(Value::Null.into_monty(), MontyObject::None);
        assert_eq!(Value::Int(7).into_monty(), MontyObject::Int(7));
        assert_eq!(
            Value::Str("x".into()).into_monty(),
            MontyObject::String("x".into())
        );
    }

    #[test]
    fn rows_become_list_of_dicts() {
        let rows = Rows(vec![vec![
            ("region".to_string(), Value::Str("EU".into())),
            ("t".to_string(), Value::Float(150.0)),
        ]]);
        let MontyObject::List(items) = rows.into_monty() else {
            panic!("Rows must convert to a List");
        };
        assert_eq!(items.len(), 1);
        let MontyObject::Dict(pairs) = &items[0] else {
            panic!("each row must be a Dict");
        };
        // DictPairs is IntoIterator over (MontyObject, MontyObject); clone to read in the test.
        let got: Vec<(MontyObject, MontyObject)> = pairs.clone().into_iter().collect();
        assert_eq!(got[0].0, MontyObject::String("region".into()));
        assert_eq!(got[0].1, MontyObject::String("EU".into()));
        assert_eq!(got[1].1, MontyObject::Float(150.0));
    }
}
