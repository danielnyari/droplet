//! Procedural macros for Droplet. Currently: `#[droplet_tool]`.
//!
//! `#[droplet_tool]` turns one Rust function into a Monty external function: it re-emits the
//! function unchanged AND (Task 4) generates a dispatch thunk + a Python `.pyi` stub, registered
//! at link time via `inventory`. No hand-maintained tool table or stubs (PRODUCT.md invariant #4).

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Mark a function as a Droplet tool callable from sandboxed agent code.
///
/// V1a: identity — re-emits the function unchanged. Task 4 adds the generated dispatch + stub.
#[proc_macro_attribute]
pub fn droplet_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    quote! { #func }.into()
}
