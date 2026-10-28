use std::fmt;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::OnceLock;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Runs host closure `id` on JSON arguments, answering its JSON result.
pub type HostCall = fn(id: i64, args: &str) -> Result<String, String>;

static HOST: OnceLock<HostCall> = OnceLock::new();

/// Installs the function that runs closures an embedding host registered.
/// The first installation wins; a host installs once at startup.
pub fn set_host(call: HostCall) {
    let _ = HOST.set(call);
}

/// A closure a widget description carries.
///
/// Copies of a description share the closure, and two callbacks are equal
/// only when they share one closure: a description rebuilt from the same
/// code holds new closures and so differs. A callback the host registered
/// keeps its id, which is how it crosses a serialized description.
pub struct Callback<F: ?Sized> {
    f: Rc<F>,
    id: Option<i64>,
}

impl<F: ?Sized> Callback<F> {
    /// A callback running `f` in this process.
    pub fn new(f: Rc<F>) -> Self {
        Callback { f, id: None }
    }

    /// The host's id for this callback, when the host registered it.
    pub fn host_id(&self) -> Option<i64> {
        self.id
    }
}

impl<F: ?Sized> Clone for Callback<F> {
    fn clone(&self) -> Self {
        Callback {
            f: Rc::clone(&self.f),
            id: self.id,
        }
    }
}

impl<F: ?Sized> Deref for Callback<F> {
    type Target = F;

    fn deref(&self) -> &F {
        &self.f
    }
}

impl<F: ?Sized> PartialEq for Callback<F> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.f, &other.f)
    }
}

impl<F: ?Sized> fmt::Debug for Callback<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.id {
            Some(id) => write!(f, "Callback({id})"),
            None => f.write_str("Callback"),
        }
    }
}

impl<F: ?Sized> Serialize for Callback<F> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.id {
            Some(id) => s.serialize_i64(id),
            None if crate::memory::measuring() => s.serialize_i64(-1),
            None => Err(serde::ser::Error::custom(
                "a toolkit closure has no host id",
            )),
        }
    }
}

// Runs host closure `id`: one argument travels as itself, several as an
// array. A failing host closure is a failure of the application's own code,
// so it ends the call the way a panic in that code would.
fn call_host<R: DeserializeOwned>(id: i64, args: String) -> R {
    let Some(call) = HOST.get() else {
        panic!("callback {id} runs with no host installed");
    };
    match call(id, &args).and_then(|out| serde_json::from_str(&out).map_err(|e| e.to_string())) {
        Ok(r) => r,
        Err(e) => panic!("callback {id} failed: {e}"),
    }
}

fn encode(args: &impl Serialize) -> String {
    // Callback arguments are toolkit values with string keys, which encode.
    serde_json::to_string(args).expect("callback arguments encode as JSON")
}

macro_rules! hosted {
    ($($a:ident $v:ident),*) => {
        impl<'de, $($a,)* R> Deserialize<'de> for Callback<dyn Fn($($a),*) -> R>
        where
            $($a: Serialize + 'static,)*
            R: DeserializeOwned + 'static,
        {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let id = i64::deserialize(d)?;
                if HOST.get().is_none() {
                    return Err(D::Error::custom("a hosted callback needs a host"));
                }
                let f: Rc<dyn Fn($($a),*) -> R> = Rc::new(move |$($v: $a),*| {
                    call_host(id, hosted!(@args $($v),*))
                });
                Ok(Callback { f, id: Some(id) })
            }
        }
    };
    (@args $v:ident) => { encode(&$v) };
    (@args $($v:ident),*) => { encode(&($($v,)*)) };
}

hosted!(A a);
hosted!(A a, B b);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_equals_its_copies_only() {
        let a: Callback<dyn Fn(i64) -> i64> = Callback::new(Rc::new(|x| x + 1));
        let b: Callback<dyn Fn(i64) -> i64> = Callback::new(Rc::new(|x| x + 1));
        assert_eq!(a, a.clone());
        assert_ne!(a, b);
        assert_eq!(a(1), 2);
    }

    #[test]
    fn a_hosted_callback_round_trips_its_id_and_calls_the_host() {
        set_host(|id, args| {
            Ok(format!(
                "{}",
                id * 100 + args.parse::<i64>().map_err(|e| e.to_string())?
            ))
        });
        let c: Callback<dyn Fn(i64) -> i64> = serde_json::from_str("7").unwrap();
        assert_eq!(c(5), 705);
        assert_eq!(serde_json::to_string(&c).unwrap(), "7");
    }
}
