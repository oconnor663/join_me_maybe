use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, format_ident, quote};
use std::collections::HashSet;
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
                // An error I've made before is to write something like this:
                // ```
                // join!(
                //     async { ... } => { ... }
                // ```
                // That is, trying to put a body after a future without capturing its output. This
                // is not currently allowed, because it's ambiguous whether the scrutinee is
                // supposed to be a future or a stream. Make sure that gets a clear error message.
                if input.peek(syn::Token![=>]) {
                    return Err(input.error(
                        "`[fut] => [body]` is not allowed, you must write `[output] = [fut] => [body]`",
                    ));
                }
                let _ = input.parse::<syn::Token![,]>()?;
            }
        }
        if arms.iter().all(|arm| arm.is_maybe) {
            return Err(input.error(
                "a `join!` with only `maybe` arms returns immediately and executes nothing",
            ));
        }
        let mut unique_idents = HashSet::new();
        for arm in &arms {
            if let Some(label) = &arm.cancel_label {
                if !unique_idents.insert(label) {
                    return Err(syn::Error::new_spanned(
                        label,
                        format!("the label `{}` is used more than once", label),
                    ));
                }
            }
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
        // Finished flags are set whenever a "labeled definitely" stream/future completes, either
        // naturally or by being cancelled. We use these to make sure the
        // `definitely_finished_count` gets bumped exactly once for each. ("Unlabeled definitely"
        // arms always bump the count when they finish, but labeled definitely arms need to be
        // defensive about cancelling themselves right before finishing.) Note that even if
        // finished is set, the corresponding body/finally might still be running. Tracking things
        // this way lets us cancel a "maybe" body as soon as all the "definitely" futures/streams
        // complete, without waiting for all the "definitely" bodies, which is important because
        // only one body runs at a time. (Otherwise a long-running "maybe" body could delay exit by
        // preventing execution of the "definitely" bodies.)
        let finished_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_finished", span = Span::mixed_site()))
            .collect();
        // We need a flag to differentiate "this stream finished and was dropped" from "this stream
        // was cancelled".
        let should_run_finally_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_should_run_finally", span = Span::mixed_site()))
            .collect();
        // Cancelled flags are set if a labeled arm is explicitly cancelled. For "labeled
        // definitely" arms, the corresponding finished flag is also set. Setting the cancelled
        // flag also cancels the corresponding body/finally if it's already running.
        let cancelled_flag_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_cancelled", span = Span::mixed_site()))
            .collect();
        // Parens are generally necessary here (e.g. for negation) even though the spots where
        // they're unnecessary generate a bunch of warnings in expanded code.
        let definitely_finished = quote! {
            (#definitely_finished_count.load(::core::sync::atomic::Ordering::Relaxed) == #total_definitely)
        };
        for i in 0..self.arms.len() {
            if let Some(label) = &self.arms[i].cancel_label {
                let cancelled_flag_name = &cancelled_flag_names[i];
                initializers.extend(quote! {
                    let #cancelled_flag_name = ::core::sync::atomic::AtomicBool::new(false);
                });
                if self.arms[i].is_maybe {
                    initializers.extend(quote! {
                        #[allow(unused_variables)]
                        let #label = &::join_me_maybe::_impl::new_canceller(
                            &#cancelled_flag_name,
                            None,
                            &#definitely_finished_count,
                        );
                    });
                } else {
                    let finished_flag_name = &finished_flag_names[i];
                    initializers.extend(quote! {
                        let #finished_flag_name = ::core::sync::atomic::AtomicBool::new(false);
                    });
                    initializers.extend(quote! {
                        #[allow(unused_variables)]
                        let #label = &::join_me_maybe::_impl::new_canceller(
                            &#cancelled_flag_name,
                            ::core::option::Option::Some(&#finished_flag_name),
                            &#definitely_finished_count,
                        );
                    });
                }
            }
        }

        // Now define all the arm futures, which will have references to the cancellers above
        // in-scope.
        let arm_names: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}", span = Span::mixed_site()))
            .collect();
        let arm_items: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_item", span = Span::mixed_site()))
            .collect();
        let arm_outputs: Vec<_> = (0..self.arms.len())
            .map(|i| format_ident!("arm_{i}_output", span = Span::mixed_site()))
            .collect();
        let mut arm_pins = Vec::new(); // Pin<&mut Option<T>>
        for i in 0..self.arms.len() {
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let arm_output = &arm_outputs[i];
            // All futures/streams get pinned in place, and inside an `Option` too so that we can
            // drop them. But labeled ones get further wrapped (i.e. the `Pin<&mut Option<T>>` is
            // wrapped) in an `AtomicRefCell`.
            let mut pin_and_wrap = |expr| {
                if self.arms[i].cancel_label.is_some() {
                    arm_pins.push(quote! { #arm_name.borrow_mut() });
                    quote! {
                        let #arm_name = ::core::pin::pin!(::core::option::Option::Some(#expr));
                        let #arm_name = ::join_me_maybe::_impl::AtomicRefCell::new(#arm_name);
                    }
                } else {
                    arm_pins.push(quote! { #arm_name });
                    quote! {
                        let mut #arm_name = ::core::pin::pin!(::core::option::Option::Some(#expr));
                    }
                }
            };
            match &self.arms[i].kind {
                ArmKind::FutureOnly { future } => {
                    let pinned_and_wrapped = pin_and_wrap(future);
                    initializers.extend(quote! {
                        #pinned_and_wrapped
                        let mut #arm_output = ::core::option::Option::None;
                    });
                }
                ArmKind::FutureAndBody { future, .. } => {
                    let pinned_and_wrapped = pin_and_wrap(future);
                    initializers.extend(quote! {
                        #pinned_and_wrapped
                        let mut #arm_item = ::core::option::Option::None;
                        let mut #arm_output = ::core::option::Option::None;
                    });
                }
                ArmKind::StreamAndBody {
                    stream, finally, ..
                } => {
                    let pinned_and_wrapped = pin_and_wrap(stream);
                    initializers.extend(quote! {
                        #pinned_and_wrapped
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
        // generate a "body future", a `match` statement for its body, and an input enum to drive
        // that `match`.
        let mut bodies_input_enum_generic_params = TokenStream2::new();
        let mut bodies_input_enum_variants = TokenStream2::new();
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
        let output_temporary = format_ident!("_output");
        for (i, arm) in self.arms.iter().enumerate() {
            let arm_output = &arm_outputs[i];
            if let ArmKind::FutureAndBody { pattern, body, .. } = &arm.kind {
                has_bodies = true;
                let param_name = format_ident!("T{i}");
                let variant_name = format_ident!("Arm{i}");
                bodies_input_enum_generic_params.extend(quote! { #param_name, });
                bodies_input_enum_variants.extend(quote! { #variant_name(#param_name), });
                bodies_match_arms.extend(quote! {
                    // Suppress unreachable code warnings if the #body returns.
                    #private_module_name::ArmsInput::#variant_name(#pattern) => {
                        let #output_temporary = {
                            #body
                        };
                        #[allow(unreachable_code)]
                        {
                            #arm_output = ::core::option::Option::Some(#output_temporary);
                        }
                    }
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
                bodies_match_arms.extend(quote! {
                    #private_module_name::ArmsInput::#variant_name(#pattern) => {
                        let _: () = #body;
                    }
                });
                if let Some(finally) = finally {
                    let variant_name = format_ident!("Arm{i}Finally");
                    // Stream finally expressions have no input.
                    bodies_input_enum_variants.extend(quote! { #variant_name, });
                    bodies_match_arms.extend(quote! {
                        #private_module_name::ArmsInput::#variant_name => {
                            let #output_temporary = {
                                #finally
                            };
                            #[allow(unreachable_code)]
                            {
                                #arm_output = ::core::option::Option::Some(#output_temporary);
                            }
                        }
                    });
                }
            }
        }
        let run_body_fn = format_ident!("run_body_fn", span = Span::mixed_site());
        let run_body_future = format_ident!("run_body_future", span = Span::mixed_site());
        let run_body_no_return = format_ident!("run_body_no_return", span = Span::mixed_site());
        let body_is_maybe = format_ident!("body_is_maybe", span = Span::mixed_site());
        let body_cancelled_flag = format_ident!("body_cancelled_flag", span = Span::mixed_site());
        let mut run_body_tokens = TokenStream2::new();
        if has_bodies {
            let mut canceller_muts = TokenStream2::new();
            for i in 0..self.arms.len() {
                if let Some(label) = &self.arms[i].cancel_label {
                    let arm_name = &arm_names[i];
                    let cancelled_flag = &cancelled_flag_names[i];
                    if self.arms[i].is_maybe {
                        canceller_muts.extend(quote! {
                            #[allow(unused_variables)]
                            let #label = &::join_me_maybe::_impl::new_canceller_mut(
                                &#cancelled_flag,
                                None,
                                &#definitely_finished_count,
                                &#arm_name,
                            );
                        });
                    } else {
                        let finished_flag = &finished_flag_names[i];
                        canceller_muts.extend(quote! {
                            #[allow(unused_variables)]
                            let #label = &::join_me_maybe::_impl::new_canceller_mut(
                                &#cancelled_flag,
                                ::core::option::Option::Some(&#finished_flag),
                                &#definitely_finished_count,
                                &#arm_name,
                            );
                        });
                    }
                }
            }
            let item = format_ident!("item", span = Span::mixed_site());
            run_body_tokens.extend(quote! {
                mod #private_module_name {
                    pub enum ArmsInput<#bodies_input_enum_generic_params> {
                        #bodies_input_enum_variants
                    }
                }
                #canceller_muts
                let #run_body_no_return = ::core::sync::atomic::AtomicBool::new(false);
                let mut #run_body_fn = async |#item| {
                    match #item {
                        #bodies_match_arms
                    }
                    // If one of the arms short-circuits with `return` or `?`, that becomes the
                    // output of `#run_body_future`, and we'll collect it when we poll. If not,
                    // we'll set the no_return flag (which we can read without dropping the
                    // `#run_body_future`) and block forever on `pending` (to avoid needing to
                    // conjure up a return value of the right type).
                    #[allow(unreachable_code)]
                    {
                        #run_body_no_return.store(true, ::core::sync::atomic::Ordering::Relaxed);
                        ::core::future::pending().await
                    }
                };
                // XXX: Morally we should `pin!` this. However, if we do, then we won't be able to
                // `drop()` it. We need explicit drops below so that the new body future doesn't
                // overlap in time with the old one. (With simple assignment, they do overlap,
                // because the compiler needs to be defensive about panics.) This is necessary when
                // the body closure 1) is mutating / AsyncFnMut and 2) needs Drop.
                let mut #run_body_future = ::core::option::Option::None;
                let mut #body_is_maybe = false;
                let mut #body_cancelled_flag: ::core::option::Option<&::core::sync::atomic::AtomicBool> = ::core::option::Option::None;
            });
        }

        let mut polling_and_counting = TokenStream2::new();
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let arm_should_run_finally = &should_run_finally_flag_names[i];
            let arm_output = &arm_outputs[i];
            let cancelled_flag = &cancelled_flag_names[i];
            let finished_flag = &finished_flag_names[i];
            let future_or_stream = match &arm.kind {
                ArmKind::FutureOnly { .. } | ArmKind::FutureAndBody { .. } => {
                    format_ident!("future", span = Span::mixed_site())
                }
                ArmKind::StreamAndBody { .. } => format_ident!("stream", span = Span::mixed_site()),
            };
            let poll_is_ready = match &arm.kind {
                ArmKind::FutureOnly { .. } | ArmKind::FutureAndBody { .. } => {
                    let handle_output = if matches!(arm.kind, ArmKind::FutureOnly { .. }) {
                        quote! { #arm_output = ::core::option::Option::Some(output); }
                    } else {
                        quote! { #arm_item = ::core::option::Option::Some(output); }
                    };
                    quote! {
                        match ::join_me_maybe::_impl::PollOnce(#future_or_stream).await {
                            ::core::task::Poll::Ready(output) => {
                                #handle_output
                                true
                            }
                            ::core::task::Poll::Pending => false,
                        }
                    }
                }
                ArmKind::StreamAndBody { finally, .. } => {
                    let set_should_run_finally = if finally.is_some() {
                        quote! {
                            #arm_should_run_finally = true;
                        }
                    } else {
                        quote! {}
                    };
                    quote! {
                        match ::join_me_maybe::_impl::PollNextOnce(#future_or_stream).await {
                            ::core::task::Poll::Ready(::core::option::Option::Some(item)) => {
                                // The stream has yielded an item, which needs to be consumed by
                                // the body. We're returning `false` here, because the stream isn't
                                // finished, but note that we haven't registered a wakeup. If the
                                // body closure consumes this item, it will rerun the whole
                                // top-level loop, to give us a chance to poll this stream again.
                                // See `#item_consumed_from_live_stream`.
                                #arm_item = ::core::option::Option::Some(item);
                                false
                            }
                            ::core::task::Poll::Ready(None) => {
                                // The stream is finished.
                                #set_should_run_finally
                                true
                            }
                            ::core::task::Poll::Pending => false,
                        }
                    }
                }
            };
            // *Always* check the definitely count after each poll, because in general any branch
            // could cancel any other.
            let check_definitely_finished = quote! {
                if #definitely_finished {
                    // Not a real loop break, just a skip-the-rest jump.
                    break;
                }
            };
            if arm.cancel_label.is_some() {
                let bump_count = if !arm.is_maybe {
                    quote! {
                        // If this just-finished, labeled future/stream is a "definitely" arm,
                        // we need to bump the count, but we don't want to do that
                        // unconditionally. It might've just cancelled itself right before
                        // exiting (pointlessly?) and already bumped the count. It would be
                        // convenient to just call `.cancel()` here, but we don't want to set
                        // the cancelled flag, because we want the body/finally (if any) to
                        // run. Repeat a similar check here.
                        let already_finished = #finished_flag.swap(true, ::core::sync::atomic::Ordering::Relaxed);
                        if !already_finished {
                            #definitely_finished_count.fetch_add(1, ::core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                } else {
                    quote! {}
                };
                polling_and_counting.extend(quote! {
                    if !#cancelled_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        let mut guard = #arm_name.borrow_mut();
                        let is_ready = if let ::core::option::Option::Some(#future_or_stream) = guard.as_mut().as_pin_mut() {
                            #poll_is_ready
                        } else {
                            false
                        };
                        if is_ready {
                            guard.set(::core::option::Option::None);
                            #bump_count
                        }
                        #check_definitely_finished
                    }
                });
            } else {
                // An unlabeled "definitely" future/stream can't be cancelled (other than by
                // cancelling the whole macro), so we unconditionally bump the count when it exits.
                let bump_count = if !arm.is_maybe {
                    quote! {
                        #definitely_finished_count.fetch_add(1, ::core::sync::atomic::Ordering::Relaxed);
                    }
                } else {
                    quote! {}
                };
                polling_and_counting.extend(quote! {
                    let is_ready = if let ::core::option::Option::Some(#future_or_stream) = #arm_name.as_mut().as_pin_mut() {
                        #poll_is_ready
                    } else {
                        false
                    };
                    if is_ready {
                        #arm_name.set(::core::option::Option::None);
                        #bump_count
                    }
                    #check_definitely_finished
                });
            }
        }

        // When a future gets cancelled, that means two thing. First, the obvious one, it shouldn't
        // ever get polled again. But second -- and it's easy to miss this part -- it needs to get
        // *dropped promptly*. Consider a case where one arm is holding an async lock, and another
        // arm is trying to acquire it. If the first arm is cancelled but not dropped, then the
        // second arm will deadlock. See "Futurelock": https://rfd.shared.oxide.computer/rfd/0609.
        let mut drop_maybe = TokenStream2::new();
        let mut drop_labeled = TokenStream2::new();
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let cancelled_flag = &cancelled_flag_names[i];
            let arm_pin = &arm_pins[i];
            // An even subtler version of the same rule is that arm *items* must also get dropped
            // promptly. These are the ready values of future arms with bodies, or the items
            // yielded from streams. These need to get dropped as soon as we know the corresponding
            // body will never run. It's rare that this matters, but the value of a future could be
            // for example a `MutexGuard`.
            let mut drop_item = TokenStream2::new();
            if matches!(
                arm.kind,
                ArmKind::FutureAndBody { .. } | ArmKind::StreamAndBody { .. },
            ) {
                let arm_item = &arm_items[i];
                drop_item.extend(quote! {
                    #arm_item = None;
                });
            }
            if arm.is_maybe {
                drop_maybe.extend(quote! {
                    #arm_pin.set(::core::option::Option::None);
                    #drop_item
                });
            }
            if self.arms[i].cancel_label.is_some() {
                drop_labeled.extend(quote! {
                    if #cancelled_flag.load(::core::sync::atomic::Ordering::Relaxed) {
                        #arm_pin.set(::core::option::Option::None);
                        #drop_item
                    }
                });
            }
        }
        let drop_cancelled = quote! {
            if #definitely_finished {
                #drop_maybe
            }
            #drop_labeled
        };

        // The run bodies loop. As long as there are items available, and we don't have an existing
        // `#run_body_future` that's returned `Pending`, keep trying to consume items.
        let mut try_to_call_run_body = TokenStream2::new();
        // We need to keep looping without returning `Pending` if running bodies might've unblocked
        // some of the scrutinees. This can happen when an item is consumed from a stream.
        let item_consumed_from_live_stream =
            format_ident!("item_consumed_from_live_stream", span = Span::mixed_site());
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let arm_name = &arm_names[i];
            let arm_item = &arm_items[i];
            let body_is_maybe_value = arm.is_maybe;
            let body_cancelled_flag_value = if arm.cancel_label.is_some() {
                let cancelled_flag = &cancelled_flag_names[i];
                quote! { ::core::option::Option::Some(&#cancelled_flag) }
            } else {
                quote! { None }
            };
            let set_body_flags_and_continue = quote! {
                #body_is_maybe = #body_is_maybe_value;
                #body_cancelled_flag = #body_cancelled_flag_value;
                continue; // Loop again to poll this.
            };
            if let ArmKind::FutureAndBody { .. } = &arm.kind {
                let variant_name = format_ident!("Arm{i}");
                try_to_call_run_body.extend(quote! {
                    if let ::core::option::Option::Some(item) = #arm_item.take() {
                        // Drop-then-assign means that the old value and the new value don't
                        // overlap, which actually isn't the case for simple assignment (because
                        // the compiler has to be defensive about panics). This is necessary when
                        // the body closure 1) is mutating / AsyncFnMut and 2) needs Drop.
                        // SAFETY: We are using `Pin::new_unchecked` with this value, so we're
                        // morally obligated to drop it in-place. Calling `drop()` technically
                        // moves the value into drop, which isn't allowed, even though the compiler
                        // almost certainly elides the move. Instead of relying on that, explicitly
                        // overwrite the value with `None` first, even though it feels redundant.
                        // (In summary, `... = None` is for soundness, and `drop` is for borrowck.)
                        #run_body_future = None;
                        drop(#run_body_future);
                        #run_body_future = ::core::option::Option::Some(#run_body_fn(#private_module_name::ArmsInput::#variant_name(item)));
                        #set_body_flags_and_continue
                    }
                });
            }
            if let ArmKind::StreamAndBody { finally, .. } = &arm.kind {
                let variant_name = format_ident!("Arm{i}");
                let is_live = if arm.cancel_label.is_some() {
                    if arm.is_maybe {
                        // Maybe arms don't have a finished flag. Just inspect the AtomicRefCell
                        // itself.
                        quote! { #arm_name.borrow().is_some() }
                    } else {
                        let finished_flag = &finished_flag_names[i];
                        quote! { !#finished_flag.load(::core::sync::atomic::Ordering::Relaxed) }
                    }
                } else {
                    quote! { #arm_name.is_some() }
                };
                try_to_call_run_body.extend(quote! {
                    if let ::core::option::Option::Some(item) = #arm_item.take() {
                        // SAFETY: See above about `None` and `drop`.
                        #run_body_future = None;
                        drop(#run_body_future);
                        #run_body_future = ::core::option::Option::Some(#run_body_fn(#private_module_name::ArmsInput::#variant_name(item)));
                        if #is_live {
                            #item_consumed_from_live_stream = true;
                        }
                        #set_body_flags_and_continue
                    }
                });
                if finally.is_some() {
                    let variant_name = format_ident!("Arm{i}Finally");
                    let arm_should_run_finally = &should_run_finally_flag_names[i];
                    try_to_call_run_body.extend(quote! {
                        // Note that we just checked `#arm_item` above.
                        if #arm_should_run_finally {
                            // SAFETY: See above about `None` and `drop`.
                            #run_body_future = None;
                            drop(#run_body_future);
                            #run_body_future = ::core::option::Option::Some(#run_body_fn(#private_module_name::ArmsInput::#variant_name));
                            #arm_should_run_finally = false;
                            #set_body_flags_and_continue
                        }
                    });
                }
            }
        }
        let mut run_bodies_loop = TokenStream2::new();
        if has_bodies {
            let body_is_cancelled = quote! {
                ((#body_is_maybe && #definitely_finished)
                    || #body_cancelled_flag.is_some_and(|flag| flag.load(::core::sync::atomic::Ordering::Relaxed)))
            };
            run_bodies_loop.extend(quote! {
                loop {
                    // Check whether the currently running body (if any) is cancelled. We need to
                    // do this in the loop, rather than just once before entering the loop, because
                    // we don't check this in #try_to_call_run_body.
                    if #run_body_future.is_some() && #body_is_cancelled {
                        // Drop-then-assign means that the old value and the new value don't overlap,
                        // which actually isn't the case for simple assignment (because the compiler
                        // has to be defensive about panics). This is necessary when the body closure
                        // 1) is mutating / AsyncFnMut and 2) needs Drop.
                        // SAFETY: We are using `Pin::new_unchecked` with this value, so we're
                        // obligated to drop it in-place. Calling `drop()` technically moves the value
                        // into drop, which isn't allowed, even though the compiler almost certainly
                        // elides the move. Instead of relying on that, explicitly overwrite the value
                        // with `None` first, even though it feels redundant. (In summary, `... = None`
                        // is for soundness, `drop` is for borrowck, and the second `... = None` is to
                        // keep the variable initialized.)
                        #run_body_future = None;
                        drop(#run_body_future);
                        #run_body_future = None;
                    }
                    if let ::core::option::Option::Some(future) = #run_body_future.as_mut() {
                        let poll = ::join_me_maybe::_impl::PollOnce(unsafe {
                            ::core::pin::Pin::new_unchecked(future)
                        }).await;
                        if let ::core::task::Poll::Ready(output) = poll {
                            // The body closure diverged with `return` or `?`. Propagate that into
                            // a return from the calling function. The rest of the macro is
                            // cancelled.
                            return output;
                        } else if #run_body_no_return.load(::core::sync::atomic::Ordering::Relaxed) {
                            // Execution of the body closure reached the end of caller code (did
                            // not diverge), it set the `#run_body_no_return` flag, and then it
                            // blocked forever on `core::future::pending`. Clear that flag and then
                            // proceed to try to run another body.
                            #run_body_no_return.store(false, ::core::sync::atomic::Ordering::Relaxed);
                        } else if #body_is_cancelled {
                            // The body cancelled *itself*, either directly or by cancelling the
                            // last of the "definitely" arms. Again proceed to try to run another
                            // body.
                        } else {
                            // The body is pending and not cancelled, and presumably it's
                            // registered a wakeup. End the run bodies loop. (The outermost loop
                            // might repeat, though, if we've unblocked some streams.)
                            break;
                        }
                    }
                    // SAFETY: See above about `None` and `drop`.
                    #run_body_future = None;
                    drop(#run_body_future);
                    #run_body_future = None;
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

        let finished_check;
        let drop_run_body_fn;
        if has_bodies {
            finished_check = quote! { #definitely_finished && #run_body_future.is_none() };
            drop_run_body_fn = quote! {
                // These borrow the output variables, and we need to unwrap those.
                // SAFETY: See above about `None` and `drop`.
                #run_body_future = None;
                drop(#run_body_future);
                // The closure itself doesn't implement Drop, so it's not strictly necessary to
                // drop it explicitly, but I think it's clearer.
                drop(#run_body_fn);
            };
        } else {
            finished_check = definitely_finished.clone();
            drop_run_body_fn = quote! {}
        };
        tokens.extend(quote! {
            {
                #initializers
                #run_body_tokens
                loop {
                    if !#definitely_finished {
                        // Not really another loop, just a way to short-circuit polling with `break` if all
                        // the "definitely" arms finish in the middle.
                        loop {
                            #polling_and_counting
                            break;
                        }
                    }
                    // Drop any cancelled scrutinees (either labeled cancellation, or "maybe" arms
                    // after the "definitely" scrutinees have finished). Also drop any pending
                    // items belonging to those arms. If a cancelled body is currently running, it
                    // will drop in #run_bodies_loop below.
                    #drop_cancelled
                    let mut #item_consumed_from_live_stream = false;
                    #run_bodies_loop
                    if #finished_check {
                        // We are DONE!
                        #drop_run_body_fn
                        break (#return_values);
                    } else if !#item_consumed_from_live_stream {
                        // If running bodies didn't unblock any of the scrutinees (either because
                        // we're done running them, or because no items were consumed from any
                        // streams), then we can't make further progress right now, and we need to
                        // yield.
                        ::join_me_maybe::_impl::yield_once().await;
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
