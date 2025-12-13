// This lint is triggered by darling's generated code for `#[darling(default)]`
#![allow(clippy::option_if_let_else)]

use darling::{FromDeriveInput, FromMeta, util::PathList};
use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, Ident, Path, parse_macro_input};

/// Converts a `PascalCase` or `camelCase` string to `kebab-case`.
fn to_kebab_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('-');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Wrapper for `syn::Path` that parses from `key = Type` syntax.
#[derive(Debug)]
struct TypePath(Path);

impl FromMeta for TypePath {
    fn from_meta(item: &syn::Meta) -> darling::Result<Self> {
        if let syn::Meta::NameValue(nv) = item
            && let syn::Expr::Path(expr_path) = &nv.value
        {
            return Ok(Self(expr_path.path.clone()));
        }
        Err(darling::Error::unsupported_shape("expected `key = Type`"))
    }
}

/// Configuration for the `#[aggregate(...)]` attribute.
#[derive(Debug, FromDeriveInput)]
#[darling(attributes(aggregate), supports(struct_any))]
struct AggregateArgs {
    ident: Ident,
    vis: syn::Visibility,
    id: TypePath,
    error: TypePath,
    events: PathList,
    #[darling(default)]
    kind: Option<String>,
    #[darling(default)]
    event_enum: Option<String>,
}

/// Derives the `Aggregate` trait for a struct.
///
/// This macro generates:
/// - An event enum containing all aggregate event types
/// - `ProjectionEvent` trait implementation for event deserialization
/// - `SerializableEvent` trait implementation for event serialization
/// - `From<E>` implementations for each event type
/// - `Aggregate` trait implementation that dispatches to `Apply<E>` for events
///
/// **Note:** Commands are handled via individual `Handle<C>` trait implementations.
/// No command enum is generated - use `execute_command::<Aggregate, Command>()` directly.
///
/// # Attributes
///
/// ## Required
/// - `id = Type` - Aggregate ID type
/// - `error = Type` - Error type for command handling
/// - `events(Type1, Type2, ...)` - Event types
///
/// ## Optional
/// - `kind = "name"` - Aggregate type identifier (default: lowercase struct name)
/// - `event_enum = "Name"` - Override generated event enum name (default: `{Struct}Event`)
///
/// # Example
///
/// ```ignore
/// #[derive(Aggregate)]
/// #[aggregate(id = String, error = String, events(FundsDeposited, FundsWithdrawn))]
/// pub struct Account {
///     balance: i64,
/// }
/// ```
#[proc_macro_derive(Aggregate, attributes(aggregate))]
pub fn derive_aggregate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match AggregateArgs::from_derive_input(&input) {
        Ok(args) => generate_aggregate_impl(args, &input),
        Err(err) => err.write_errors().into(),
    }
}

fn generate_aggregate_impl(args: AggregateArgs, input: &DeriveInput) -> TokenStream {
    let event_types: Vec<&Path> = args.events.iter().collect();

    if event_types.is_empty() {
        return darling::Error::custom("events(...) must contain at least one event type")
            .with_span(&input.ident)
            .write_errors()
            .into();
    }

    let struct_name = &args.ident;
    let struct_vis = &args.vis;
    let id_type = &args.id.0;
    let error_type = &args.error.0;

    let kind = args
        .kind
        .unwrap_or_else(|| to_kebab_case(&struct_name.to_string()));

    let event_enum_name = args.event_enum.map_or_else(
        || Ident::new(&format!("{struct_name}Event"), struct_name.span()),
        |name| Ident::new(&name, struct_name.span()),
    );

    let variant_names: Vec<_> = event_types
        .iter()
        .map(|p| {
            &p.segments
                .last()
                .expect("event type path must have at least one segment")
                .ident
        })
        .collect();

    let expanded = quote! {
        #[derive(Clone, ::serde::Serialize, ::serde::Deserialize)]
        #struct_vis enum #event_enum_name {
            #(#variant_names(#event_types)),*
        }

        impl ::event_sourcing::ProjectionEvent for #event_enum_name {
            const EVENT_KINDS: &'static [&'static str] = &[#(#event_types::KIND),*];

            fn from_stored<C: ::event_sourcing::Codec>(
                kind: &str,
                data: &[u8],
                codec: &C,
            ) -> Result<Self, ::event_sourcing::EventDecodeError<C::Error>> {
                match kind {
                    #(#event_types::KIND => Ok(Self::#variant_names(
                        codec.deserialize(data).map_err(::event_sourcing::EventDecodeError::Codec)?
                    )),)*
                    _ => Err(::event_sourcing::EventDecodeError::UnknownKind {
                        kind: kind.to_string(),
                        expected: Self::EVENT_KINDS,
                    }),
                }
            }
        }

        impl ::event_sourcing::SerializableEvent for #event_enum_name {
            fn to_persistable<C: ::event_sourcing::Codec, M>(
                self,
                codec: &C,
                metadata: M,
            ) -> Result<::event_sourcing::PersistableEvent<M>, C::Error> {
                let (kind, data) = match self {
                    #(Self::#variant_names(event) => (#event_types::KIND.to_string(), codec.serialize(&event)?)),*
                };
                Ok(::event_sourcing::PersistableEvent { kind, data, metadata })
            }
        }

        #(
            impl From<#event_types> for #event_enum_name {
                fn from(event: #event_types) -> Self {
                    Self::#variant_names(event)
                }
            }
        )*

        impl ::event_sourcing::Aggregate for #struct_name {
            const KIND: &'static str = #kind;
            type Event = #event_enum_name;
            type Error = #error_type;
            type Id = #id_type;

            fn apply(&mut self, event: &Self::Event) {
                match event {
                    #(#event_enum_name::#variant_names(e) => ::event_sourcing::Apply::apply(self, e)),*
                }
            }
        }
    };

    TokenStream::from(expanded)
}
