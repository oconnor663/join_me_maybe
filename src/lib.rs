use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, format_ident, quote};
use syn::{
    Expr, Ident,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

mod kw {
    syn::custom_keyword!(maybe);
    syn::custom_keyword!(definitely);
}

struct JoinMeMaybeArm {
    body: Expr,
    is_maybe: bool,
}

impl Parse for JoinMeMaybeArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        let is_maybe = if lookahead.peek(kw::definitely) {
            _ = input.parse::<kw::definitely>()?;
            false
        } else if lookahead.peek(kw::maybe) {
            _ = input.parse::<kw::maybe>()?;
            true
        } else {
            return Err(lookahead.error());
        };
        let body = input.parse()?;
        Ok(Self { body, is_maybe })
    }
}

struct JoinMeMaybe {
    arms: Vec<JoinMeMaybeArm>,
}

impl Parse for JoinMeMaybe {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut arms = Vec::new();
        while !input.is_empty() {
            let arm = input.parse::<JoinMeMaybeArm>()?;
            arms.push(arm);
            if !input.is_empty() {
                let _ = input.parse::<syn::Token![,]>()?;
            }
        }
        if arms.iter().all(|arm| arm.is_maybe) {
            return Err(input.error("At least one arm must be `definitely`"));
        }
        Ok(Self { arms })
    }
}

impl ToTokens for JoinMeMaybe {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let mut initializers = TokenStream2::new();
        let cancel_flag = format_ident!("cancel_flag", span = Span::mixed_site());
        let arm_names: Vec<Ident> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{}", i, span = Span::mixed_site()))
            .collect();
        for (arm, name) in self.arms.iter().zip(&arm_names) {
            let body = &arm.body;
            initializers.extend(quote! {
                let mut #name = ::std::pin::pin!(::futures::future::maybe_done(#body));
            });
        }

        let mut cancel_returns = TokenStream2::new();
        for _ in 0..self.arms.len() {
            cancel_returns.extend(quote! { None, });
        }

        let mut success_returns = TokenStream2::new();
        for name in &arm_names {
            success_returns.extend(quote! {
                // The `definitely` outputs are guaranteed to be `Some` here, but the `maybe` ones
                // might be `None`.
                #name.as_mut().take_output(),
            });
        }

        let mut poll_calls = TokenStream2::new();
        let definitely_finished = format_ident!("definitely_finished", span = Span::mixed_site());
        let cx = format_ident!("cx", span = Span::mixed_site());
        let total_definitely = self.arms.iter().filter(|arm| !arm.is_maybe).count();
        let mut definitely_so_far = 0;
        for (arm, name) in self.arms.iter().zip(&arm_names) {
            poll_calls.extend(quote! {
                if #name.as_mut().output_mut().is_none() {
                    _ = ::std::future::Future::poll(#name.as_mut(), #cx);
                }
                if #cancel_flag.load(::std::sync::atomic::Ordering::Relaxed) {
                    return ::std::task::Poll::Ready((#cancel_returns));
                }
            });
            if !arm.is_maybe {
                poll_calls.extend(quote! {
                    #definitely_finished &= #name.as_mut().output_mut().is_some();
                });
                definitely_so_far += 1;
                if definitely_so_far == total_definitely {
                    poll_calls.extend(quote! {
                        if #definitely_finished {
                            return ::std::task::Poll::Ready((#success_returns));
                        }
                    });
                }
            }
        }

        tokens.extend(quote! {
            {
                let mut #cancel_flag = ::std::sync::atomic::AtomicBool::new(false);
                // Note that `cancel` is not a mixed-site identifier; it's exposed.
                let cancel = || {
                    #cancel_flag.store(true, ::std::sync::atomic::Ordering::Relaxed);
                };
                #initializers
                ::std::future::poll_fn(|#cx| {
                    let mut #definitely_finished = true;
                    #poll_calls
                    ::std::task::Poll::Pending
                }).await
            }
        });
    }
}

#[proc_macro]
pub fn join_me_maybe(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let c = parse_macro_input!(input as JoinMeMaybe);
    quote! { #c }.into()
}
