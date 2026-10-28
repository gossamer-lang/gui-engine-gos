use zbus::zvariant::{Array, Dict, ObjectPath, Signature, Structure, StructureBuilder, Value};

/// One D-Bus value as the AT-SPI protocol builds and reads it.
///
/// Container signatures name the element (`Items`) or carried (`Variant`)
/// type; values decoded from the bus leave them empty.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Wire {
    Text(String),
    Path(String),
    Byte(i64),
    Short(i64),
    Signed(i64),
    Unsigned(i64),
    Real(f64),
    Flag(bool),
    Fields(Vec<Wire>),
    Entry(Vec<Wire>),
    Items(&'static str, Vec<Wire>),
    Variant(&'static str, Vec<Wire>),
}

/// Why a `Wire` value cannot go on the bus.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("{0} is out of range for its D-Bus type")]
    OutOfRange(i64),
    #[error("a {0} needs exactly {1} values")]
    Arity(&'static str, usize),
    #[error("an empty struct has no D-Bus encoding")]
    EmptyFields,
    #[error("a dictionary holds only dictionary entries")]
    NotAnEntry,
    #[error(transparent)]
    Variant(#[from] zbus::zvariant::Error),
    #[error("invalid D-Bus signature: {0:?}")]
    Signature(zbus::zvariant::signature::Error),
}

impl Wire {
    /// The bus encoding of this value.
    pub fn to_value(&self) -> Result<Value<'static>, WireError> {
        Ok(match self {
            Wire::Text(s) => Value::from(s.clone()),
            Wire::Path(p) => Value::from(ObjectPath::try_from(p.clone())?),
            Wire::Byte(n) => Value::U8(u8::try_from(*n).map_err(|_| WireError::OutOfRange(*n))?),
            Wire::Short(n) => Value::I16(i16::try_from(*n).map_err(|_| WireError::OutOfRange(*n))?),
            // AT-SPI's 32-bit fields carry their low bits, as libdbus stores them.
            Wire::Signed(n) => Value::I32(*n as i32),
            Wire::Unsigned(n) => Value::U32(*n as u32),
            Wire::Real(v) => Value::F64(*v),
            Wire::Flag(v) => Value::Bool(*v),
            Wire::Fields(values) | Wire::Entry(values) => {
                if values.is_empty() {
                    return Err(WireError::EmptyFields);
                }
                values
                    .iter()
                    .try_fold(StructureBuilder::new(), |b, v| {
                        Ok::<_, WireError>(b.append_field(v.to_value()?))
                    })?
                    .build()?
                    .into()
            }
            Wire::Items(element, values) => items(element, values)?,
            Wire::Variant(_, values) => match values.as_slice() {
                [carried] => Value::Value(Box::new(carried.to_value()?)),
                _ => return Err(WireError::Arity("variant", 1)),
            },
        })
    }

    /// The `Wire` reading of a value decoded from the bus.
    pub fn from_value(value: &Value<'_>) -> Wire {
        match value {
            Value::Str(s) => Wire::Text(s.to_string()),
            Value::ObjectPath(p) => Wire::Path(p.to_string()),
            Value::Signature(s) => Wire::Text(s.to_string()),
            Value::U8(n) => Wire::Byte(i64::from(*n)),
            Value::I16(n) => Wire::Short(i64::from(*n)),
            Value::U16(n) => Wire::Unsigned(i64::from(*n)),
            Value::I32(n) => Wire::Signed(i64::from(*n)),
            Value::U32(n) => Wire::Unsigned(i64::from(*n)),
            Value::I64(n) => Wire::Signed(*n),
            Value::U64(n) => Wire::Unsigned(*n as i64),
            Value::F64(v) => Wire::Real(*v),
            Value::Bool(v) => Wire::Flag(*v),
            Value::Value(inner) => Wire::Variant("", vec![Wire::from_value(inner)]),
            Value::Array(a) => Wire::Items("", a.iter().map(Wire::from_value).collect()),
            Value::Dict(d) => Wire::Items(
                "",
                d.iter()
                    .map(|(k, v)| Wire::Fields(vec![Wire::from_value(k), Wire::from_value(v)]))
                    .collect(),
            ),
            Value::Structure(s) => Wire::Fields(s.fields().iter().map(Wire::from_value).collect()),
            #[cfg(unix)]
            Value::Fd(_) => Wire::Fields(vec![]),
        }
    }
}

// A dictionary-entry element type makes the array a D-Bus dictionary.
fn items(element: &str, values: &[Wire]) -> Result<Value<'static>, WireError> {
    if let Signature::Dict { key, value } =
        Signature::try_from(format!("a{element}").as_str()).map_err(WireError::Signature)?
    {
        let mut dict = Dict::new(&key, &value);
        for entry in values {
            let Wire::Entry(pair) = entry else {
                return Err(WireError::NotAnEntry);
            };
            let [k, v] = pair.as_slice() else {
                return Err(WireError::Arity("dictionary entry", 2));
            };
            dict.append(k.to_value()?, v.to_value()?)?;
        }
        return Ok(Value::Dict(dict));
    }
    let signature = Signature::try_from(element).map_err(WireError::Signature)?;
    let mut array = Array::new(&signature);
    for v in values {
        array.append(v.to_value()?)?;
    }
    Ok(Value::Array(array))
}

/// The arguments a message body carries.
pub fn arguments(body: &zbus::message::Body) -> zbus::Result<Vec<Wire>> {
    if body.is_empty() {
        return Ok(vec![]);
    }
    let fields: Structure<'_> = body.deserialize()?;
    Ok(fields.fields().iter().map(Wire::from_value).collect())
}

/// A message body of `values`, one argument each.
pub fn body(values: &[Wire]) -> Result<Structure<'static>, WireError> {
    let builder = values.iter().try_fold(StructureBuilder::new(), |b, v| {
        Ok::<_, WireError>(b.append_field(v.to_value()?))
    })?;
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_variant_dictionary_encodes_as_a_dict() {
        let w = Wire::Items(
            "{sv}",
            vec![Wire::Entry(vec![
                Wire::Text("k".into()),
                Wire::Variant("i", vec![Wire::Signed(3)]),
            ])],
        );
        let v = w.to_value().unwrap();
        assert_eq!(v.value_signature().to_string(), "a{sv}");
    }

    #[test]
    fn an_empty_array_keeps_its_element_type() {
        let v = Wire::Items("(so)", vec![]).to_value().unwrap();
        assert_eq!(v.value_signature().to_string(), "a(so)");
    }

    #[test]
    fn a_reference_round_trips_through_the_bus_encoding() {
        let w = Wire::Fields(vec![
            Wire::Text(":1.5".into()),
            Wire::Path("/org/a11y/atspi/null".into()),
        ]);
        assert_eq!(Wire::from_value(&w.to_value().unwrap()), w);
    }
}
