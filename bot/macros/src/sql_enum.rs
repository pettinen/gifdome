use convert_case::{Case, Casing};
use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, ParseBuffer},
    token::Eq,
    Attribute, Expr, Fields, ItemEnum, LitStr, Variant,
};

pub struct SqlEnum {
    r#enum: ItemEnum,
    variants: Vec<SqlEnumVariant>,
}

impl Parse for SqlEnum {
    fn parse(input: &ParseBuffer) -> syn::Result<Self> {
        let r#enum = input.parse::<ItemEnum>()?;
        let variants = r#enum
            .variants
            .clone()
            .into_iter()
            .map(SqlEnumVariant::new)
            .collect();

        Ok(Self { r#enum, variants })
    }
}

impl ToTokens for SqlEnum {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let name_ident = Ident::new("name", Span::call_site());
        let (name_attrs, other_attrs): (Vec<&Attribute>, Vec<&Attribute>) = self
            .r#enum
            .attrs
            .iter()
            .partition(|attr| attr.path().is_ident(&name_ident));
        if name_attrs.len() > 1 {
            panic!("multiple name(...) attributes specified for sql_enum");
        }
        let snake_case = match name_attrs.first() {
            Some(name_attr) => name_attr.parse_args::<LitStr>().unwrap().value(),
            None => self.r#enum.ident.to_string().to_case(Case::Snake),
        };

        let vis = &self.r#enum.vis;
        let ident = &self.r#enum.ident;
        let generics = &self.r#enum.generics;
        let variants = &self.variants;
        let variant_names = variants.into_iter().map(|variant| &variant.snake_case);
        let impl_display_lines = variants
            .into_iter()
            .map(|variant| variant.impl_display_line());

        tokens.extend(quote! {
            #[derive(Debug, FromSql, ToSql, Serialize)]
            #[postgres(name = #snake_case)]
            #(#other_attrs)*
            #vis enum #ident #generics {
                #(#variants),*
            }

            impl #ident {
                pub fn variants() -> Vec<String> {
                    vec![#(#variant_names),*].into_iter().map(|name| name.to_string()).collect()
                }
            }

            impl std::fmt::Display for #ident {
                fn fmt(&self, f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    let as_string = match self {
                        #(#impl_display_lines),*
                    };
                    write!(f, "{}", as_string)
                }
            }
        })
    }
}

struct SqlEnumVariant {
    attrs: Vec<Attribute>,
    discriminant: Option<(Eq, Expr)>,
    fields: Fields,
    ident: Ident,
    snake_case: String,
    kebab_case: String,
}

impl SqlEnumVariant {
    fn new(variant: Variant) -> Self {
        let name_ident = Ident::new("name", Span::call_site());
        let (name_attrs, other_attrs): (Vec<&Attribute>, Vec<&Attribute>) = variant
            .attrs
            .iter()
            .partition(|attr| attr.path().is_ident(&name_ident));
        if name_attrs.len() > 1 {
            panic!("multiple name(...) attributes specified for sql_enum variant");
        }
        let snake_case = match name_attrs.first() {
            Some(name_attr) => name_attr.parse_args::<LitStr>().unwrap().value(),
            None => variant.ident.to_string().to_case(Case::Snake),
        };
        let kebab_case = match name_attrs.first() {
            Some(name_attr) => name_attr.parse_args::<LitStr>().unwrap().value(),
            None => variant.ident.to_string().to_case(Case::Kebab),
        };
        Self {
            attrs: other_attrs.into_iter().cloned().collect(),
            discriminant: variant.discriminant,
            fields: variant.fields,
            ident: variant.ident,
            snake_case,
            kebab_case,
        }
    }

    fn impl_display_line(&self) -> TokenStream {
        let ident = &self.ident;
        let kebab_case = &self.kebab_case;
        quote! {
            Self::#ident => #kebab_case
        }
    }
}

impl ToTokens for SqlEnumVariant {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let attrs = &self.attrs;
        let fields = &self.fields;
        let ident = &self.ident;
        let snake_case = &self.snake_case;
        let kebab_case = &self.kebab_case;

        tokens.extend(quote! {
            #[postgres(name = #snake_case)]
            #[serde(rename = #kebab_case)]
            #(#attrs)*
            #ident #fields
        });
        if let Some(discriminant) = &self.discriminant {
            let expr = &discriminant.1;
            tokens.extend(quote! {
                = #expr
            });
        }
    }
}
