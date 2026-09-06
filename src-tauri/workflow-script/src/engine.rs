//! rquickjs engine adapter for `ScriptSource` and the QuickJS script-to-host value boundary.
//!
//! # Threading
//!
//! `Runtime` and `Context` are not `Send`. Construct and use the engine only on
//! the driver worker thread; the dispatch thread performs only startup validation
//! with a disposable engine.
//!
//! # Synchronous time slice
//!
//! Each [`StepSource::advance`] installs an interrupt handler. Synchronous JavaScript
//! (boot-level code and each uninterrupted microtask segment) exceeding
//! [`crate::SCRIPT_SYNC_SLICE_MS`] fails the run. The driver's loop-level deadline
//! cannot interrupt a worker spinning in JavaScript.
//!
//! # Step identity
//!
//! The zero-based Nth `agent()` call returns the Promise for global driver index N.
//! Requests retain emission order in the returned vector, and settled results return
//! to their Promise by `StepOutcome::index`.

// Object identity uses the raw JSValue pointer so the boundary's visited table can
// preserve cycles and shared references. rquickjs has no safe pointer accessor; this
// unsafe block reads only the tag union pointer and never dereferences it.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::{
    Array, CatchResultExt, Context, Ctx, Exception, Function, Object, Persistent, Runtime, Type,
    Value,
};
use serde_json::Value as JsonValue;
use workflow_core::boundary::{
    clone_in, clone_out, graph_from_json, graph_to_json, SinkBuilder, SourceKind, SourceValue,
};
use workflow_core::{
    StepOutcome, StepProgress, StepRolePolicy, StepSource, WorkflowError, WorkflowStepRequest,
    MAX_BOUNDARY_ITEMS, MAX_LIFETIME_STEPS, MAX_LOG_MESSAGES,
};

use crate::{validate_meta_json, ScriptMeta, SCRIPT_MEMORY_LIMIT_BYTES, SCRIPT_SYNC_SLICE_MS};

const PRELUDE_SOURCE: &str = include_str!("prelude.js");

/// Constrain a closure to a higher-ranked `for<'js>` signature so multi-argument
/// native functions infer one shared `'js` lifetime.
fn js_fn2<F, R>(function: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, Value<'js>) -> R + 'static,
{
    function
}

fn js_fn3<F, R>(function: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, Value<'js>, Value<'js>) -> R + 'static,
{
    function
}

/// Allowed `agent()` option keys. `model` is separate because it needs a dedicated
/// rejection message.
const ALLOWED_OPT_KEYS: [&str; 6] = [
    "label",
    "phase",
    "schema",
    "effort",
    "agentType",
    "isolation",
];

/// The only permitted `isolation` value.
const WORKTREE_ISOLATION: &str = "worktree";

/// Convert an rquickjs error, usually a JavaScript exception, to stable text.
fn caught_text(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    let result: Result<(), rquickjs::Error> = Err(error);
    match result.catch(ctx) {
        Err(caught) => caught.to_string(),
        Ok(()) => unreachable!("an Err stays an Err through catch"),
    }
}

/// Convert `Int` and `Float` to `f64`; return `None` for other types.
fn number_of(value: &Value<'_>) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_int().map(|int| f64::from(int)))
}

// ---------------------------------------------------------------------------
// Boundary adapter: QuickJS values as `SourceValue` / `SinkBuilder`
// ---------------------------------------------------------------------------

/// Outbound direction: feed script values to the workflow-core cloner.
#[derive(Clone)]
struct QjsSource<'js> {
    ctx: Ctx<'js>,
    value: Value<'js>,
}

impl<'js> QjsSource<'js> {
    fn new(ctx: Ctx<'js>, value: Value<'js>) -> Self {
        Self { ctx, value }
    }
}

impl<'js> SourceValue for QjsSource<'js> {
    fn kind(&self) -> SourceKind {
        match self.value.type_of() {
            Type::Undefined | Type::Uninitialized | Type::Symbol => SourceKind::Undefined,
            Type::Null => SourceKind::Null,
            Type::Bool => SourceKind::Bool,
            Type::Int | Type::Float => SourceKind::Number,
            // BigInt is serialized as decimal text because `JSON.stringify` throws
            // for it; this is the only lossless, visible representation.
            Type::String | Type::BigInt => SourceKind::String,
            Type::Array => SourceKind::Array,
            Type::Function | Type::Constructor => SourceKind::Function,
            _ => SourceKind::Object,
        }
    }

    fn identity(&self) -> Option<usize> {
        if matches!(self.kind(), SourceKind::Array | SourceKind::Object) {
            // Read only the tag union's pointer for identity; never dereference it.
            let pointer = unsafe { self.value.as_raw().u.ptr } as usize;
            Some(pointer)
        } else {
            None
        }
    }

    fn boolean(&self) -> bool {
        self.value.as_bool().unwrap_or(false)
    }

    fn number(&self) -> f64 {
        number_of(&self.value).unwrap_or(f64::NAN)
    }

    fn string(&self) -> String {
        if let Some(text) = self.value.as_string() {
            return text.to_string().unwrap_or_default();
        }
        if let Some(big) = self.value.as_big_int() {
            if let Ok(value) = big.clone().to_i64() {
                return value.to_string();
            }
        }
        String::new()
    }

    fn array_length(&self) -> Result<f64, String> {
        let Some(object) = self.value.as_object() else {
            return Err("Array value is no longer an object".into());
        };
        match object.get::<_, Value>("length") {
            Ok(length) => Ok(number_of(&length).unwrap_or(f64::NAN)),
            Err(error) => Err(caught_text(&self.ctx, error)),
        }
    }

    fn array_element(&self, index: usize) -> Result<Self, String> {
        let Some(object) = self.value.as_object() else {
            return Err("Array value is no longer an object".into());
        };
        match object.get::<_, Value>(index as u32) {
            Ok(value) => Ok(Self::new(self.ctx.clone(), value)),
            Err(error) => Err(caught_text(&self.ctx, error)),
        }
    }

    fn entries(&self) -> Result<Vec<(String, Self)>, String> {
        let Some(object) = self.value.as_object() else {
            return Err("Object value is no longer an object".into());
        };
        let mut entries = Vec::new();
        for key in object.keys::<String>() {
            let key = key.map_err(|error| caught_text(&self.ctx, error))?;
            let value: Value = object
                .get(key.as_str())
                .map_err(|error| caught_text(&self.ctx, error))?;
            entries.push((key, Self::new(self.ctx.clone(), value)));
        }
        Ok(entries)
    }
}

/// Inbound direction: materialize a plain-data graph as script values.
struct QjsSink<'js> {
    ctx: Ctx<'js>,
    /// Reused freeze helper. Host error records use a null prototype and are frozen,
    /// so they deliberately do not satisfy script-side `instanceof Error`.
    freeze: Function<'js>,
}

impl<'js> QjsSink<'js> {
    fn new(ctx: Ctx<'js>) -> Result<Self, String> {
        let freeze: Function = ctx
            .eval("(value => Object.freeze(value))")
            .map_err(|error| caught_text(&ctx, error))?;
        Ok(Self { ctx, freeze })
    }

    fn convert(&self, error: rquickjs::Error) -> String {
        caught_text(&self.ctx, error)
    }
}

impl<'js> SinkBuilder for QjsSink<'js> {
    type Value = Value<'js>;

    fn null(&mut self) -> Result<Self::Value, String> {
        Ok(Value::new_null(self.ctx.clone()))
    }

    fn boolean(&mut self, value: bool) -> Result<Self::Value, String> {
        Ok(Value::new_bool(self.ctx.clone(), value))
    }

    fn number(&mut self, value: f64) -> Result<Self::Value, String> {
        Ok(Value::new_float(self.ctx.clone(), value))
    }

    fn string(&mut self, value: &str) -> Result<Self::Value, String> {
        rquickjs::String::from_str(self.ctx.clone(), value)
            .map(rquickjs::String::into_value)
            .map_err(|error| self.convert(error))
    }

    fn empty_array(&mut self) -> Result<Self::Value, String> {
        Array::new(self.ctx.clone())
            .map(Array::into_value)
            .map_err(|error| self.convert(error))
    }

    fn empty_object(&mut self) -> Result<Self::Value, String> {
        let object = Object::new(self.ctx.clone()).map_err(|error| self.convert(error))?;
        // A null prototype combines with boundary `__proto__` filtering to defend
        // against prototype pollution.
        object
            .set_prototype(None)
            .map_err(|error| self.convert(error))?;
        Ok(object.into_value())
    }

    fn push_element(
        &mut self,
        array: &Self::Value,
        element: &Self::Value,
    ) -> Result<(), String> {
        let Some(array) = array.as_array() else {
            return Err("Sink array identity was lost".into());
        };
        array
            .set(array.len(), element.clone())
            .map_err(|error| self.convert(error))
    }

    fn set_property(
        &mut self,
        object: &Self::Value,
        key: &str,
        value: &Self::Value,
    ) -> Result<(), String> {
        let Some(object) = object.as_object() else {
            return Err("Sink object identity was lost".into());
        };
        object
            .set(key, value.clone())
            .map_err(|error| self.convert(error))
    }

    fn host_error(
        &mut self,
        name: &str,
        message: &str,
        stack: &str,
    ) -> Result<Self::Value, String> {
        let object = Object::new(self.ctx.clone()).map_err(|error| self.convert(error))?;
        object
            .set_prototype(None)
            .map_err(|error| self.convert(error))?;
        object.set("name", name).map_err(|error| self.convert(error))?;
        object
            .set("message", message)
            .map_err(|error| self.convert(error))?;
        object
            .set("stack", stack)
            .map_err(|error| self.convert(error))?;
        self.freeze
            .call::<_, Value>((object,))
            .map_err(|error| self.convert(error))
    }
}

/// Clone outbound and serialize to JSON.
fn value_to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<JsonValue, String> {
    let graph = clone_out(&QjsSource::new(ctx.clone(), value))
        .map_err(|error| error.to_string())?;
    graph_to_json(&graph).map_err(|error| error.to_string())
}

/// Parse JSON into a graph and materialize it inbound.
fn json_to_value<'js>(ctx: &Ctx<'js>, json: &JsonValue) -> Result<Value<'js>, String> {
    let graph = graph_from_json(json).map_err(|error| error.to_string())?;
    let mut sink = QjsSink::new(ctx.clone())?;
    clone_in(&graph, &mut sink).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Disposable engine helpers for pre-start validation
// ---------------------------------------------------------------------------

/// Create a disposable engine with a memory limit and a short time slice.
fn throwaway_engine() -> Result<(Runtime, Context), WorkflowError> {
    let runtime = Runtime::new()
        .map_err(|error| WorkflowError::Invalid(format!("Script engine initialization failed: {error}")))?;
    runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT_BYTES);
    let deadline = Instant::now() + Duration::from_millis(1_000);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime)
        .map_err(|error| WorkflowError::Invalid(format!("Script engine initialization failed: {error}")))?;
    Ok((runtime, context))
}

/// Evaluate and validate a meta literal in an empty environment.
///
/// The empty environment permits only pure literals: free identifiers raise a
/// `ReferenceError`, and outbound cloning structurally drops function values.
pub(crate) fn evaluate_meta(literal: &str) -> Result<ScriptMeta, WorkflowError> {
    let (_runtime, context) = throwaway_engine()?;
    let json = context.with(|ctx| -> Result<JsonValue, WorkflowError> {
        let wrapped = format!("({literal})");
        let value: Value = ctx.eval(wrapped.as_bytes()).map_err(|error| {
            WorkflowError::Invalid(format!(
                "meta must be a pure literal (it may not reference variables or call functions): {}",
                caught_text(&ctx, error)
            ))
        })?;
        value_to_json(&ctx, value)
            .map_err(|error| WorkflowError::Invalid(format!("meta cannot cross the boundary: {error}")))
    })?;
    validate_meta_json(&json)
}

/// Compile the body with the `AsyncFunction` constructor without executing it, so
/// syntax errors surface before startup.
pub(crate) fn check_body_syntax(body: &str) -> Result<(), WorkflowError> {
    let (_runtime, context) = throwaway_engine()?;
    context.with(|ctx| -> Result<(), WorkflowError> {
        let checker: Function = ctx
            .eval("((body) => { (async () => {}).constructor(body); })")
            .map_err(|error| {
                WorkflowError::Invalid(format!(
                    "Script engine initialization failed: {}",
                    caught_text(&ctx, error)
                ))
            })?;
        checker.call::<_, ()>((body,)).map_err(|error| {
            WorkflowError::Invalid(format!("Script body syntax error: {}", caught_text(&ctx, error)))
        })
    })
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// State shared between native functions and `advance`.
struct Inner {
    /// Step requests issued during this `advance`, in emission order.
    requests: Vec<WorkflowStepRequest>,
    /// Pending `log()` narrative lines.
    logs: Vec<String>,
    /// Current phase set by `phase(title)`.
    current_phase: Option<String>,
    /// Phase title to index mapping. Register `meta.phases` in declaration order;
    /// append titles first encountered in the script.
    phase_titles: Vec<String>,
    /// Total steps issued during the lifetime, equal to the next global step index.
    issued_total: usize,
    /// Issued steps without results, used for deadlock detection.
    pending: usize,
    /// Successful settlement value of the script body Promise.
    done: Option<JsonValue>,
    /// Script failure text from an uncaught exception or an unbridgeable return value.
    failed: Option<String>,
    budget_total: Option<u64>,
    tokens_spent: u64,
    /// Role policy for this run, enforced synchronously by `issue_step` at `agent()`.
    role_policy: StepRolePolicy,
}

impl Inner {
    /// Return or register the stable index for a phase title.
    fn phase_index(&mut self, title: &str) -> usize {
        if let Some(position) = self.phase_titles.iter().position(|known| known == title) {
            return position;
        }
        self.phase_titles.push(title.to_owned());
        self.phase_titles.len() - 1
    }
}

/// Script source. Construction and use are confined to one thread.
///
/// Field order determines drop order. Both `Persistent` values must be released
/// before `Context` and `Runtime`, or QuickJS can fail its GC-list assertion during
/// runtime destruction.
pub struct ScriptSource {
    boot: Persistent<Function<'static>>,
    resolve: Persistent<Function<'static>>,
    inner: Rc<RefCell<Inner>>,
    /// Body consumed and evaluated on the first `advance`.
    body: Option<String>,
    /// Number of consumed outcomes. `previous` is append-only.
    cursor: usize,
    interrupted: Arc<AtomicBool>,
    /// Synchronous time slice for one `advance`; tests use a short slice to prove
    /// that unbounded synchronous loops are interrupted.
    sync_slice: Duration,
    context: Context,
    runtime: Runtime,
}

impl ScriptSource {
    pub(crate) fn start(
        body: String,
        meta: ScriptMeta,
        args: Option<JsonValue>,
        budget_total: Option<u64>,
        role_policy: StepRolePolicy,
    ) -> Result<Self, WorkflowError> {
        let runtime = Runtime::new()
            .map_err(|error| WorkflowError::Script(format!("Script engine initialization failed: {error}")))?;
        runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT_BYTES);
        let context = Context::full(&runtime)
            .map_err(|error| WorkflowError::Script(format!("Script engine initialization failed: {error}")))?;

        let inner = Rc::new(RefCell::new(Inner {
            requests: Vec::new(),
            logs: Vec::new(),
            current_phase: None,
            // Pre-register `meta.phases` so matching script phases share an index
            // and progress groups retain declaration order.
            phase_titles: meta.phases.iter().map(|phase| phase.title.clone()).collect(),
            issued_total: 0,
            pending: 0,
            done: None,
            failed: None,
            budget_total,
            tokens_spent: 0,
            role_policy,
        }));

        let (boot, resolve) = context.with(|ctx| -> Result<_, WorkflowError> {
            let fail = |error: rquickjs::Error, ctx: &Ctx<'_>| {
                WorkflowError::Script(format!("Script runtime setup failed: {}", caught_text(ctx, error)))
            };
            let natives = Object::new(ctx.clone()).map_err(|error| fail(error, &ctx))?;

            let issue_state = Rc::clone(&inner);
            natives
                .set(
                    "issue",
                    Function::new(
                        ctx.clone(),
                        js_fn3(move |ctx, prompt, opts| issue_step(&ctx, &issue_state, prompt, opts)),
                    )
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let log_state = Rc::clone(&inner);
            natives
                .set(
                    "log",
                    Function::new(ctx.clone(), move |message: String| {
                        let mut inner = log_state.borrow_mut();
                        // The ledger owns trimming. This is only a hard cap to keep
                        // a synchronous hot loop from exhausting memory between drains.
                        if inner.logs.len() < MAX_LOG_MESSAGES * 2 {
                            inner.logs.push(message);
                        }
                    })
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let phase_state = Rc::clone(&inner);
            natives
                .set(
                    "phase",
                    Function::new(ctx.clone(), move |title: String| {
                        let mut inner = phase_state.borrow_mut();
                        inner.phase_index(&title);
                        inner.current_phase = Some(title);
                    })
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let done_state = Rc::clone(&inner);
            natives
                .set(
                    "done",
                    Function::new(
                        ctx.clone(),
                        js_fn2(move |ctx, value| {
                            let mut inner = done_state.borrow_mut();
                            match value_to_json(&ctx, value) {
                                Ok(json) => inner.done = Some(json),
                                Err(error) => {
                                    inner.failed =
                                        Some(format!("Script return value cannot cross the boundary: {error}"));
                                }
                            }
                        }),
                    )
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let fail_state = Rc::clone(&inner);
            natives
                .set(
                    "fail",
                    Function::new(ctx.clone(), move |message: String| {
                        fail_state.borrow_mut().failed = Some(message);
                    })
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let total_state = Rc::clone(&inner);
            natives
                .set(
                    "budgetTotal",
                    Function::new(ctx.clone(), move || -> Option<f64> {
                        total_state.borrow().budget_total.map(|total| total as f64)
                    })
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            let spent_state = Rc::clone(&inner);
            natives
                .set(
                    "budgetSpent",
                    Function::new(ctx.clone(), move || -> f64 {
                        spent_state.borrow().tokens_spent as f64
                    })
                    .map_err(|error| fail(error, &ctx))?,
                )
                .map_err(|error| fail(error, &ctx))?;

            natives
                .set("maxItems", MAX_BOUNDARY_ITEMS as u32)
                .map_err(|error| fail(error, &ctx))?;

            let prelude: Function = ctx
                .eval(PRELUDE_SOURCE.as_bytes())
                .map_err(|error| fail(error, &ctx))?;
            let pair: Array = prelude
                .call((natives,))
                .map_err(|error| fail(error, &ctx))?;
            let boot: Function = pair.get(0).map_err(|error| fail(error, &ctx))?;
            let resolve: Function = pair.get(1).map_err(|error| fail(error, &ctx))?;

            if let Some(args) = &args {
                let value = json_to_value(&ctx, args)
                    .map_err(|error| WorkflowError::Script(format!("args cannot cross the boundary: {error}")))?;
                ctx.globals()
                    .set("args", value)
                    .map_err(|error| fail(error, &ctx))?;
            }

            Ok((
                Persistent::save(&ctx, boot),
                Persistent::save(&ctx, resolve),
            ))
        })?;

        Ok(Self {
            runtime,
            context,
            inner,
            boot,
            resolve,
            body: Some(body),
            cursor: 0,
            interrupted: Arc::new(AtomicBool::new(false)),
            sync_slice: Duration::from_millis(SCRIPT_SYNC_SLICE_MS),
        })
    }

    /// Test hook: inject a short time slice to prove hot loops are interrupted.
    #[cfg(test)]
    pub(crate) fn set_sync_slice_for_tests(&mut self, slice: Duration) {
        self.sync_slice = slice;
    }

    fn slice_error(&self) -> WorkflowError {
        WorkflowError::Script(format!(
            "Synchronous script execution exceeded its {} ms time slice; orchestration scripts may assemble prompts and classify results, but must not perform long computations",
            self.sync_slice.as_millis()
        ))
    }

    /// Pump the microtask queue until quiescent. `advance` installs the time-slice handler.
    fn pump(&self) -> Result<(), WorkflowError> {
        while self.runtime.is_job_pending() {
            // Prelude then/catch links handle individual job exceptions. Continue
            // draining because failures such as a memory limit have no better target.
            let _ = self.runtime.execute_pending_job();
            if self.interrupted.load(Ordering::SeqCst) {
                return Err(self.slice_error());
            }
        }
        Ok(())
    }

    fn advance_inner(
        &mut self,
        previous: &[StepOutcome],
    ) -> Result<StepProgress, WorkflowError> {
        // 1. First call: start the body. Top-level synchronous code is time-sliced.
        if let Some(body) = self.body.take() {
            let boot = self.boot.clone();
            let interrupted = Arc::clone(&self.interrupted);
            let slice_error = self.slice_error();
            self.context.with(|ctx| -> Result<(), WorkflowError> {
                let boot = boot.restore(&ctx).map_err(|error| {
                    WorkflowError::Script(format!("Script boot function is unavailable: {error}"))
                })?;
                boot.call::<_, ()>((body.as_str(),)).map_err(|error| {
                    if interrupted.load(Ordering::SeqCst) {
                        slice_error.clone()
                    } else {
                        WorkflowError::Script(format!(
                            "Script failed to start: {}",
                            caught_text(&ctx, error)
                        ))
                    }
                })
            })?;
        }

        // 2. Return newly settled outcomes to their matching Promises.
        if previous.len() > self.cursor {
            let fresh = previous[self.cursor..].to_vec();
            self.cursor = previous.len();
            let resolve = self.resolve.clone();
            let state = Rc::clone(&self.inner);
            self.context.with(|ctx| -> Result<(), WorkflowError> {
                let resolve = resolve.restore(&ctx).map_err(|error| {
                    WorkflowError::Script(format!("Script resolver is unavailable: {error}"))
                })?;
                for outcome in &fresh {
                    {
                        let mut inner = state.borrow_mut();
                        if let Some(tokens) = outcome.tokens {
                            inner.tokens_spent = inner.tokens_spent.saturating_add(tokens);
                        }
                        inner.pending = inner.pending.saturating_sub(1);
                    }
                    let value = match &outcome.value {
                        Some(json) => json_to_value(&ctx, json).map_err(|error| {
                            WorkflowError::Script(format!(
                                "Step {} result cannot cross the boundary: {error}",
                                outcome.index
                            ))
                        })?,
                        // Failed or skipped steps settle as null in the script domain;
                        // their failure reasons stay in progress cards and final records.
                        None => Value::new_null(ctx.clone()),
                    };
                    resolve
                        .call::<_, ()>((outcome.index as u32, value))
                        .map_err(|error| {
                            WorkflowError::Script(format!(
                                "Result delivery failed: {}",
                                caught_text(&ctx, error)
                            ))
                        })?;
                }
                Ok(())
            })?;
        }

        // 3. Pump microtasks to quiescence. Continuations may issue new steps.
        self.pump()?;

        // 4. Settle this advance. Failure wins, then completion; once the script settles,
        // unconsumed requests issued in the same advance are invalid.
        let mut inner = self.inner.borrow_mut();
        if let Some(message) = inner.failed.take() {
            return Err(WorkflowError::Script(message));
        }
        if let Some(value) = inner.done.take() {
            return Ok(StepProgress::Done(value));
        }
        let requests = std::mem::take(&mut inner.requests);
        if requests.is_empty() && inner.pending == 0 {
            return Err(WorkflowError::Deadlock(
                "The script has not completed and has no pending agent() calls; it is waiting for a Promise that can never settle"
                    .into(),
            ));
        }
        Ok(StepProgress::Run(requests))
    }
}

impl StepSource for ScriptSource {
    fn advance(&mut self, previous: &[StepOutcome]) -> Result<StepProgress, WorkflowError> {
        // The time-slice guard covers all of `advance`: boot-level code, synchronous
        // outcome delivery, and every microtask pumped under the same deadline.
        self.interrupted.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + self.sync_slice;
        let flag = Arc::clone(&self.interrupted);
        self.runtime.set_interrupt_handler(Some(Box::new(move || {
            if Instant::now() > deadline {
                flag.store(true, Ordering::SeqCst);
                true
            } else {
                false
            }
        })));
        let result = self.advance_inner(previous);
        self.runtime.set_interrupt_handler(None);
        if self.interrupted.load(Ordering::SeqCst) {
            return Err(self.slice_error());
        }
        result
    }

    fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.inner.borrow_mut().logs)
    }
}

/// Suffix listing valid names for role error messages.
///
/// Return no list when the names are unavailable. `accepts` then permits every name,
/// so only the required-role branch can reach this function.
fn legal_role_suffix(policy: &StepRolePolicy) -> String {
    match policy.known_names.as_deref() {
        None => String::new(),
        Some([]) => "; this conversation has no configured roles".to_owned(),
        Some(names) => format!("; roles available in this conversation: {}", names.join(", ")),
    }
}

/// Native `agent()` entry point: validate prompt and options, enforce limits and
/// budget, then register the request.
fn issue_step<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<Inner>>,
    prompt: Value<'js>,
    opts: Value<'js>,
) -> Result<u32, rquickjs::Error> {
    let Some(prompt) = prompt.as_string() else {
        return Err(Exception::throw_type(
            ctx,
            "agent(prompt, opts) prompt must be a string",
        ));
    };
    let prompt = prompt.to_string()?;
    if prompt.trim().is_empty() {
        return Err(Exception::throw_type(
            ctx,
            "agent() prompt must not be empty; it is the only task body visible to the step subagent",
        ));
    }
    let mut request = WorkflowStepRequest::from_prompt(prompt);

    if !(opts.is_null() || opts.is_undefined()) {
        if opts.is_array() || opts.is_function() || opts.as_object().is_none() {
            return Err(Exception::throw_type(
                ctx,
                "agent() opts must be a plain object (label/phase/schema/effort/agentType)",
            ));
        }
        let object = opts.as_object().expect("checked above");
        for key in object.keys::<String>() {
            let key = key?;
            match key.as_str() {
                "model" => {
                    return Err(Exception::throw_type(
                        ctx,
                        "Steps do not accept model: the role name is the complete model-facing input. Select a role with agentType; this tool schema's $defs.agentType lists valid values, and user configuration maps roles to provider/model.",
                    ))
                }
                known if ALLOWED_OPT_KEYS.contains(&known) => {}
                unknown => {
                    return Err(Exception::throw_type(
                        ctx,
                        &format!(
                            "agent() opts does not recognize key {unknown}; allowed keys: label, phase, schema, effort, agentType, isolation"
                        ),
                    ))
                }
            }
        }
        request.label = read_opt_string(ctx, object, "label")?;
        let explicit_phase = read_opt_string(ctx, object, "phase")?;
        if explicit_phase.is_some() {
            request.phase = explicit_phase;
        }
        request.effort = read_opt_string(ctx, object, "effort")?;
        if let Some(effort) = request.effort.as_deref() {
            if !matches!(effort, "low" | "medium" | "high" | "xhigh") {
                return Err(Exception::throw_type(
                    ctx,
                    &format!("effort must be low, medium, high, or xhigh; got {effort}"),
                ));
            }
        }
        request.agent_type = read_opt_string(ctx, object, "agentType")?;
        request.isolation = read_opt_string(ctx, object, "isolation")?;
        if let Some(isolation) = request.isolation.as_deref() {
            if isolation != WORKTREE_ISOLATION {
                return Err(Exception::throw_type(
                    ctx,
                    &format!(
                        "isolation must be \"{WORKTREE_ISOLATION}\"; got {isolation}"
                    ),
                ));
            }
        }
        let schema: Value = object.get("schema")?;
        if !(schema.is_undefined() || schema.is_null()) {
            let json = value_to_json(ctx, schema).map_err(|error| {
                Exception::throw_type(ctx, &format!("schema cannot cross the boundary: {error}"))
            })?;
            workflow_core::schema::compile(&json).map_err(|error| {
                Exception::throw_type(ctx, &format!("schema is invalid: {error}"))
            })?;
            request.schema = Some(json);
        }
    }

    // Enforce the role policy here, before host derivation. A derivation failure
    // would only resolve this step as null, allowing the script to continue with
    // missing results. A synchronous exception reaches the prelude catch and fails
    // the run, allowing the model to correct `agentType`.
    //
    // Keep this outside the options block because `agent("prompt")` omits options
    // and must still be rejected when a role is required.
    {
        let policy = state.borrow().role_policy.clone();
        match request.agent_type.as_deref() {
            None if policy.required => {
                return Err(Exception::throw_type(
                    ctx,
                    &format!(
                        "agent() opts.agentType is required: a role decides which model runs the step, and omitting it would silently inherit the main conversation's model{}",
                        legal_role_suffix(&policy)
                    ),
                ));
            }
            Some(name) if !policy.accepts(name) => {
                return Err(Exception::throw_type(
                    ctx,
                    &format!(
                        "agentType {name:?} is not a role available in this conversation{}",
                        legal_role_suffix(&policy)
                    ),
                ));
            }
            _ => {}
        }
    }

    let mut inner = state.borrow_mut();
    if inner.issued_total >= MAX_LIFETIME_STEPS {
        return Err(Exception::throw_type(
            ctx,
            &format!("A run may issue at most {MAX_LIFETIME_STEPS} steps; this is a safeguard against runaway loops, not a concurrency limit"),
        ));
    }
    if let Some(total) = inner.budget_total {
        if inner.tokens_spent >= total {
            return Err(Exception::throw_type(
                ctx,
                &format!(
                    "Workflow budget is exhausted (limit {total} tokens, {} tokens charged); no new agent() calls are accepted",
                    inner.tokens_spent
                ),
            ));
        }
    }
    if request.phase.is_none() {
        request.phase = inner.current_phase.clone();
    }
    if let Some(title) = request.phase.clone() {
        request.phase_index = Some(inner.phase_index(&title));
    }
    let id = inner.issued_total;
    inner.issued_total += 1;
    inner.pending += 1;
    inner.requests.push(request);
    Ok(id as u32)
}

/// Read an optional string option; present values must be nonempty strings.
fn read_opt_string<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> Result<Option<String>, rquickjs::Error> {
    let value: Value = object.get(key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_string() else {
        return Err(Exception::throw_type(
            ctx,
            &format!("agent() opts.{key} must be a string"),
        ));
    };
    let text = text.to_string()?;
    if text.trim().is_empty() {
        return Err(Exception::throw_type(
            ctx,
            &format!("agent() opts.{key} must not be an empty string"),
        ));
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test scripts use the permissive role policy by default. Role-required behavior
    /// has dedicated tests.
    fn spec(script: &str) -> crate::ScriptSpec {
        crate::ScriptSpec::parse(script, None, None, StepRolePolicy::permissive())
            .expect("test script parses")
            .1
    }

    /// Equivalent helper for the role-required policy.
    fn spec_requiring(script: &str, known: &[&str]) -> crate::ScriptSpec {
        crate::ScriptSpec::parse(
            script,
            None,
            None,
            StepRolePolicy {
                required: true,
                known_names: Some(known.iter().map(|name| (*name).to_owned()).collect()),
            },
        )
        .expect("test script parses")
        .1
    }

    fn spec_with(script: &str, args: JsonValue) -> crate::ScriptSpec {
        crate::ScriptSpec::parse(script, Some(args), None, StepRolePolicy::permissive())
            .expect("test script parses")
            .1
    }

    fn outcome(index: usize, value: JsonValue) -> StepOutcome {
        StepOutcome {
            index,
            value: Some(value),
            cached: false,
            error: None,
            tokens: None,
        }
    }

    fn null_outcome(index: usize) -> StepOutcome {
        StepOutcome {
            index,
            value: None,
            cached: false,
            error: Some("failed".into()),
            tokens: None,
        }
    }

    /// Minimal driver loop: feed outcomes and collect requests until completion or failure.
    fn drive_to_done(
        source: &mut ScriptSource,
        mut respond: impl FnMut(usize, &WorkflowStepRequest) -> StepOutcome,
    ) -> Result<JsonValue, WorkflowError> {
        let mut outcomes: Vec<StepOutcome> = Vec::new();
        let mut issued: Vec<WorkflowStepRequest> = Vec::new();
        for _ in 0..64 {
            match source.advance(&outcomes)? {
                StepProgress::Done(value) => return Ok(value),
                StepProgress::Run(requests) => {
                    for request in requests {
                        let index = issued.len();
                        issued.push(request.clone());
                        outcomes.push(respond(index, &request));
                    }
                }
            }
        }
        panic!("script never settled");
    }

    const HEADER: &str = "export const meta = { name: \"t\", description: \"d\" }\n";

    fn script(body: &str) -> String {
        format!("{HEADER}{body}")
    }

    #[test]
    fn a_script_that_only_returns_settles_on_the_first_advance() {
        let mut source = spec(&script("return { ok: 1, text: \"猫\" }")).start().unwrap();
        let value = source.advance(&[]).unwrap();
        assert_eq!(
            value,
            StepProgress::Done(json!({ "ok": 1, "text": "猫" }))
        );
    }

    #[test]
    fn agent_calls_issue_requests_in_order_and_resolve_with_their_outcomes() {
        let mut source = spec(&script(
            "const a = await agent(\"first\");\nconst b = await agent(`second saw ${a}`);\nreturn { a, b };",
        ))
        .start()
        .unwrap();
        let value = drive_to_done(&mut source, |index, request| {
            outcome(index, json!(format!("r{index}:{}", request.prompt)))
        })
        .unwrap();
        assert_eq!(
            value,
            json!({ "a": "r0:first", "b": "r1:second saw r0:first" })
        );
    }

    #[test]
    fn parallel_fans_out_in_one_wave_and_maps_failures_to_null() {
        let mut source = spec(&script(
            "const results = await parallel([() => agent(\"x\"), () => { throw new Error(\"boom\"); }, () => agent(\"y\")]);\nreturn results;",
        ))
        .start()
        .unwrap();
        // The first `advance` returns both real steps; a throwing thunk consumes none.
        let progress = source.advance(&[]).unwrap();
        let StepProgress::Run(requests) = progress else {
            panic!("expected a wave of requests");
        };
        assert_eq!(
            requests.iter().map(|request| request.prompt.as_str()).collect::<Vec<_>>(),
            vec!["x", "y"]
        );
        let outcomes = vec![outcome(0, json!("X")), outcome(1, json!("Y"))];
        let done = source.advance(&outcomes).unwrap();
        assert_eq!(done, StepProgress::Done(json!(["X", null, "Y"])));
    }

    #[test]
    fn pipeline_has_no_stage_barrier_and_a_throwing_stage_drops_the_item_to_null() {
        let mut source = spec_with(
            &script(
                "const out = await pipeline(args, (item) => agent(`s1:${item}`), (prev, item) => { if (item === \"b\") throw new Error(\"no\"); return agent(`s2:${prev}`); });\nreturn out;",
            ),
            json!(["a", "b"]),
        )
        .start()
        .unwrap();

        // First wave: stage one for both items.
        let StepProgress::Run(wave1) = source.advance(&[]).unwrap() else {
            panic!("expected stage-one wave");
        };
        assert_eq!(
            wave1.iter().map(|request| request.prompt.as_str()).collect::<Vec<_>>(),
            vec!["s1:a", "s1:b"]
        );

        // Returning only item a's stage one must immediately issue its stage two
        // without waiting for item b.
        let outcomes = vec![outcome(0, json!("A1"))];
        let StepProgress::Run(wave2) = source.advance(&outcomes).unwrap() else {
            panic!("expected item-a stage-two without a barrier");
        };
        assert_eq!(
            wave2.iter().map(|request| request.prompt.as_str()).collect::<Vec<_>>(),
            vec!["s2:A1"]
        );

        // Return the remaining outcomes: item b's throwing stage two becomes null.
        let outcomes = vec![
            outcome(0, json!("A1")),
            outcome(2, json!("A2")),
            outcome(1, json!("B1")),
        ];
        let done = source.advance(&outcomes).unwrap();
        assert_eq!(done, StepProgress::Done(json!(["A2", null])));
    }

    #[test]
    fn a_failed_step_resolves_to_null_in_the_script_domain() {
        let mut source = spec(&script(
            "const value = await agent(\"doomed\");\nreturn { value };",
        ))
        .start()
        .unwrap();
        let StepProgress::Run(_) = source.advance(&[]).unwrap() else {
            panic!("expected one request");
        };
        let done = source.advance(&[null_outcome(0)]).unwrap();
        assert_eq!(done, StepProgress::Done(json!({ "value": null })));
    }

    /// Under the required policy, missing `agentType` throws before issuing a request.
    /// The error must include valid values so the script can be corrected.
    #[test]
    fn a_required_role_throws_at_the_agent_call_before_any_step_is_issued() {
        let mut source = spec_requiring(
            &script(
                r#"try { agent("p"); return "missed"; } catch (e) { return e.message; }"#,
            ),
            &["reviewer", "worker"],
        )
        .start()
        .unwrap();
        let StepProgress::Done(value) = source.advance(&[]).unwrap() else {
            panic!("A script missing its role must not issue any steps");
        };
        let message = value.as_str().expect("error message");
        assert!(message.contains("agentType"), "{message}");
        assert!(message.contains("reviewer"), "message must list valid values: {message}");
        assert!(message.contains("worker"), "message must list valid values: {message}");
    }

    /// A role name outside this run's valid set also throws synchronously so an
    /// unknown and a missing name receive equally actionable feedback.
    #[test]
    fn an_unknown_role_name_throws_at_the_agent_call_too() {
        let mut source = spec_requiring(
            &script(
                r#"try { agent("p", { agentType: "revieww" }); return "missed"; } catch (e) { return e.message; }"#,
            ),
            &["reviewer"],
        )
        .start()
        .unwrap();
        let StepProgress::Done(value) = source.advance(&[]).unwrap() else {
            panic!("A script with an unknown role must not issue any steps");
        };
        let message = value.as_str().expect("error message");
        assert!(message.contains("revieww"), "{message}");
        assert!(message.contains("reviewer"), "message must list valid values: {message}");
    }

    /// A valid role is issued and preserved on the request; requirement checks only
    /// reject missing or unknown names.
    #[test]
    fn a_legal_role_still_issues_the_step_with_its_agent_type() {
        let mut source = spec_requiring(
            &script("return await agent(\"p\", { agentType: \"reviewer\" });"),
            &["reviewer"],
        )
        .start()
        .unwrap();
        let StepProgress::Run(wave) = source.advance(&[]).unwrap() else {
            panic!("expected one request");
        };
        assert_eq!(wave.len(), 1);
        assert_eq!(wave[0].agent_type.as_deref(), Some("reviewer"));
    }

    /// With unavailable role names, check presence but not the name itself. A failed
    /// disk read must not reject an otherwise valid script; derivation resolves names.
    #[test]
    fn an_unknown_role_set_checks_presence_but_never_the_name() {
        let policy = StepRolePolicy {
            required: true,
            known_names: None,
        };
        let (_, spec) = crate::ScriptSpec::parse(
            &script("return await agent(\"p\", { agentType: \"anything\" });"),
            None,
            None,
            policy,
        )
        .unwrap();
        let mut source = spec.start().unwrap();
        let StepProgress::Run(wave) = source.advance(&[]).unwrap() else {
            panic!("expected one request");
        };
        assert_eq!(wave[0].agent_type.as_deref(), Some("anything"));
    }

    #[test]
    fn empty_prompts_model_opts_and_unknown_opts_throw_catchable_type_errors() {
        let mut source = spec(&script(
            r#"const errors = [];
for (const attempt of [
  () => agent(""),
  () => agent("p", { model: "gpt" }),
  () => agent("p", { isolation: "vm" }),
  () => agent("p", { nope: 1 }),
  () => agent("p", { effort: "max" }),
]) {
  try { attempt(); errors.push("missed"); } catch (e) { errors.push(e.message.slice(0, 6)); }
}
return errors;"#,
        ))
        .start()
        .unwrap();
        let done = source.advance(&[]).unwrap();
        let StepProgress::Done(value) = done else {
            panic!("script should settle without issuing steps");
        };
        let texts = value.as_array().unwrap();
        assert_eq!(texts.len(), 5);
        for text in texts {
            assert_ne!(text, &json!("missed"));
        }
    }

    /// `isolation: "worktree"` is accepted and travels unchanged to the driver;
    /// validation is a value whitelist, not merely a key-presence check.
    #[test]
    fn worktree_isolation_is_accepted_and_travels_on_the_request() {
        let mut source = spec(&script(
            "const [a, b] = await parallel([() => agent(\"iso\", { isolation: \"worktree\" }), () => agent(\"plain\")]);\nreturn [a, b];",
        ))
        .start()
        .unwrap();
        let StepProgress::Run(requests) = source.advance(&[]).unwrap() else {
            panic!("expected two requests");
        };
        assert_eq!(requests[0].isolation.as_deref(), Some("worktree"));
        assert_eq!(requests[1].isolation, None);
    }

    #[test]
    fn determinism_blocks_throw_but_new_date_with_arguments_still_works() {
        let mut source = spec(&script(
            r#"const hits = [];
for (const attempt of [() => Date.now(), () => new Date(), () => Math.random()]) {
  try { attempt(); hits.push("missed"); } catch (e) { hits.push("blocked"); }
}
const fixed = new Date(0).getTime();
return { hits, fixed };"#,
        ))
        .start()
        .unwrap();
        let done = source.advance(&[]).unwrap();
        assert_eq!(
            done,
            StepProgress::Done(json!({ "hits": ["blocked", "blocked", "blocked"], "fixed": 0 }))
        );
    }

    #[test]
    fn an_uncaught_rejection_fails_the_run_with_the_error_text() {
        let mut source = spec(&script("throw new TypeError(\"broken plan\");")).start().unwrap();
        let error = source.advance(&[]).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Script execution failed"), "{text}");
        assert!(text.contains("TypeError: broken plan"), "{text}");
    }

    #[test]
    fn awaiting_a_promise_nobody_can_settle_is_reported_as_a_deadlock() {
        let mut source = spec(&script(
            "await new Promise(() => {});\nreturn 1;",
        ))
        .start()
        .unwrap();
        let error = source.advance(&[]).unwrap_err();
        assert!(matches!(error, WorkflowError::Deadlock(_)), "{error}");
    }

    #[test]
    fn a_hot_synchronous_loop_is_interrupted_by_the_sync_slice() {
        let mut source = spec(&script("while (true) {}")).start().unwrap();
        source.set_sync_slice_for_tests(Duration::from_millis(100));
        let started = Instant::now();
        let error = source.advance(&[]).unwrap_err();
        assert!(error.to_string().contains("time slice"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn phase_and_labels_annotate_requests_and_meta_phases_pin_declaration_order() {
        let source_text = "export const meta = { name: \"t\", description: \"d\", phases: [{ title: \"Scan\" }, { title: \"Fix\" }] }\nphase(\"Fix\");\nconst a = agent(\"one\", { label: \"L\" });\nphase(\"Scan\");\nconst b = agent(\"two\");\nconst c = agent(\"three\", { phase: \"Extra\" });\nawait Promise.all([a, b, c]);\nreturn 1;";
        let (_, spec) = crate::ScriptSpec::parse(
            source_text,
            None,
            None,
            StepRolePolicy::permissive(),
        )
        .unwrap();
        let mut source = spec.start().unwrap();
        let StepProgress::Run(requests) = source.advance(&[]).unwrap() else {
            panic!("expected requests");
        };
        assert_eq!(requests[0].label.as_deref(), Some("L"));
        assert_eq!(requests[0].phase.as_deref(), Some("Fix"));
        assert_eq!(requests[0].phase_index, Some(1));
        assert_eq!(requests[1].phase.as_deref(), Some("Scan"));
        assert_eq!(requests[1].phase_index, Some(0));
        assert_eq!(requests[2].phase.as_deref(), Some("Extra"));
        assert_eq!(requests[2].phase_index, Some(2));
    }

    #[test]
    fn logs_drain_in_order_and_survive_between_advances() {
        let mut source = spec(&script(
            "log(\"start\");\nconst a = await agent(\"x\");\nlog(`saw ${a}`);\nreturn 1;",
        ))
        .start()
        .unwrap();
        let StepProgress::Run(_) = source.advance(&[]).unwrap() else {
            panic!("expected a request");
        };
        assert_eq!(source.drain_logs(), vec!["start".to_owned()]);
        let done = source.advance(&[outcome(0, json!("X"))]).unwrap();
        assert_eq!(done, StepProgress::Done(json!(1)));
        assert_eq!(source.drain_logs(), vec!["saw X".to_owned()]);
    }

    #[test]
    fn budget_reports_totals_and_starves_new_agents_once_spent() {
        let (_, spec) = crate::ScriptSpec::parse(
            &script(
                r#"const before = [budget.total, budget.spent(), budget.remaining()];
await agent("one");
const after = [budget.total, budget.spent(), budget.remaining()];
let starved = "no";
try { agent("two"); } catch (e) { starved = "yes"; }
return { before, after, starved };"#,
            ),
            None,
            Some(100),
            StepRolePolicy::permissive(),
        )
        .unwrap();
        let mut source = spec.start().unwrap();
        let StepProgress::Run(_) = source.advance(&[]).unwrap() else {
            panic!("expected a request");
        };
        let mut paid = outcome(0, json!("ok"));
        paid.tokens = Some(150);
        let done = source.advance(&[paid]).unwrap();
        assert_eq!(
            done,
            StepProgress::Done(json!({
                "before": [100, 0, 100],
                "after": [100, 150, 0],
                "starved": "yes"
            }))
        );
    }

    #[test]
    fn budget_without_a_total_reports_null_and_infinite_remaining() {
        let mut source = spec(&script(
            "return { total: budget.total, infinite: budget.remaining() === Infinity };",
        ))
        .start()
        .unwrap();
        let done = source.advance(&[]).unwrap();
        assert_eq!(
            done,
            StepProgress::Done(json!({ "total": null, "infinite": true }))
        );
    }

    #[test]
    fn args_cross_the_boundary_as_plain_data() {
        let mut source = spec_with(
            &script("return { keys: Object.keys(args), first: args.items[0] };"),
            json!({ "items": [7, 8], "name": "猫" }),
        )
        .start()
        .unwrap();
        let done = source.advance(&[]).unwrap();
        assert_eq!(
            done,
            StepProgress::Done(json!({ "keys": ["items", "name"], "first": 7 }))
        );
    }

    #[test]
    fn schema_opts_are_validated_and_travel_on_the_request() {
        let mut source = spec(&script(
            "const v = await agent(\"structured\", { schema: { type: \"object\" } });\nreturn v;",
        ))
        .start()
        .unwrap();
        let StepProgress::Run(requests) = source.advance(&[]).unwrap() else {
            panic!("expected a request");
        };
        assert_eq!(requests[0].schema, Some(json!({ "type": "object" })));
        let done = source
            .advance(&[outcome(0, json!({ "checked": true }))])
            .unwrap();
        assert_eq!(done, StepProgress::Done(json!({ "checked": true })));
    }

    #[test]
    fn the_lifetime_step_cap_throws_into_the_script() {
        let mut source = spec(&script(
            r#"let issued = 0;
try {
  while (true) { agent(`step ${issued}`); issued += 1; }
} catch (e) {
  return { issued, message: e.message.includes("1000") };
}"#,
        ))
        .start()
        .unwrap();
        // A script can issue the full cap in one synchronous segment, catch the
        // error, and return. Completion invalidates its unawaited requests.
        let done = source.advance(&[]).unwrap();
        assert_eq!(
            done,
            StepProgress::Done(json!({ "issued": MAX_LIFETIME_STEPS, "message": true }))
        );
    }
}
