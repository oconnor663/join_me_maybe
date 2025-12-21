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

enum JoinMeMaybeArmKind {
    FutureOnly {
        future: Expr,
    },
    FutureAndBody {
        pattern: syn::Pat,
        future: Expr,
        body: Expr,
    },
    StreamAndBody {
        pattern: syn::Pat,
        stream: Expr,
        body: Expr,
    },
}

struct JoinMeMaybeArm {
    cancel_label: Option<Ident>,
    // "Definitely" is the opposite of "maybe". Previously there was a `definitely` keyword, but it
    // was unnecessarily verbose.
    is_maybe: bool,
    kind: JoinMeMaybeArmKind,
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
        let mut is_future_and_body = false;
        let mut is_stream_and_body = false;
        let fork = input.fork();
        if syn::Pat::parse_single(&fork).is_ok() {
            if fork.peek(syn::Token![=]) {
                is_future_and_body = true;
            } else if fork.peek(syn::Token![in]) {
                is_stream_and_body = true;
            }
        }
        let kind = if is_future_and_body {
            let pattern = syn::Pat::parse_single(input)?;
            _ = input.parse::<syn::Token![=]>()?;
            let future = input.parse()?;
            _ = input.parse::<syn::Token![=>]>()?;
            let body = input.parse()?;
            JoinMeMaybeArmKind::FutureAndBody {
                pattern,
                future,
                body,
            }
        } else if is_stream_and_body {
            let pattern = syn::Pat::parse_single(input)?;
            _ = input.parse::<syn::Token![in]>()?;
            let stream = input.parse()?;
            _ = input.parse::<syn::Token![=>]>()?;
            let body = input.parse()?;
            JoinMeMaybeArmKind::StreamAndBody {
                pattern,
                stream,
                body,
            }
        } else {
            let future = input.parse()?;
            JoinMeMaybeArmKind::FutureOnly { future }
        };
        Ok(Self {
            cancel_label,
            is_maybe,
            kind,
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
            // As with `match` statements, the trailing comma is optional if the arm ends with
            // a block.
            let trailing_comma_optional = matches!(
                arm.kind,
                JoinMeMaybeArmKind::FutureAndBody {
                    body: Expr::Block(_),
                    ..
                } | JoinMeMaybeArmKind::StreamAndBody {
                    body: Expr::Block(_),
                    ..
                }
            );
            arms.push(arm);
            if !input.is_empty() {
                if trailing_comma_optional {
                    // Parse an optional trailing comma if it's there.
                    if input.peek(syn::Token![,]) {
                        let _ = input.parse::<syn::Token![,]>()?;
                    }
                } else {
                    // Parse a mandatory trailing comma.
                    let _ = input.parse::<syn::Token![,]>()?;
                }
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

        // First define the finished flags and cancellers. The finished flags get set to true
        // whenever an arm finishes naturally (poll returns Ready) *or* another arm cancels it.
        let total_definitely = self.arms.iter().filter(|arm| !arm.is_maybe).count();
        let definitely_finished_count =
            format_ident!("definitely_finished_count", span = Span::mixed_site());
        initializers.extend(quote! {
            let #definitely_finished_count = ::core::sync::atomic::AtomicUsize::new(0);
        });
        let finished_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_finished", span = Span::mixed_site()))
            .collect();
        for i in 0..self.arms.len() {
            if let Some(label) = &self.arms[i].cancel_label {
                let flag_name = &finished_flag_names[i];
                initializers.extend(quote! {
                    let #flag_name = ::core::sync::atomic::AtomicBool::new(false);
                });
                if self.arms[i].is_maybe {
                    initializers.extend(quote! {
                        let #label = join_me_maybe::_impl::new_maybe_canceller(
                            &#flag_name,
                        );
                    });
                } else {
                    initializers.extend(quote! {
                        let #label = join_me_maybe::_impl::new_definitely_canceller(
                            &#flag_name,
                            &#definitely_finished_count,
                        );
                    });
                }
            }
        }

        // Now define all the arm futures, which will have the cancellers above in-scope.
        let arm_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}", span = Span::mixed_site()))
            .collect();
        let arm_outputs: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_output", span = Span::mixed_site()))
            .collect();
        for ((arm, arm_name), arm_output) in self.arms.iter().zip(&arm_names).zip(&arm_outputs) {
            match &arm.kind {
                JoinMeMaybeArmKind::FutureOnly { future }
                | JoinMeMaybeArmKind::FutureAndBody { future, .. } => {
                    initializers.extend(quote! {
                        // We can't use `futures::future::Fuse`, because it doesn't expose the
                        // inner future. This version makes the `inner` field `pub`. However,
                        // `futures::stream::Fuse` does expose the inner stream through methods.
                        let mut #arm_name = ::core::pin::pin!(::join_me_maybe::_impl::fuse_future(#future));
                        let mut #arm_output = ::core::option::Option::None;
                    });
                }
                JoinMeMaybeArmKind::StreamAndBody { stream, .. } => {
                    initializers.extend(quote! {
                        let mut #arm_name = ::core::pin::pin!(::join_me_maybe::_impl::fuse_stream(#stream));
                    });
                }
            }
        }

        // Assemble the list of `CancellerMut` declarations. We emit this inside multiple times,
        // inside of each `poll_map`/`poll_for_each` closure.
        let mut cancellermuts = TokenStream2::new();
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            if let Some(label) = &arm.cancel_label {
                cancellermuts.extend(quote! {
                    let mut #label = ::join_me_maybe::_impl::new_canceller_mut(
                        &#label,
                        #arm_name.as_mut().get_pin_mut(),
                    );
                });
            }
        }

        let mut polling_and_counting = TokenStream2::new();
        let poll_output = format_ident!("poll_output", span = Span::mixed_site());
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            let arm_output = &arm_outputs[i];
            let finished_flag = &finished_flag_names[i];
            let poll_is_ready = match &arm.kind {
                JoinMeMaybeArmKind::FutureOnly { .. } => quote! {
                    match ::core::future::Future::poll(#arm_name.as_mut(), cx) {
                        ::core::task::Poll::Ready(#poll_output) => {
                            #arm_output = ::core::option::Option::Some(#poll_output);
                            true
                        }
                        ::core::task::Poll::Pending => false,
                    }
                },
                JoinMeMaybeArmKind::FutureAndBody { pattern, body, .. } => quote! {
                    match ::core::future::Future::poll(#arm_name.as_mut(), cx) {
                        ::core::task::Poll::Ready(#poll_output) => {
                            #arm_output = ::core::option::Option::Some((|#pattern| {
                                #cancellermuts
                                #body
                            })(#poll_output));
                            true
                        }
                        ::core::task::Poll::Pending => false,
                    }
                },
                JoinMeMaybeArmKind::StreamAndBody { pattern, body, .. } => quote! {
                    loop {
                        match ::futures::Stream::poll_next(#arm_name.as_mut(), cx) {
                            ::core::task::Poll::Ready(::core::option::Option::Some(#poll_output)) => {
                                (|#pattern| {
                                    #cancellermuts
                                    #body
                                })(#poll_output);
                            }
                            ::core::task::Poll::Ready(::core::option::Option::None) => break true,
                            ::core::task::Poll::Pending => break false,
                        }
                    }
                },
            };
            if let Some(label) = &arm.cancel_label {
                // If this is a "definitely" future/stream, we need to bump the finished count
                // after it exits, but we don't want to do that unconditionally. The future
                // might've just cancelled itself right before exiting (pointlessly?) and already
                // bumped the count. Calling `cancel` again has no effect on the output but keeps
                // the count consistent.
                polling_and_counting.extend(quote! {
                    if !#finished_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        if #poll_is_ready {
                            #label.cancel();
                        }
                    }
                });
            } else if arm.is_maybe {
                // This is a `maybe` future without a finished/cancelled flag.
                polling_and_counting.extend(quote! {
                    _ = #poll_is_ready;
                });
            } else {
                // This "definitely" future can't be cancelled, so we can unconditionally bump the
                // count when it exits.
                polling_and_counting.extend(quote! {
                    if #poll_is_ready {
                        #definitely_finished_count.store(
                            #definitely_finished_count.load(::core::sync::atomic::Ordering::Relaxed) + 1,
                            ::core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                });
            }
            polling_and_counting.extend(quote! {
                if #definitely_finished_count.load(::core::sync::atomic::Ordering::Relaxed) == #total_definitely {
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
        for ((arm, arm_name), finished_flag) in
            self.arms.iter().zip(&arm_names).zip(&finished_flag_names)
        {
            if arm.cancel_label.is_some() {
                cancelling.extend(quote! {
                    if #finished_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        // No effect if the future is already finished.
                        #arm_name.as_mut().cancel();
                    }
                });
            }
        }

        let mut return_values = TokenStream2::new();
        for (arm, arm_output) in self.arms.iter().zip(&arm_outputs) {
            match &arm.kind {
                JoinMeMaybeArmKind::FutureOnly { .. }
                | JoinMeMaybeArmKind::FutureAndBody { .. } => {
                    if arm.is_maybe || arm.cancel_label.is_some() {
                        // This arm is cancellable. Keep it wrapped in `Option`.
                        return_values.extend(quote! {
                            #arm_output.take(),
                        });
                    } else {
                        // There's no way to cancel this arm without cancelling the whole macro. Unwrap it.
                        return_values.extend(quote! {
                            #arm_output.take().expect("this arm can't be cancelled"),
                        });
                    }
                }
                // Streams don't return anything.
                JoinMeMaybeArmKind::StreamAndBody { .. } => return_values.extend(quote! { (), }),
            }
        }

        tokens.extend(quote! {
            {
                #initializers
                ::core::future::poll_fn(|cx| {
                    // Not really a loop, just a way to short-circuit with `break`.
                    loop {
                        #polling_and_counting
                        // Polling above might `break` and skip cancelling. That's fine, because
                        // everything drops after we return `Ready`.
                        #cancelling
                        // If we don't `break` during polling, we exit here.
                        return ::core::task::Poll::Pending;
                    }
                    // If we `break` during polling, we exit here.
                    ::core::task::Poll::Ready((#return_values))
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
