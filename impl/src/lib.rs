use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, format_ident, quote};
use syn::{
    Expr, Ident,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

mod kw {
    syn::custom_keyword!(maybe);
    syn::custom_keyword!(finally);
}

enum ArmKind {
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
        finally: Option<Expr>,
    },
}

struct JoinMeMaybeArm {
    cancel_label: Option<Ident>,
    // "Definitely" is the opposite of "maybe". Previously there was a `definitely` keyword, but it
    // was unnecessarily verbose.
    is_maybe: bool,
    kind: ArmKind,
}

impl Parse for JoinMeMaybeArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let cancel_label = if input.peek(syn::Ident)
            && input.peek2(syn::Token![:])
            // See `test_potentially_ambiguous_colons`.
            && !input.peek2(syn::Token![::])
        {
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
            ArmKind::FutureAndBody {
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
            let finally = if input.peek(kw::finally) {
                _ = input.parse::<kw::finally>()?;
                Some(input.parse()?)
            } else {
                None
            };
            ArmKind::StreamAndBody {
                pattern,
                stream,
                body,
                finally,
            }
        } else {
            let future = input.parse()?;
            ArmKind::FutureOnly { future }
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
            arms.push(arm);
            // If there's any more input, require a trailing comma first.
            if !input.is_empty() {
                let _ = input.parse::<syn::Token![,]>()?;
            }
        }
        if arms.iter().all(|arm| arm.is_maybe) {
            return Err(input.error(
                "a `join!` with only `maybe` arms returns immediately and executes nothing",
            ));
        }
        Ok(Self { arms })
    }
}

impl ToTokens for JoinMeMaybe {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        // Define the finished flags and cancellers. The finished flags get set to true whenever an
        // arm finishes naturally (poll returns Ready) *or* another arm cancels it.
        let mut initializers = TokenStream2::new();
        let total_definitely = self.arms.iter().filter(|arm| !arm.is_maybe).count();
        let definitely_finished_count =
            format_ident!("definitely_finished_count", span = Span::mixed_site());
        initializers.extend(quote! {
            let #definitely_finished_count = ::core::sync::atomic::AtomicUsize::new(0);
        });
        let finished_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_finished", span = Span::mixed_site()))
            .collect();
        // We need a flag to differentiate "this stream finished and was dropped" from "this stream
        // was cancelled".
        let should_run_finally_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_should_run_finally", span = Span::mixed_site()))
            .collect();
        // Parens are generally necessary here (e.g. for negation) even though the spots where
        // they're unnecessary generate a bunch of warnings in expanded code.
        let definitely_finished = quote! {
            (#definitely_finished_count.load(::core::sync::atomic::Ordering::Relaxed) == #total_definitely)
        };
        for i in 0..self.arms.len() {
            if let Some(label) = &self.arms[i].cancel_label {
                let flag_name = &finished_flag_names[i];
                initializers.extend(quote! {
                    let #flag_name = ::core::sync::atomic::AtomicBool::new(false);
                });
                if self.arms[i].is_maybe {
                    initializers.extend(quote! {
                        let #label = ::join_me_maybe::_impl::new_maybe_canceller(
                            &#flag_name,
                        );
                    });
                } else {
                    initializers.extend(quote! {
                        let #label = ::join_me_maybe::_impl::new_definitely_canceller(
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
        let arm_items: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_item", span = Span::mixed_site()))
            .collect();
        let arm_outputs: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_output", span = Span::mixed_site()))
            .collect();
        for i in 0..self.arms.len() {
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let arm_output = &arm_outputs[i];
            match &self.arms[i].kind {
                ArmKind::FutureOnly { future } => {
                    initializers.extend(quote! {
                        // XXX: We could `pin!` these and use `FusedFuture` or similar to drop them
                        // promptly, but we're already managing `#run_body_future` with `Option`
                        // and `Pin::new_unchecked`, so just do the same here.
                        let mut #arm_name = ::core::option::Option::Some(#future);
                        let mut #arm_output = ::core::option::Option::None;
                    });
                }
                ArmKind::FutureAndBody { future, .. } => {
                    initializers.extend(quote! {
                        let mut #arm_name = ::core::option::Option::Some(#future);
                        let mut #arm_item = ::core::option::Option::None;
                        let mut #arm_output = ::core::option::Option::None;
                    });
                }
                ArmKind::StreamAndBody {
                    stream, finally, ..
                } => {
                    initializers.extend(quote! {
                        let mut #arm_name = ::core::option::Option::Some(#stream);
                        let mut #arm_item = ::core::option::Option::None;
                    });
                    if finally.is_some() {
                        let should_run_finally = &should_run_finally_flag_names[i];
                        initializers.extend(quote! {
                            let mut #should_run_finally = false;
                            let mut #arm_output = ::core::option::Option::None;
                        });
                    }
                }
            }
        }

        // If any arm has a body (or a `finally` expression, but that requires a body), we need to
        // generate a "body future" with a `match` statement in of it, plus an enum to drive that
        // `match`.
        let mut bodies_input_enum_generic_params = TokenStream2::new();
        let mut bodies_input_enum_variants = TokenStream2::new();
        let mut bodies_output_enum_generic_params = TokenStream2::new();
        let mut bodies_output_enum_variants = TokenStream2::new();
        let mut bodies_match_arms = TokenStream2::new();
        let mut has_bodies = false;
        // Mixed-site identifiers can hide variables from the caller, but they can't hide
        // things that have no scope, like a module. Incorporate the crate version into the
        // module name, to make it reasonably private in practice. (A random name would be
        // *really* private, but that would make the build nondeterministic.)
        let private_module_name = format_ident!(
            "__{}_v{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION").replace('.', "_"),
        );
        for (i, arm) in self.arms.iter().enumerate() {
            if let ArmKind::FutureAndBody { pattern, body, .. } = &arm.kind {
                has_bodies = true;
                let param_name = format_ident!("T{i}");
                let variant_name = format_ident!("Arm{i}");
                bodies_input_enum_generic_params.extend(quote! { #param_name, });
                bodies_input_enum_variants.extend(quote! { #variant_name(#param_name), });
                bodies_output_enum_generic_params.extend(quote! { #param_name, });
                bodies_output_enum_variants.extend(quote! { #variant_name(#param_name), });
                bodies_match_arms.extend(quote! {
                    ArmsInput::#variant_name(#pattern) => ArmsOutput::#variant_name(#body),
                });
            }
            if let ArmKind::StreamAndBody {
                pattern,
                body,
                finally,
                ..
            } = &arm.kind
            {
                has_bodies = true;
                let param_name = format_ident!("T{i}");
                let variant_name = format_ident!("Arm{i}");
                bodies_input_enum_generic_params.extend(quote! { #param_name, });
                bodies_input_enum_variants.extend(quote! { #variant_name(#param_name), });
                // Stream bodies always output `()`.
                bodies_output_enum_variants.extend(quote! { #variant_name, });
                bodies_match_arms.extend(quote! {
                    ArmsInput::#variant_name(#pattern) => {
                        let _: () = #body;
                        ArmsOutput::#variant_name
                    }
                });
                if let Some(finally) = finally {
                    let variant_name = format_ident!("Arm{i}Finally");
                    // Stream finally expressions have no input.
                    bodies_input_enum_variants.extend(quote! { #variant_name, });
                    bodies_output_enum_generic_params.extend(quote! { #param_name, });
                    bodies_output_enum_variants.extend(quote! { #variant_name(#param_name), });
                    bodies_match_arms.extend(quote! {
                        ArmsInput::#variant_name => ArmsOutput::#variant_name(#finally),
                    });
                }
            }
        }
        let run_body_fn = format_ident!("run_body_fn", span = Span::mixed_site());
        let run_body_future = format_ident!("run_body_future", span = Span::mixed_site());
        let mut run_body_tokens = TokenStream2::new();
        if has_bodies {
            run_body_tokens.extend(quote! {
                mod #private_module_name {
                    pub enum ArmsInput<#bodies_input_enum_generic_params> {
                        #bodies_input_enum_variants
                    }
                    pub enum ArmsOutput<#bodies_output_enum_generic_params> {
                        #bodies_output_enum_variants
                    }
                }
                let mut #run_body_fn = async |item| {
                    use #private_module_name::{ArmsInput, ArmsOutput};
                    match item {
                        #bodies_match_arms
                    }
                };
                // XXX: Morally we should `pin!` this. However, if we do, then we won't be able to
                // `drop()` it. We need explicit drops below so that the new body future doens't
                // overlap in time with the old one. (With simple assignment, they do overlap,
                // because the compiler needs to be defensive about panics.) This is necessary when
                // the body closure 1) is mutating / AsyncFnMut and 2) needs Drop.
                let mut #run_body_future = ::core::option::Option::None;
            });
        }

        let mut polling_and_counting = TokenStream2::new();
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let arm_should_run_finally = &should_run_finally_flag_names[i];
            let arm_output = &arm_outputs[i];
            let finished_flag = &finished_flag_names[i];
            let poll_is_ready = match &arm.kind {
                ArmKind::FutureOnly { .. } => quote! {
                    if let Some(future) = #arm_name.as_mut() {
                        let pinned = unsafe { ::core::pin::Pin::new_unchecked(future) };
                        match ::join_me_maybe::_impl::PollOnce(pinned).await {
                            ::core::task::Poll::Ready(output) => {
                                #arm_name = None;
                                #arm_output = ::core::option::Option::Some(output);
                                true
                            }
                            ::core::task::Poll::Pending => false,
                        }
                    } else {
                        false
                    }
                },
                ArmKind::FutureAndBody { .. } => quote! {
                    if let Some(future) = #arm_name.as_mut() {
                        let pinned = unsafe { ::core::pin::Pin::new_unchecked(future) };
                        match ::join_me_maybe::_impl::PollOnce(pinned).await {
                            ::core::task::Poll::Ready(item) => {
                                #arm_name = None;
                                #arm_item = ::core::option::Option::Some(item);
                                true
                            }
                            ::core::task::Poll::Pending => false,
                        }
                    } else {
                        false
                    }
                },
                ArmKind::StreamAndBody { finally, .. } => {
                    let set_should_run_finally = if finally.is_some() {
                        quote! {
                            #arm_should_run_finally = true;
                        }
                    } else {
                        quote! {}
                    };
                    quote! {
                        if let Some(stream) = #arm_name.as_mut() {
                            let pinned = unsafe { ::core::pin::Pin::new_unchecked(stream) };
                            match ::join_me_maybe::_impl::PollNextOnce(pinned).await {
                                ::core::task::Poll::Ready(Some(item)) => {
                                    // The has yielded an item, which needs to be consumed by the body.
                                    // We're returning `false` here, because the stream isn't finished,
                                    // but note that we haven't registered a wakeup. If the body
                                    // closure consumes this item, it will rerun the whole top-level
                                    // loop, to give us a chance to poll this stream again. See
                                    // `#item_consumed_from_live_stream`.
                                    #arm_item = ::core::option::Option::Some(item);
                                    false
                                }
                                ::core::task::Poll::Ready(None) => {
                                    // The stream is finished.
                                    #arm_name = None;
                                    #set_should_run_finally
                                    true
                                }
                                ::core::task::Poll::Pending => false,
                            }
                        } else {
                            false
                        }
                    }
                }
            };
            let future_or_stream = match &arm.kind {
                ArmKind::FutureOnly { .. } | ArmKind::FutureAndBody { .. } => {
                    format_ident!("future", span = Span::mixed_site())
                }
                ArmKind::StreamAndBody { .. } => format_ident!("stream", span = Span::mixed_site()),
            };
            // *Always* check the definitely count after each poll, because in general any branch
            // could cancel any other.
            let check_definitely_finished = quote! {
                if #definitely_finished {
                    // Not a real loop break, just a skip-the-rest jump.
                    break;
                }
            };
            if let Some(label) = &arm.cancel_label {
                // If this is a "definitely" future/stream, we need to bump the finished count
                // after it exits, but we don't want to do that unconditionally. The future
                // might've just cancelled itself right before exiting (pointlessly?) and already
                // bumped the count. Calling `cancel` again has no effect on the output but keeps
                // the count consistent.
                polling_and_counting.extend(quote! {
                    if let Some(#future_or_stream) = #arm_name.as_mut() && !#finished_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        let is_ready = #poll_is_ready;
                        if is_ready {
                            #label.cancel();
                        }
                        #check_definitely_finished
                    }
                });
            } else if arm.is_maybe {
                // This is a `maybe` future without a finished/cancelled flag.
                polling_and_counting.extend(quote! {
                    if let Some(#future_or_stream) = #arm_name.as_mut() {
                        _ = #poll_is_ready;
                        #check_definitely_finished
                    }
                });
            } else {
                // This "definitely" future can't be cancelled, so we can unconditionally bump the
                // count when it exits.
                polling_and_counting.extend(quote! {
                    if let Some(#future_or_stream) = #arm_name.as_mut() {
                        let is_ready = #poll_is_ready;
                        if is_ready {
                            #definitely_finished_count.store(
                                #definitely_finished_count.load(::core::sync::atomic::Ordering::Relaxed) + 1,
                                ::core::sync::atomic::Ordering::Relaxed,
                            );
                        }
                        #check_definitely_finished
                    }
                });
            }
        }

        // When a future gets cancelled, that means two thing. First, the obvious one, it shouldn't
        // ever get polled again. But second -- and it's easy to miss this part -- it needs to get
        // *dropped promptly*. Consider a case where one arm is holding an async lock, and another
        // arm is trying to acquire it. If the first arm is cancelled but not dropped, then the
        // second arm will deadlock. See "Futurelock": https://rfd.shared.oxide.computer/rfd/0609.
        let mut cancel_all = TokenStream2::new();
        let mut cancel_labeled = TokenStream2::new();
        for ((arm, arm_name), finished_flag) in
            self.arms.iter().zip(&arm_names).zip(&finished_flag_names)
        {
            cancel_all.extend(quote! {
                #arm_name = None;
            });
            if arm.cancel_label.is_some() {
                cancel_labeled.extend(quote! {
                    if #finished_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        #arm_name = None;
                    }
                });
            }
        }
        let cancelling = quote! {
            if #definitely_finished {
                #cancel_all
            } else {
                #cancel_labeled
            }
        };

        // The run bodies loop. As long as there are items available, and we don't have an existing
        // `#run_body_future` that's returned `Pending`, keep trying to consume items.
        let mut try_to_call_run_body = TokenStream2::new();
        let mut handle_body_output_arms = TokenStream2::new();
        // We need to keep looping without returning `Pending` if running bodies might've unblocked
        // some of the scrutinees. This can happen when an item is consumed from a stream.
        let item_consumed_from_live_stream =
            format_ident!("item_consumed_from_live_stream", span = Span::mixed_site());
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let arm_output = &arm_outputs[i];
            if let ArmKind::FutureAndBody { .. } = &arm.kind {
                let variant_name = format_ident!("Arm{i}");
                try_to_call_run_body.extend(quote! {
                    if let Some(item) = #arm_item.take() {
                        // Drop-then-assign means that the old value and the new value don't
                        // overlap, which actually isn't the case for simple assignment (because
                        // the compiler has to be defensive about panics). This is necessary when
                        // the body closure 1) is mutating / AsyncFnMut and 2) needs Drop.
                        drop(#run_body_future);
                        #run_body_future = Some(#run_body_fn(
                            #private_module_name::ArmsInput::#variant_name(item)
                        ));
                        continue; // Loop again to poll this.
                    }
                });
                handle_body_output_arms.extend(quote! {
                    #private_module_name::ArmsOutput::#variant_name(output) => {
                        #arm_output = ::core::option::Option::Some(output);
                    }
                });
            }
            if let ArmKind::StreamAndBody { finally, .. } = &arm.kind {
                let variant_name = format_ident!("Arm{i}");
                try_to_call_run_body.extend(quote! {
                    if let Some(item) = #arm_item.take() {
                        // See above about `drop`.
                        drop(#run_body_future);
                        #run_body_future = Some(#run_body_fn(
                            #private_module_name::ArmsInput::#variant_name(item)
                        ));
                        if #arm_name.is_some() {
                            #item_consumed_from_live_stream = true;
                        }
                        continue; // Loop again to poll this.
                    }
                });
                handle_body_output_arms.extend(quote! {
                    #private_module_name::ArmsOutput::#variant_name => {}
                });
                if finally.is_some() {
                    let variant_name = format_ident!("Arm{i}Finally");
                    let arm_should_run_finally = &should_run_finally_flag_names[i];
                    try_to_call_run_body.extend(quote! {
                        // Note that we just checked `#arm_item` above.
                        if #arm_should_run_finally {
                            // See above about `drop`.
                            drop(#run_body_future);
                            #run_body_future = Some(#run_body_fn(
                                #private_module_name::ArmsInput::#variant_name
                            ));
                            #arm_should_run_finally = false;
                            continue; // Loop again to poll this.
                        }
                    });
                    handle_body_output_arms.extend(quote! {
                        #private_module_name::ArmsOutput::#variant_name(output) => {
                            #arm_output = ::core::option::Option::Some(output);
                        }
                    });
                }
            }
        }
        let mut run_bodies_loop = TokenStream2::new();
        if has_bodies {
            run_bodies_loop.extend(quote! {
                loop {
                    if let Some(future) = #run_body_future.as_mut() {
                        let poll = ::join_me_maybe::_impl::PollOnce(unsafe {
                            ::core::pin::Pin::new_unchecked(future)
                        }).await;
                        if let ::core::task::Poll::Ready(output) = poll {
                            // See above about `drop`.
                            drop(#run_body_future);
                            #run_body_future = None;
                            match output {
                                #handle_body_output_arms
                            }
                        } else {
                            break;
                        }
                    }
                    #try_to_call_run_body
                    if #run_body_future.is_none() {
                        // There are no more items.
                        break;
                    }
                }
            });
        }

        let mut return_values = TokenStream2::new();
        for (arm, arm_output) in self.arms.iter().zip(&arm_outputs) {
            match &arm.kind {
                ArmKind::FutureOnly { .. }
                | ArmKind::FutureAndBody { .. }
                | ArmKind::StreamAndBody {
                    finally: Some(_), ..
                } => {
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
                // Streams without `finally`, don't return anything.
                ArmKind::StreamAndBody { finally: None, .. } => {
                    return_values.extend(quote! { (), })
                }
            }
        }

        let finished_check = if has_bodies {
            quote! { #definitely_finished && #run_body_future.is_none() }
        } else {
            quote! { #definitely_finished }
        };
        tokens.extend(quote! {
            {
                #initializers
                #run_body_tokens
                loop {
                    if !#definitely_finished {
                        // Not really another loop, just a way to short-circuit polling with `break` if all
                        // the "definitely" atrms finish in the middle.
                        loop {
                            #polling_and_counting
                            break;
                        }
                        #cancelling
                    }
                    let mut #item_consumed_from_live_stream = false;
                    #run_bodies_loop
                    if #finished_check {
                        // We are DONE!
                        break (#return_values);
                    } else if #definitely_finished || !#item_consumed_from_live_stream {
                        // If running bodies didn't unblock any of the scrutinees (either because
                        // we're done running them, or because no items were consumed from any
                        // streams), then we can't make further progress right now, and we need to
                        // yield.
                        ::futures::pending!();
                    }
                    // Loop again (either immediately, if we've potentially unblocked a scrutinee,
                    // or after being woken up, if we just yielded).
                }
            }
        });
    }
}

#[proc_macro]
pub fn join(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let c = parse_macro_input!(input as JoinMeMaybe);
    quote! { #c }.into()
}
