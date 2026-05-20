# Agent Notes

This crate is a Rust proc-macro implementation of `join_me_maybe::join!`. Most
interesting bugs are state-machine bugs in generated code, especially around
cancellation, body scheduling, stream item buffering, and dropping things
promptly.

## Local Workflow

- Use tight timeouts when running tests that may deadlock, for example:
  `timeout 5s cargo test <test-name> -- --test-threads=1 --nocapture`. If you
  know you're running a single test, 1s is almost always enough.
- For macro expansion debugging, use `scripts/expand-example-scratch`. It writes
  scratch crates under `target/expand-scratch/`, adds the needed nightly feature
  gates, and keeps generated files out of normal Cargo builds.

## Semantics To Preserve

- Cancelled arms (both scrutinees and body/finally blocks) must not be polled
  again. This applies to both forms of cancellation, explicit `.cancel()` calls
  and also "maybe" arms after the "definitely" arms are finished.
- Cancelled arms, running bodies/finally expressions, and pending yielded items
  must be dropped before the macro yields back to its caller.
- Arm bodies run one at a time and may mutate shared local state.
- Streams are not polled concurrently with their own body. This is intentional.
- `finally` runs after natural stream completion, but not after cancellation.
- `maybe` arms are cancelled once all definitely scrutinees are finished; this
  does not require waiting for definitely bodies/finally expressions.

## Not Bugs

- `.cancel()` does not synchronously drop the target arm. It sets cancellation
  flags; cleanup happens at the macro's cleanup point before yielding to the
  caller. Do not add asserts that a cancelled scrutinee immediately appear as
  `None` via `with_pin_mut` or that e.g. `Mutex::try_lock` in another arm
  immediately succeeds.
- The same rule applies to implicit `maybe` cancellation after all definitely
  scrutinees finish. A definitely body may run before the cancelled maybe
  scrutinee has been physically dropped, as long as cleanup happens before the
  macro yields to its caller.
- Self-cancellation is allowed. An arm may call its own `.cancel()` and continue
  executing until its next `.await`; the implementation must not try to
  synchronously borrow/drop the currently executing arm in a way that panics.
- We don't currently force yields to the caller if the user provides an
  always-ready stream. In theory we could (if we invoked our own waker before
  yielding), but this is not currently considered a bug.
- If something puts a pending future or stream into a state where it's able to
  make progress, but it doesn't invoke a waker, it's not our fault if that
  leads to a hang. It's mandatory that futures invoke wakers when it's time to
  poll them, and there's no practical way we can work around it if they don't.
  (E.g. you could set some global flag to unpause your future/stream, and
  there's no way for us to know that you did that.)
