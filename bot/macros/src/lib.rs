use proc_macro::TokenStream;
use quote::ToTokens;
use syn::parse_macro_input;

mod sql_enum;

use sql_enum::SqlEnum;

#[proc_macro_attribute]
pub fn sql_enum(_meta: TokenStream, input: TokenStream) -> TokenStream {
    parse_macro_input!(input as SqlEnum)
        .to_token_stream()
        .into()
}
