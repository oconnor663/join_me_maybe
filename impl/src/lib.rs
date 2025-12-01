use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, format_ident, quote};
use syn::{
    Expr, Ident,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

mod kw {
    syn::custom_keyword!(maybe);
}

struct JoinMeMaybeArm {
    cancel_label: Option<Ident>,
    // "Definitely" is the opposite of "maybe". Previously there was a `definitely` keyword, but it
    // was unnecessarily verbose.
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
        let is_maybe = if input.peek(kw::maybe) {
            _ = input.parse::<kw::maybe>()?;
            true
        } else {
            false
        };
        let body = input.parse()?;
        Ok(Self {
            cancel_label,
            is_definitely: !is_maybe,
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

        // First define the finished flags and cancellers. The finished flags get set to true
        // whenever an arm finishes naturally (poll returns Ready) *or* another arm cancels it.
        let total_definitely = self.arms.iter().filter(|arm| arm.is_definitely).count();
        let definitely_finished_count =
            format_ident!("definitely_finished_count", span = Span::mixed_site());
        initializers.extend(quote! {
            let #definitely_finished_count = ::core::sync::atomic::AtomicUsize::new(0);
        });
        let finished_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_finished", span = Span::mixed_site()))
            .collect();
        let canceller_internal_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_canceller", span = Span::mixed_site()))
            .collect();
        for i in 0..self.arms.len() {
            if let Some(label) = &self.arms[i].cancel_label {
                let flag_name = &finished_flag_names[i];
                initializers.extend(quote! {
                    let #flag_name = ::core::sync::atomic::AtomicBool::new(false);
                });
                // We use the "internal" name below, so that the caller gets an unused variable
                // warning if they don't actually use this label themselves.
                let canceller_internal_name = &canceller_internal_names[i];
                if self.arms[i].is_definitely {
                    initializers.extend(quote! {
                        let #canceller_internal_name = join_me_maybe::Canceller::new_definitely(&#flag_name, &#definitely_finished_count);
                    });
                } else {
                    initializers.extend(quote! {
                        let #canceller_internal_name = join_me_maybe::Canceller::new_maybe(&#flag_name);
                    });
                }
                initializers.extend(quote! {
                    // This is what's in-scope for callers.
                    let #label = &#canceller_internal_name;
                });
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
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_future = &arm_futures[i];
            let finished_flag = &finished_flag_names[i];
            let canceller_internal_name = &canceller_internal_names[i];
            if arm.cancel_label.is_some() {
                // If this is a "definitely" future, we need to bump the finished count after it
                // exits, but we don't want to do that unconditionally. The future might've just
                // cancelled itself right before exiting (pointlessly?) and already bumped the
                // count. Calling `cancel` again has no effect on the output but keeps the count
                // consistent.
                polling_and_counting.extend(quote! {
                    if !#finished_flag.load(Relaxed) {
                        if Future::poll(#arm_future.as_mut(), cx).is_ready() {
                            // Use the internal name here so that the caller still gets unused
                            // variable warnings if they never refer to their label.
                            #canceller_internal_name.cancel();
                        }
                    }
                });
            } else if arm.is_definitely {
                // This "definitely" future can't be cancelled, so we can unconditionally bump the
                // count when it exits.
                polling_and_counting.extend(quote! {
                    if #arm_future.is_future() {
                        if Future::poll(#arm_future.as_mut(), cx).is_ready() {
                            #definitely_finished_count.store(#definitely_finished_count.load(Relaxed) + 1, Relaxed);
                        }
                    }
                });
            } else {
                // This is a `maybe` future without a finished/cancelled flag.
                polling_and_counting.extend(quote! {
                    if #arm_future.is_future() {
                        _ = Future::poll(#arm_future.as_mut(), cx);
                    }
                });
            }
            polling_and_counting.extend(quote! {
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
                    if #arm_future.is_future() && #finished_flag.load(Relaxed) {
                        #arm_future.set(::join_me_maybe::maybe_done::MaybeDone::Gone);
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
