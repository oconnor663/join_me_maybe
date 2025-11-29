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
        let total_definitely = self.arms.iter().filter(|arm| arm.is_definitely).count();
        let definitely_finished_count =
            format_ident!("definitely_finished_count", span = Span::mixed_site());
        initializers.extend(quote! {
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
                // This identifier will be in-scope for caller code in the arms.
                if self.arms[i].is_definitely {
                    initializers.extend(quote! {
                        let #label = join_me_maybe::Canceller::new_definitely(&#flag_name, &#definitely_finished_count);
                    });
                } else {
                    initializers.extend(quote! {
                        // This identifier will be in-scope for caller code in the arms.
                        let #label = join_me_maybe::Canceller::new_maybe(&#flag_name);
                    });
                }
            }
        }

        // Now define all the arm futures, which will have the cancellers above in-scope.
        let arm_futures: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}", span = Span::mixed_site()))
            .collect();
        for (arm, arm_future) in self.arms.iter().zip(&arm_futures) {
            let body = &arm.body;
            initializers.extend(quote! {
                let mut #arm_future = ::core::pin::pin!(::join_me_maybe::maybe_done::maybe_done(#body));
            });
        }

        let mut polling_and_counting = TokenStream2::new();
        for ((arm, arm_future), finished_flag) in
            self.arms.iter().zip(&arm_futures).zip(&finished_flag_names)
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
                    if Future::poll(#arm_future.as_mut(), cx).is_ready() {
                        #mark_finished
                    }
                }
                if #definitely_finished_count.load(Relaxed) == #total_definitely {
                    break;
                }
            });
        }

        // When a future gets cancelled, that means two thing. First, the obvious one, it shouldn't
        // ever get polled again. But second -- and it's easy to miss this part -- it needs to get
        // *dropped promptly*. Consider a case where one arm is holding an async lock, and another
        // arm is trying to acquire it. If the first arm is cancelled but not dropped, then the
        // second arm will deadlock. See "Futurelock": https://rfd.shared.oxide.computer/rfd/0609.
        let mut cancelling = TokenStream2::new();
        for ((arm, arm_future), finished_flag) in
            self.arms.iter().zip(&arm_futures).zip(&finished_flag_names)
        {
            if arm.cancel_label.is_some() {
                cancelling.extend(quote! {
                    if #finished_flag.load(Relaxed) {
                        #arm_future.as_mut().cancel_if_pending();
                    }
                });
            }
        }

        let mut return_values = TokenStream2::new();
        for (arm, arm_future) in self.arms.iter().zip(&arm_futures) {
            if !arm.is_definitely || arm.cancel_label.is_some() {
                // This arm is cancellable. Keep it wrapped in `Option`.
                return_values.extend(quote! {
                    #arm_future.as_mut().take_output(),
                });
            } else {
                // There's no way to cancel this arm without cancelling the whole macro. Unwrap it.
                return_values.extend(quote! {
                    #arm_future.as_mut().take_output().expect("this arm can't be cancelled"),
                });
            }
        }

        tokens.extend(quote! {
            {
                #initializers
                ::core::future::poll_fn(|cx| {
                    use ::core::sync::atomic::Ordering::Relaxed;
                    use ::core::future::Future;
                    use ::core::task::Poll;
                    // Not really a loop, just a way to short-circuit with `break`.
                    loop {
                        #polling_and_counting
                        // Polling above might `break` and skip cancelling. That's fine, because
                        // everything drops after we return `Ready`.
                        #cancelling
                        // If we don't `break` during polling, we exit here.
                        return Poll::Pending;
                    }
                    // If we `break` during polling, we exit here.
                    Poll::Ready((#return_values))
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
