extern crate proc_macro;

use crate::proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(State)]
pub fn derive_state_trait(input: TokenStream) -> TokenStream{
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let generics = input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let expanded = quote!{
        impl #impl_generics crate::stateful::State for #name #ty_generics #where_clause{
            fn as_any(&self) -> &dyn std::any::Any {self}
        }
    };
    TokenStream::from(expanded)
}
