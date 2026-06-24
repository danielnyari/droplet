//! Procedural macros for Droplet. `#[droplet_tool]` makes a Rust function callable from sandboxed
//! agent code: it re-emits the function, generates a Monty dispatch thunk and a Python `.pyi` stub
//! line from the signature, and registers both at link time via `inventory`. There is no
//! hand-maintained tool table or stub file anywhere (PRODUCT.md invariant #4).

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

/// Mark a function as a Droplet tool. See the module docs for the calling convention.
#[proc_macro_attribute]
pub fn droplet_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let fn_name = func.sig.ident.clone();
    let fn_name_str = fn_name.to_string();
    let thunk_name = format_ident!("__droplet_dispatch_{}", fn_name);

    // Split the engine parameter (if first and typed &mut DuckEngine) from the agent-visible ones.
    let params: Vec<&FnArg> = func.sig.inputs.iter().collect();
    let engine_first = params.first().is_some_and(|a| is_engine_param(a));
    let visible: Vec<&FnArg> = if engine_first {
        params[1..].to_vec()
    } else {
        params.clone()
    };

    // For each agent-visible param, capture (ident, type) for both the thunk and the stub.
    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();
    for arg in &visible {
        let FnArg::Typed(pt) = arg else {
            return compile_err(&func, "tools cannot take `self`");
        };
        let Pat::Ident(pi) = &*pt.pat else {
            return compile_err(&func, "tool parameters must be simple identifiers");
        };
        arg_idents.push(pi.ident.clone());
        arg_types.push((*pt.ty).clone());
    }

    // The thunk converts args via FromMonty, calls the fn (engine passed through if present), and
    // packs the return via IntoMonty. Tools return Result<R, DropletError>, so `?` propagates.
    let indices: Vec<syn::Index> = (0..arg_idents.len()).map(syn::Index::from).collect();
    let call = if engine_first {
        quote! { #fn_name(eng, #(#arg_idents),*) }
    } else {
        quote! { #fn_name(#(#arg_idents),*) }
    };
    let engine_binding = if engine_first {
        quote! {}
    } else {
        // Engine is unused for engine-less tools; silence the warning without renaming the param.
        quote! { let _ = &mut *eng; }
    };

    // The Python stub line, e.g. `def echo(text: str) -> str: ...`.
    let ret_py = python_return_type(&func.sig.output);
    let stub = build_stub(&fn_name_str, &arg_idents, &arg_types, &ret_py);

    let expanded = quote! {
        #func

        #[doc(hidden)]
        fn #thunk_name(
            eng: &mut crate::engine_duckdb::DuckEngine,
            args: &[::monty::MontyObject],
            _kwargs: &[(::monty::MontyObject, ::monty::MontyObject)],
        ) -> ::core::result::Result<::monty::MontyObject, crate::DropletError> {
            #engine_binding
            #( let #arg_idents = <#arg_types as crate::convert::FromMonty>::from_monty(&args[#indices])?; )*
            let __ret = #call?;
            ::core::result::Result::Ok(crate::convert::IntoMonty::into_monty(__ret))
        }

        ::inventory::submit! {
            crate::tool::Tool {
                name: #fn_name_str,
                stub: #stub,
                dispatch: #thunk_name,
            }
        }
    };
    expanded.into()
}

/// True if this parameter is `eng: &mut DuckEngine` (the injected host engine).
fn is_engine_param(arg: &FnArg) -> bool {
    let FnArg::Typed(pt) = arg else { return false };
    let Type::Reference(r) = &*pt.ty else {
        return false;
    };
    r.mutability.is_some() && last_ident(&r.elem).as_deref() == Some("DuckEngine")
}

/// The last path-segment identifier of a type (`&str` -> "str", `Rows` -> "Rows", etc.).
fn last_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Reference(r) => last_ident(&r.elem),
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Rust type -> Python stub type. Unknown -> "object" (callers should add a known mapping instead).
fn python_type(ty: &Type) -> String {
    match last_ident(ty).as_deref() {
        Some("String" | "str") => "str",
        Some("i64") => "int",
        Some("f64") => "float",
        Some("bool") => "bool",
        Some("Rows") => "list[dict]",
        _ => "object",
    }
    .to_string()
}

/// The Python return type from `-> Result<R, DropletError>` (or from a bare `-> R`).
fn python_return_type(output: &ReturnType) -> String {
    let ReturnType::Type(_, ty) = output else {
        return "None".to_string();
    };
    if let Type::Path(p) = &**ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Result"
        && let syn::PathArguments::AngleBracketed(ab) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = ab.args.first()
    {
        return python_type(inner);
    }
    python_type(ty)
}

/// Assemble `def NAME(p1: t1, p2: t2) -> ret: ...`.
fn build_stub(name: &str, idents: &[syn::Ident], types: &[Type], ret_py: &str) -> String {
    let params = idents
        .iter()
        .zip(types)
        .map(|(id, ty)| format!("{id}: {}", python_type(ty)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("def {name}({params}) -> {ret_py}: ...")
}

/// Emit a compile error attached to the offending function's tokens.
fn compile_err(func: &ItemFn, msg: &str) -> TokenStream {
    syn::Error::new_spanned(&func.sig, msg)
        .to_compile_error()
        .into()
}
