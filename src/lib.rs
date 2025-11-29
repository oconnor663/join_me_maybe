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
    cancel_label: Option<Ident>,
    is_definitely: bool,
    body: Expr,
}

impl Parse for JoinMeMaybeArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let cancel_label = if input.peek(syn::Ident) && input.peek2(syn::Token![:]) {
            let ident = input.parse::<Ident>()?;
            _ = input.parse::<syn::Token![:]>()?;
            Some(ident)
        } else {
            None
        };
        let lookahead = input.lookahead1();
        let is_definitely = if lookahead.peek(kw::definitely) {
            _ = input.parse::<kw::definitely>()?;
            true
        } else if lookahead.peek(kw::maybe) {
            _ = input.parse::<kw::maybe>()?;
            false
        } else {
            return Err(lookahead.error());
        };
        let body = input.parse()?;
        Ok(Self {
            cancel_label,
            is_definitely,
            body,
        })
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
        if !arms.iter().any(|arm| arm.is_definitely) {
            return Err(input.error("At least one arm must be `definitely`"));
        }
        Ok(Self { arms })
    }
}

impl ToTokens for JoinMeMaybe {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let mut initializers = TokenStream2::new();

        // First define all the finished flags and cancellers. The finished flags get set to true
        // whenever an arm finishes naturally (poll returns Ready) *or* another atom cancels it.
        let total_definitely_count = self.arms.iter().filter(|arm| arm.is_definitely).count();
        let total_definitely_const = format_ident!("TOTAL_DEFINITELY", span = Span::mixed_site());
        let definitely_finished_count =
            format_ident!("definitely_finished_count", span = Span::mixed_site());
        initializers.extend(quote! {
            const #total_definitely_const: usize = #total_definitely_count;
            let #definitely_finished_count = ::core::sync::atomic::AtomicUsize::new(0);
        });
        let finished_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_finished", span = Span::mixed_site()))
            .collect();
        for i in 0..self.arms.len() {
            let flag_name = &finished_flag_names[i];
            initializers.extend(quote! {
                let #flag_name = ::core::sync::atomic::AtomicBool::new(false);
            });
            if let Some(label) = &self.arms[i].cancel_label {
                let mut count_field = TokenStream2::new();
                let mut count_field_initialize = TokenStream2::new();
                let mut cancel_body = TokenStream2::new();
                if self.arms[i].is_definitely {
                    count_field.extend(quote! {
                        count: &'a ::core::sync::atomic::AtomicUsize,
                    });
                    count_field_initialize.extend(quote! {
                        count: &#definitely_finished_count,
                    });
                    cancel_body.extend(quote! {
                        // No need for atomic compare-exchange or fetch-add here. We're only using
                        // atomics to avoid needing to write unsafe code. (Relaxed atomic loads and
                        // stores are extremely cheap, often equal to regular ones.)
                        use ::core::sync::atomic::Ordering::Relaxed;
                        if !self.finished.load(Relaxed) {
                            self.finished.store(true, Relaxed);
                            // To keep this count accurate, we rely on each arm to mark itself
                            // finished if it exits naturally. See below.
                            self.count.store(self.count.load(Relaxed) + 1, Relaxed);
                        }
                    });
                } else {
                    cancel_body.extend(quote! {
                        // This is a `maybe` arm, so it doesn't matter if this flag gets set twice.
                        // We don't count these.
                        use ::core::sync::atomic::Ordering::Relaxed;
                        self.finished.store(true, Relaxed);
                    });
                }
                initializers.extend(quote! {
                    // This identifier will be in-scope for caller code in the arms.
                    let #label = {
                        struct Canceller<'a> {
                            finished: &'a ::core::sync::atomic::AtomicBool,
                            #count_field
                        };
                        impl<'a> Canceller<'a> {
                            fn cancel(&self) {
                                #cancel_body
                            }
                        }
                        Canceller {
                            finished: &#flag_name,
                            #count_field_initialize
                        }
                    };
                });
            }
        }

        // Now define all the arm futures, which will have the cancellers above in-scope.
        let arm_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}", span = Span::mixed_site()))
            .collect();
        for (arm, name) in self.arms.iter().zip(&arm_names) {
            let body = &arm.body;
            initializers.extend(quote! {
                let mut #name = ::core::pin::pin!(::futures::future::maybe_done(#body));
            });
        }

        let mut return_values = TokenStream2::new();
        for name in &arm_names {
            return_values.extend(quote! {
                #name.as_mut().take_output(),
            });
        }

        let mut polling_and_counting = TokenStream2::new();
        polling_and_counting.extend(quote! {
            use ::core::sync::atomic::Ordering::Relaxed;
            use ::core::future::Future;
            use ::core::task::Poll;
        });
        let definitely_finished = format_ident!("definitely_finished", span = Span::mixed_site());
        let cx = format_ident!("cx", span = Span::mixed_site());
        for ((arm, name), finished_flag) in
            self.arms.iter().zip(&arm_names).zip(&finished_flag_names)
        {
            let mut mark_finished = TokenStream2::new();
            if arm.is_definitely {
                mark_finished.extend(quote! {
                    // This `definitely` arm just exited naturally. Check again whether its
                    // finished flag is already set (i.e. whether it pointlessly cancelled itself
                    // right before exiting and already updated the finish count that way). If not,
                    // update the finished count. As above, fetch-add isn't needed.
                    if !#finished_flag.load(Relaxed) {
                        #finished_flag.store(true, Relaxed);
                        #definitely_finished_count.store(#definitely_finished_count.load(Relaxed) + 1, Relaxed);
                    }
                });
            } else {
                // For `maybe` arms, just set `finished` so that we don't poll them again. As with
                // the cancellers above, it doesn't matter if this gets set twice, because we're
                // not counting these.
                mark_finished.extend(quote! {
                    #finished_flag.store(true, Relaxed);
                });
            }
            polling_and_counting.extend(quote! {
                if !#finished_flag.load(Relaxed) {
                    if Future::poll(#name.as_mut(), #cx).is_ready() {
                        #mark_finished
                    }
                }
                if #definitely_finished_count.load(Relaxed) == #total_definitely_const {
                    return Poll::Ready((#return_values));
                }
            });
        }

        tokens.extend(quote! {
            {
                #initializers
                ::core::future::poll_fn(|#cx| {
                    let mut #definitely_finished = true;
                    #polling_and_counting
                    Poll::Pending
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
