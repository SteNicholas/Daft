use super::ProtoResult;
use crate::{
    non_null, not_implemented_err,
    proto::{from_proto, from_proto_vec, to_proto_vec, ToFromProto},
};

/// Export daft_ir types under an `ir` namespace to concisely disambiguate domains.
#[rustfmt::skip]
mod ir {
    pub use crate::*;
}

#[rustfmt::skip]
mod proto {
    pub use daft_proto::protos::daft::v1::*;
    pub use daft_proto::protos::daft::v1::scalar_fn::Descriptor as ScalarFnDescriptor;
}

/// Handles switching on the variant which is hidden behind the expr type.
///
/// We have special forms in the old FunctionExpr, as well as the LegacyPythonUDF which
/// requires special modeling. The common supertype is ir::Expr, so this method
/// will handle the various representations to simply return an expression.
/// It's the fact that we have scalar functions nested within two different types
/// that requires special handling when converting back into the IR i.e. we have
/// to figure out which type we are splitting into.
pub fn from_proto_function(message: proto::ScalarFn) -> ProtoResult<ir::Expr> {
    let args = ir::functions::FunctionArgs::from_proto(non_null!(message.args))?;
    let expr = match non_null!(message.descriptor) {
        proto::scalar_fn::Descriptor::Py(py) => match non_null!(py.variant) {
            proto::scalar_fn::py_fn::Variant::Legacy(legacy_fn) => {
                let func = ir::functions::python::LegacyPythonUDF::from_proto(legacy_fn)?;
                let args = args.into_inner();
                ir::rex::from_py_legacy_func(func, args)
            }
            proto::scalar_fn::py_fn::Variant::RowWise(row_wise_fn) => {
                let func = ir::functions::RowWisePyFn {
                    function_name: row_wise_fn.name.into(),
                    inner: from_proto(row_wise_fn.inner)?,
                    return_dtype: from_proto(row_wise_fn.return_dtype)?,
                    original_args: from_proto(row_wise_fn.original_args)?,
                    args: args.into_inner(),
                };
                ir::rex::from_py_rowwise_func(func)
            }
        },
        proto::scalar_fn::Descriptor::Builtin(builtin_fn) => {
            // handle special form, otherwise it's a ScalarFn
            // match from_special_form(&rs)? {
            //     Some(_) => {
            //         //
            //         not_implemented_err!("special forms for scalar functions")
            //     },
            //     None => {
            //     }
            // }
            // Daft currently does not have static function resolution, once implemented, then
            // we will be resolving to *concrete implementations* of functions based upon type
            // signatures via string mangling or other techniques. For now, it suffices to lookup
            // the dynamic functions by name because all functions are dynamic. This
            let schema = ir::Schema::empty();
            let func = ir::functions::get_function(&builtin_fn.name); // resolve logical function from name
            let func = func.get_function(args.clone(), &schema)?; // resolve physical function from types (todo)
            ir::rex::from_builtin_func(func, args)
        }
    };
    Ok(expr)
}

pub fn scalar_fn_to_proto(sf: &ir::functions::scalar::ScalarFn) -> ProtoResult<proto::ScalarFn> {
    match sf {
        ir::functions::scalar::ScalarFn::Builtin(builtin_scalar_fn) => {
            let args = builtin_scalar_fn.inputs.to_proto()?;
            Ok(proto::ScalarFn {
                descriptor: Some(proto::scalar_fn::Descriptor::Builtin(
                    proto::scalar_fn::BuiltinFn {
                        name: builtin_scalar_fn.name().to_string(),
                    },
                )),
                args: Some(args),
            })
        }
        ir::functions::scalar::ScalarFn::Python(ir::functions::PyScalarFn::RowWise(
            row_wise_fn,
        )) => {
            // Convert all arguments to unbound arguments (aka no param name) then reuse existing conversion logic.
            let function_args = row_wise_fn
                .args
                .iter()
                .map(|arg| ir::functions::FunctionArg::Unnamed(arg.clone()))
                .collect();
            let function_args = ir::functions::FunctionArgs::new_unchecked(function_args);
            let args = function_args.to_proto()?;

            Ok(proto::ScalarFn {
                descriptor: Some(proto::scalar_fn::Descriptor::Py(proto::scalar_fn::PyFn {
                    variant: Some(proto::scalar_fn::py_fn::Variant::RowWise(
                        proto::scalar_fn::py_fn::RowWiseFn {
                            name: row_wise_fn.function_name.to_string(),
                            return_dtype: Some(row_wise_fn.return_dtype.to_proto()?),
                            inner: Some(row_wise_fn.inner.to_proto()?),
                            original_args: Some(row_wise_fn.original_args.to_proto()?),
                        },
                    )),
                })),
                args: Some(args),
            })
        }
    }
}

/// The Expr::Function holds its args, so we can impl ToFromProto on it.
/// Also, the proto::ScalarFnDescriptor is an enum, not a message, so we also can't
/// implement ToFromProto on the FunctionExpr to Descriptor. FunctionExpr is also
/// either an RsDescriptor or PyDescriptor. All this together means the simplest method
/// is a custom to_proto implementation. Finally FunctionExpr doesn't use FunctionArgs
/// so we'll have to derive those as unnamed arguments. It would be nice to have a type
/// here rather than the inlined struct, and I could define one in mod.rs but kiss.
pub fn function_expr_to_proto(
    func: &ir::functions::FunctionExpr,
    args: &[ir::ExprRef],
) -> ProtoResult<proto::ScalarFn> {
    // build the args

    // switch
    let descriptor = match func {
        ir::functions::FunctionExpr::Map(map_expr) => {
            let rs = map_expr.to_proto()?;
            proto::ScalarFnDescriptor::Builtin(rs)
        }
        ir::functions::FunctionExpr::Sketch(sketch_expr) => {
            let rs = sketch_expr.to_proto()?;
            proto::ScalarFnDescriptor::Builtin(rs)
        }
        ir::functions::FunctionExpr::Struct(struct_expr) => {
            let rs = struct_expr.to_proto()?;
            proto::ScalarFnDescriptor::Builtin(rs)
        }
        ir::functions::FunctionExpr::Partitioning(partitioning_expr) => {
            let rs = partitioning_expr.to_proto()?;
            proto::ScalarFnDescriptor::Builtin(rs)
        }
        ir::functions::FunctionExpr::Python(python_udf) => {
            let py = python_udf.to_proto()?;
            proto::ScalarFnDescriptor::Py(proto::scalar_fn::PyFn {
                variant: Some(proto::scalar_fn::py_fn::Variant::Legacy(py)),
            })
        }
    };

    // Convert all arguments to unbound arguments (aka no param name) then reuse existing conversion logic.
    let function_args = args
        .iter()
        .map(|arg| ir::functions::FunctionArg::Unnamed(arg.clone()))
        .collect();
    let function_args = ir::functions::FunctionArgs::new_unchecked(function_args);
    let args = function_args.to_proto()?;

    Ok(proto::ScalarFn {
        descriptor: Some(descriptor),
        args: Some(args),
    })
}

/// FunctionArgs are not bound but are a representation of *how* the customer passed arguments.
impl ToFromProto for ir::functions::FunctionArgs<ir::ExprRef> {
    type Message = proto::scalar_fn::Args;

    fn from_proto(message: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        let args = from_proto_vec(message.args)?;
        Ok(Self::new_unchecked(args))
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        let args = to_proto_vec(self.iter())?;
        Ok(Self::Message { args })
    }
}

/// FunctionArg was passed either named or not, in the future we want ALL arguments to be bound to their parameter.
impl ToFromProto for ir::functions::FunctionArg<ir::ExprRef> {
    type Message = proto::scalar_fn::Arg;

    fn from_proto(message: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        let name = message.param.clone();
        let expr = ir::Expr::from_proto(non_null!(message.expr))?.into();
        let arg = match name {
            Some(name) => Self::named(name, expr),
            None => Self::unnamed(expr),
        };
        Ok(arg)
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        let arg = match self {
            Self::Named { name, arg } => Self::Message {
                param: name.to_string().into(),
                expr: arg.to_proto()?.into(),
            },
            Self::Unnamed(arg) => Self::Message {
                param: None,
                expr: arg.to_proto()?.into(),
            },
        };
        Ok(arg)
    }
}

/// Returns some ToFromProto type for the special form magic strings.
///
/// Note:
/// This lets us consolidate the modeling of scalar functions in the protos while
/// the DSL remains split across types. Ideally these are modeled as either their
/// own expressions for within the scalar function expression. Interesting there
/// are path expressions in this, which are best modeled as their own expressions
/// which enables path flattening/merging. I would suggest adding path expression
/// variants, making sketch_percentile a scalar function, adding a "partition
/// transform" special form since its pattern matched elsewhere, then making the
/// python UDF its own thing. I've chose to model all as builtins because it's
/// quite simple to go in/out at the expense of some hackery.
#[allow(unused)]
fn from_special_form(
    message: proto::scalar_fn::BuiltinFn,
) -> ProtoResult<Option<ir::functions::FunctionExpr>> {
    let sf = match message.name.as_str() {
        "_map_get" => {
            let map_expr = ir::functions::map::MapExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Map(map_expr)
        }
        "_sketch_percentile" => {
            let sketch_expr = ir::functions::sketch::SketchExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Sketch(sketch_expr)
        }
        "_struct_get" => {
            let struct_expr = ir::functions::struct_::StructExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Struct(struct_expr)
        }
        // Interestingly, we have common_scan_info::partitioning and functions::partitioning
        "_partitioning_years" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        "_partitioning_months" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        "_partitioning_days" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        "_partitioning_hours" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        "_partitioning_iceberg_bucket" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        "_partitioning_iceberg_truncate" => {
            let partitioning_expr =
                ir::functions::partitioning::PartitioningExpr::from_proto(message)?;
            ir::functions::FunctionExpr::Partitioning(partitioning_expr)
        }
        _ => return Ok(None),
    };
    Ok(Some(sf))
}

impl ToFromProto for ir::functions::map::MapExpr {
    type Message = proto::scalar_fn::BuiltinFn;

    fn from_proto(_: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        not_implemented_err!("map_expr")
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        not_implemented_err!("map_expr")
    }
}

impl ToFromProto for ir::functions::sketch::SketchExpr {
    type Message = proto::scalar_fn::BuiltinFn;

    fn from_proto(_: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        not_implemented_err!("sketch_expr")
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        not_implemented_err!("sketch_expr")
    }
}

impl ToFromProto for ir::functions::struct_::StructExpr {
    type Message = proto::scalar_fn::BuiltinFn;

    fn from_proto(_: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        not_implemented_err!("struct_expr")
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        not_implemented_err!("struct_expr")
    }
}

impl ToFromProto for ir::functions::partitioning::PartitioningExpr {
    type Message = proto::scalar_fn::BuiltinFn;

    fn from_proto(_: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        not_implemented_err!("partitioning_expr")
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        not_implemented_err!("partitioning_expr")
    }
}

/// Converts a legacy python UDF into a protobuf via the pickled callable and its args.
impl ToFromProto for ir::functions::python::LegacyPythonUDF {
    type Message = proto::scalar_fn::py_fn::LegacyFn;

    fn from_proto(message: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        // Convert the signature.
        let name = message.name;
        let arity = message.arity as usize;
        let return_type: ir::DataType = from_proto(message.return_type)?;

        // Convert the RuntimePyObject and the Rust compile is smart enough to know the types.
        let callable = from_proto(message.callable)?;
        let callable_init_args = from_proto(message.callable_init_args)?;
        let callable_call_args = from_proto(message.callable_call_args)?;

        // It's safe to assume the newly created PythonUDF is not initialized.
        let func = ir::functions::python::MaybeInitializedUDF::Uninitialized {
            inner: callable,
            init_args: callable_init_args,
        };

        // Convert the numeric fields back to their original types
        let concurrency = message.concurrency.map(|c| c as usize);
        let batch_size = message.batch_size.map(|b| b as usize);
        let use_process = message.use_process;

        // Reconstruct the ResourceRequest from the flattened fields
        let resource_request = {
            let num_cpus = message.num_cpus.map(|c| c as f64);
            let num_gpus = message.num_gpus.map(|g| g as f64);
            let max_memory_bytes = message.max_memory_bytes.map(|m| m as usize);

            if num_cpus.is_some() || num_gpus.is_some() || max_memory_bytes.is_some() {
                Some(common_resource_request::ResourceRequest::try_new_internal(
                    num_cpus,
                    num_gpus,
                    max_memory_bytes,
                )?)
            } else {
                None
            }
        };

        Ok(Self {
            name: name.into(),
            func,
            bound_args: callable_call_args,
            num_expressions: arity,
            return_dtype: return_type,
            resource_request,
            batch_size,
            concurrency,
            use_process,
        })
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        // Convert signature
        let name = self.name.to_string();
        let arity = self.num_expressions as u64;
        let return_type = self.return_dtype.to_proto()?;

        // Convert all the python things.
        let (callable, callable_init_args) = match &self.func {
            ir::functions::python::MaybeInitializedUDF::Initialized(py) => {
                // no args, already have the callable
                let callable = py.to_proto()?;
                let callable_init_args =
                    ir::functions::python::RuntimePyObject::new_none().to_proto()?;
                (callable, callable_init_args)
            }
            ir::functions::python::MaybeInitializedUDF::Uninitialized { inner, init_args } => {
                let callable = inner.to_proto()?;
                let callable_init_args = init_args.to_proto()?;
                (callable, callable_init_args)
            }
        };

        // The decorator creates a closure and these are the arguments captured in that scope.
        let callable_call_args = self.bound_args.to_proto()?;

        // Now flatten out what is currently "resources" but will get renamed at some point.
        let concurrency: Option<u64> = self.concurrency.map(|s| s as u64);
        let batch_size: Option<u64> = self.batch_size.map(|s| s as u64);
        let (num_cpus, num_gpus, max_memory_bytes) = match &self.resource_request {
            Some(req) => {
                let num_cpus: Option<u64> = req.num_cpus().map(|f| f as u64);
                let num_gpus: Option<u64> = req.num_gpus().map(|f| f as u64);
                let max_memory_bytes: Option<u64> = req.memory_bytes().map(|s| s as u64);
                (num_cpus, num_gpus, max_memory_bytes)
            }
            None => (None, None, None),
        };

        Ok(Self::Message {
            name,
            arity,
            return_type: Some(return_type),
            callable: Some(callable),
            callable_init_args: Some(callable_init_args),
            callable_call_args: Some(callable_call_args),
            concurrency,
            batch_size,
            num_cpus,
            num_gpus,
            max_memory_bytes,
            use_process: self.use_process,
        })
    }
}

/// Use the PyRuntimeObject to avoid dealing with the python feature flag.
impl ToFromProto for ir::functions::python::RuntimePyObject {
    type Message = proto::PyObject;

    fn from_proto(message: Self::Message) -> ProtoResult<Self>
    where
        Self: Sized,
    {
        Ok(bincode::deserialize(&message.object)?)
    }

    fn to_proto(&self) -> ProtoResult<Self::Message> {
        Ok(Self::Message {
            object: bincode::serialize(self)?,
        })
    }
}
