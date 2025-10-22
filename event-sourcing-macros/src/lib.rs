use proc_macro::TokenStream;
use quote::quote;
use syn::{
    DeriveInput, Lit, Meta, Path, Token, Type, parse::Parse, parse_macro_input,
    punctuated::Punctuated, token::Comma,
};

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
/// #[aggregate(
///     id = String,
///     error = String,
///     events(FundsDeposited, FundsWithdrawn),
///     kind = "account"
/// )]
/// pub struct Account {
///     balance: i64,
/// }
///
/// impl Apply<FundsDeposited> for Account {
///     fn apply(&mut self, event: &FundsDeposited) {
///         self.balance += event.amount;
///     }
/// }
///
/// impl Handle<DepositFunds> for Account {
///     fn handle(&self, command: &DepositFunds) -> Result<Vec<Self::Event>, Self::Error> {
///         if command.amount <= 0 {
///             return Err("amount must be positive".to_string());
///         }
///         Ok(vec![FundsDeposited { amount: command.amount }.into()])
///     }
/// }
///
/// // Usage:
/// let command = DepositFunds { amount: 100 };
/// repository.execute_command::<Account, DepositFunds>(&account_id, &command, &metadata)?;
/// ```
#[proc_macro_derive(Aggregate, attributes(aggregate))]
pub fn derive_aggregate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let struct_name = &input.ident;
    let struct_vis = &input.vis;

    // Extract configuration from #[aggregate(...)] attribute
    let mut id_type: Option<Type> = None;
    let mut error_type: Option<Type> = None;
    let mut event_types: Vec<Path> = Vec::new();
    let mut command_types: Vec<Path> = Vec::new();
    let mut kind: Option<String> = None;
    let mut event_enum_name: Option<String> = None;
    let mut command_enum_name: Option<String> = None;

    for attr in &input.attrs {
        if attr.path().is_ident("aggregate")
            && let Meta::List(meta_list) = &attr.meta
        {
            let _ = meta_list.parse_nested_meta(|meta| {
                if meta.path.is_ident("id") {
                    let value = meta.value()?;
                    let ty: Type = value.parse()?;
                    id_type = Some(ty);
                } else if meta.path.is_ident("error") {
                    let value = meta.value()?;
                    let ty: Type = value.parse()?;
                    error_type = Some(ty);
                } else if meta.path.is_ident("events") {
                    // Parse events(Type1, Type2, ...)
                    let content;
                    syn::parenthesized!(content in meta.input);
                    let paths: Punctuated<Path, Comma> =
                        content.parse_terminated(Path::parse, Token![,])?;
                    event_types.extend(paths);
                } else if meta.path.is_ident("commands") {
                    // Parse commands(Type1, Type2, ...)
                    let content;
                    syn::parenthesized!(content in meta.input);
                    let paths: Punctuated<Path, Comma> =
                        content.parse_terminated(Path::parse, Token![,])?;
                    command_types.extend(paths);
                } else if meta.path.is_ident("kind") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(lit_str) = lit {
                        kind = Some(lit_str.value());
                    }
                } else if meta.path.is_ident("event_enum") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(lit_str) = lit {
                        event_enum_name = Some(lit_str.value());
                    }
                } else if meta.path.is_ident("command_enum") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(lit_str) = lit {
                        command_enum_name = Some(lit_str.value());
                    }
                }
                Ok(())
            });
        }
    }

    let Some(id_type) = id_type else {
        return syn::Error::new_spanned(
            &input,
            "Aggregate derive requires #[aggregate(id = Type, ...)] attribute",
        )
        .to_compile_error()
        .into();
    };

    let Some(error_type) = error_type else {
        return syn::Error::new_spanned(
            &input,
            "Aggregate derive requires #[aggregate(error = Type, ...)] attribute",
        )
        .to_compile_error()
        .into();
    };

    if event_types.is_empty() {
        return syn::Error::new_spanned(
            &input,
            "Aggregate derive requires #[aggregate(events(...), ...)] with at least one event type",
        )
        .to_compile_error()
        .into();
    }

    // Commands are no longer required - they're handled via Handle<C> trait directly
    // The commands attribute is now ignored if present

    // Default kind to lowercase struct name
    let kind = kind.unwrap_or_else(|| struct_name.to_string().to_lowercase());

    // Default enum names
    let event_enum_name = event_enum_name
        .map(|name| syn::Ident::new(&name, struct_name.span()))
        .unwrap_or_else(|| syn::Ident::new(&format!("{}Event", struct_name), struct_name.span()));

    // Generate event enum variants
    let event_variants = event_types.iter().map(|event_type| {
        let variant_name = event_type.segments.last().unwrap().ident.clone();
        quote! {
            #variant_name(#event_type)
        }
    });

    // Generate EVENT_KINDS array
    let event_kinds = event_types.iter().map(|event_type| {
        quote! { #event_type::KIND }
    });

    // Generate from_stored match arms for events
    let event_from_stored_arms = event_types.iter().map(|event_type| {
        let variant_name = event_type.segments.last().unwrap().ident.clone();
        quote! {
            #event_type::KIND => Ok(Self::#variant_name(codec.deserialize(data)?))
        }
    });

    // Generate to_persistable match arms for events
    let event_to_persistable_arms = event_types.iter().map(|event_type| {
        let variant_name = event_type.segments.last().unwrap().ident.clone();
        quote! {
            Self::#variant_name(event) => (
                #event_type::KIND.to_string(),
                codec.serialize(&event)?
            )
        }
    });

    // Generate From<E> implementations for events
    let event_from_impls = event_types.iter().map(|event_type| {
        let variant_name = event_type.segments.last().unwrap().ident.clone();
        quote! {
            impl From<#event_type> for #event_enum_name {
                fn from(event: #event_type) -> Self {
                    Self::#variant_name(event)
                }
            }
        }
    });

    // Generate apply match arms
    let apply_arms = event_types.iter().map(|event_type| {
        let variant_name = event_type.segments.last().unwrap().ident.clone();
        quote! {
            #event_enum_name::#variant_name(ref event) => {
                ::event_sourcing::Apply::apply(self, event)
            }
        }
    });

    // Generate the complete output
    let expanded = quote! {
        // Event enum definition
        #struct_vis enum #event_enum_name {
            #(#event_variants),*
        }

        // ProjectionEvent trait implementation
        impl ::event_sourcing::ProjectionEvent for #event_enum_name {
            const EVENT_KINDS: &'static [&'static str] = &[
                #(#event_kinds),*
            ];

            fn from_stored<C: ::event_sourcing::Codec>(
                kind: &str,
                data: &[u8],
                codec: &C,
            ) -> Result<Self, C::Error> {
                match kind {
                    #(#event_from_stored_arms,)*
                    _ => panic!("Unknown event kind: {}", kind),
                }
            }
        }

        // SerializableEvent trait implementation
        impl ::event_sourcing::SerializableEvent for #event_enum_name {
            fn to_persistable<C: ::event_sourcing::Codec, M>(
                self,
                codec: &C,
                metadata: M,
            ) -> Result<::event_sourcing::PersistableEvent<M>, C::Error> {
                let (kind, data) = match self {
                    #(#event_to_persistable_arms,)*
                };
                Ok(::event_sourcing::PersistableEvent {
                    kind,
                    data,
                    metadata,
                })
            }
        }

        // From<E> implementations for events
        #(#event_from_impls)*

        // Aggregate trait implementation
        impl ::event_sourcing::Aggregate for #struct_name {
            const KIND: &'static str = #kind;

            type Event = #event_enum_name;
            type Error = #error_type;
            type Id = #id_type;

            fn apply(&mut self, event: Self::Event) {
                match event {
                    #(#apply_arms,)*
                }
            }
        }
    };

    TokenStream::from(expanded)
}
