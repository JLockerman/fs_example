use std::slice;

use pg_sys::Datum;
use pgx::*;

use flat_serialize::*;

use aggregate_utils::in_aggregate_context;
use palloc::Internal;

mod aggregate_utils;
mod palloc;

pg_module_magic!();

// an example of using flat-serialize to create a simple array type,
// represented as
// ```
// varlen header | data len | data len f64s
// ```

// flat_serialize is used to define the data layout on disk
flat_serialize_macro::flat_serialize! {
    struct SimpleArrayData {
        header: u32,
        len: u32,
        data: [f64; self.len],
    }
}

// this creates a struct like
// ```
// struct SimpleArrayData<'a> {
//     header: &'a u32,
//     data: &'a [f64],
// }
// ```
// which can be used to wrap the data

// Right now we need to define a wrapper type because #[derive(...)] isn't
// usable on flat_serialize!(...) types directly. We derive PostgresType,
// Copy, and Clone but _not_ Serialize and Deserialize. Because we don't have
// Serialize and Deserialize we add #[inoutfuncs] to tell pgx that we'll be
// adding our own inout functions.
#[derive(PostgresType, Copy, Clone)]
#[inoutfuncs]
pub struct SimpleArray<'input>(SimpleArrayData<'input>);

// here we define our in/out functions
impl<'input> InOutFuncs for SimpleArray<'input> {
    fn output(&self, buffer: &mut StringInfo) {
        use std::io::Write;
        // for output we'll just write the debug format of the data
        // if we decide to go this route we'll probably automate this process
        let _ = write!(buffer, "{:?}", self.0.data);
    }

    fn input(_input: &std::ffi::CStr) -> Self
    where
        Self: Sized,
    {
        unimplemented!("we don't bother implementing string input")
    }
}

// shim code to convert from a datum into something rust understands, all
// automatable
impl<'input> FromDatum for SimpleArray<'input> {
    unsafe fn from_datum(datum: Datum, is_null: bool, _: pg_sys::Oid) -> Option<Self>
    where
        Self: Sized,
    {
        if is_null {
            return None;
        }

        let ptr = pg_sys::pg_detoast_datum_packed(datum as *mut pg_sys::varlena);
        let data_len = varsize_any(ptr);
        let bytes = slice::from_raw_parts(ptr as *mut u8, data_len);

        let (data, _) = match SimpleArrayData::try_ref(bytes) {
            Ok(wrapped) => wrapped,
            Err(e) => error!("invalid SimpleArray {:?}", e),
        };

        SimpleArray(data).into()
    }
}

impl<'input> IntoDatum for SimpleArray<'input> {
    fn into_datum(self) -> Option<Datum> {
        // to convert to a datum just get a pointer to the start of the buffer
        // _technically_ this is only safe if we're sure that the data is laid
        // out contiguously, which we have no way to guarantee except by
        // allocation a new buffer, or storing some additional metadata.
        Some(self.0.header as *const u32 as Datum)
    }

    fn type_oid() -> pg_sys::Oid {
        rust_regtypein::<Self>()
    }
}

// a basic aggregate to construct a SimpleArray

// the trans function just pushes onto a vector
#[pg_extern]
fn simple_array_trans(
    state: Option<Internal<Vec<f64>>>,
    value: f64,
    fcinfo: pg_sys::FunctionCallInfo,
) -> Option<Internal<Vec<f64>>> {
    unsafe {
        in_aggregate_context(fcinfo, || {
            let mut state = state.unwrap_or_else(|| vec![].into());

            state.push(value);

            Some(state)
        })
    }
}

// ignore this code for now, it'll probably be library code
macro_rules! flatten {
    ($typ:ident { $($field:ident: $value:expr),* $(,)? }) => {
        {
            let data = $typ {
                $(
                    $field: $value
                ),*
            };
            let mut output = vec![];
            data.fill_vec(&mut output);

            set_varsize(output.as_mut_ptr() as *mut _, output.len() as i32);

            $typ::try_ref(output.leak()).unwrap().0
        }
    }
}

// the final function flattens the vector into something that can be stored on
// disk
#[pg_extern]
fn simple_array_final(
    state: Option<Internal<Vec<f64>>>,
    fcinfo: pg_sys::FunctionCallInfo,
) -> Option<SimpleArray<'static>> {
    unsafe {
        in_aggregate_context(fcinfo, || {
            let state = match state {
                None => return None,
                Some(state) => state,
            };
            // we need to flatten the vector to a single buffer that contains
            // both the size, the data, and the varlen header
            let flattened = flatten! {
                SimpleArrayData{
                    header: &0,
                    data: &state,
                    // note the lack of length; because it is exactly the
                    // length of a slice it will be computed from that
                }
            };

            SimpleArray(flattened).into()
        })
    }
}

// finally an index function to get a value out of a simple array
#[pg_extern]
fn index<'input>(state: SimpleArray<'input>, index: u32) -> Option<f64> {
    state.0.data.get(index as usize).cloned()
}

#[cfg(feature = "pg_test")]
mod tests {
    use pgx::*;

    #[pg_test]
    fn test_aggregate() {
        Spi::execute(|client| {
            let value = client.select("SELECT index(array, 1) FROM (SELECT simple_array(i) array FROM generate_series(1, 100, 1) i) d", None, None)
                .first()
                .get_one::<f64>();
            assert_eq!(value, Some(1.0));
        })
    }
}
