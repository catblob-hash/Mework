//! Structured boundary between script and host values.
//!
//! Executable script semantics never cross this boundary: functions become array
//! `null` slots or disappear from object properties. This structural invariant
//! ensures the host receives only a data graph. The internal representation is a
//! graph rather than JSON so cycles and shared references can round-trip; JSON is
//! an explicit adapter that rejects cycles and expands sharing.
//!
//! All four traversals use explicit work stacks. Script-generated input controls
//! nesting depth, so native recursion could exhaust the host stack. Construction
//! also enforces [`MAX_BOUNDARY_DEPTH`] so the resulting `serde_json::Value` can
//! be destroyed on a normal thread stack, and [`MAX_JSON_OUTPUT_NODES`] prevents
//! exponential JSON expansion of a small shared graph.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Number, Value};

use crate::MAX_BOUNDARY_ITEMS;

/// Maximum nesting depth for outbound cloning and JSON-to-graph input; the root
/// has depth 1.
///
/// This intentionally differs from [`crate::MAX_SCHEMA_DEPTH`] (10,000). Schemas
/// arrive through tool parameters, where serde_json's 128-level parser limit is a
/// prior safeguard. S18 scripts construct boundary values programmatically, with
/// no parser limit, and the result must be destroyed as a `serde_json::Value` on
/// the host. 1,000 far exceeds realistic agent payloads while remaining safe for
/// native recursive destruction.
pub const MAX_BOUNDARY_DEPTH: usize = 1_000;

/// Maximum total JSON nodes emitted by [`graph_to_json`].
///
/// Graphs preserve sharing while JSON does not: an n-level shared diamond graph
/// has O(n) nodes but expands to O(2^n) JSON values. This budget prevents a small
/// script graph from exhausting host memory during expansion.
pub const MAX_JSON_OUTPUT_NODES: usize = 100_000;

/// Stable index in the node store. Edges store indices, so they can point to
/// ancestors or be shared by multiple parents.
pub type NodeId = usize;

/// Structural kind of a source value.
///
/// `Undefined` and `Function` remain at the source boundary rather than entering
/// [`BoundaryNode`]. The cloner must decide from their position whether to emit
/// `null` or omit the property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
    Function,
}

/// Minimal read-only engine interface for the outbound cloner.
///
/// S18 maps rquickjs values to these operations. Filtering functions, array
/// limits, cycles, and sharing are decided here so engine adapters do not each
/// implement subtly different safety rules.
pub trait SourceValue: Clone {
    fn kind(&self) -> SourceKind;

    /// Returns a stable identity for objects and arrays, such as a pointer address.
    /// Primitives return `None`.
    ///
    /// The identity must remain stable during this clone. The visited table uses it
    /// to map cycles and shared diamonds to one node.
    fn identity(&self) -> Option<usize>;

    fn boolean(&self) -> bool;
    fn number(&self) -> f64;
    fn string(&self) -> String;

    /// Raw array `length`, which may be any `f64` in the script domain.
    ///
    /// The cloner calls this exactly once for each first-seen array. Repeated
    /// references reuse the visited entry and do not trigger another getter, fixing
    /// the observation point and preventing side-effecting accessors from changing
    /// one clone's shape. `Err` carries an accessor or Proxy-trap exception.
    fn array_length(&self) -> Result<f64, String>;

    fn array_element(&self, index: usize) -> Result<Self, String>;

    /// Own enumerable string-keyed properties in script-defined order.
    ///
    /// The result may include the literal key `"__proto__"`; the cloner skips it
    /// instead of passing it to downstream property assignment. Enumeration and
    /// reads can execute script getters, so both may fail.
    fn entries(&self) -> Result<Vec<(String, Self)>, String>;
}

/// Pure data node after crossing the boundary.
///
/// Container edges store [`NodeId`] values rather than embedded nodes, so cycles
/// and sharing that JSON cannot express remain valid shapes.
#[derive(Clone, Debug, PartialEq)]
pub enum BoundaryNode {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<NodeId>),
    Object(Vec<(String, NodeId)>),
    /// Frozen host error record `{name, message, stack}`.
    ///
    /// This is not an ordinary object with a forgeable tag. The S18 sink
    /// materializes it as a frozen null-prototype object, so `e instanceof Error`
    /// is `false` in the script.
    HostError {
        name: String,
        message: String,
        stack: String,
    },
}

/// A node store and its root node.
///
/// Private fields ensure public construction always supplies a valid root and
/// edges. [`BoundaryGraph::root`] and [`BoundaryGraph::node`] allow zero-copy
/// reads.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryGraph {
    nodes: Vec<BoundaryNode>,
    root: NodeId,
}

impl BoundaryGraph {
    /// Returns the graph root's index.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Reads a node.
    ///
    /// A `NodeId` must come from this graph. Like a slice index, an index from
    /// another graph is caller error and panics. Clone and JSON adapters never
    /// create dangling indices.
    pub fn node(&self, id: NodeId) -> &BoundaryNode {
        &self.nodes[id]
    }

    /// Creates a graph containing only a host error record.
    ///
    /// A distinct node kind prevents scripts from forging an object tag to reach
    /// the error-materialization path.
    pub fn host_error(name: &str, message: &str, stack: &str) -> Self {
        Self {
            nodes: vec![BoundaryNode::HostError {
                name: name.to_owned(),
                message: message.to_owned(),
                stack: stack.to_owned(),
            }],
            root: 0,
        }
    }
}

/// Explicit failures from boundary cloning and JSON adaptation.
#[derive(Debug, PartialEq)]
pub enum BoundaryError {
    /// Array length exceeds [`crate::MAX_BOUNDARY_ITEMS`].
    ///
    /// Callers must identify this by variant rather than a forgeable script tag.
    /// Oversized arrays always fail and are never silently truncated.
    TooManyItems { length: u64 },
    /// `length` is not a non-negative safe integer: it is negative, fractional,
    /// NaN, infinite, or greater than `2^53 - 1`.
    NonSafeIntegerLength { raw: f64 },
    /// Nesting exceeds [`MAX_BOUNDARY_DEPTH`].
    ///
    /// Rejection happens during construction. Once a deeper graph exists, any
    /// caller converting it to `serde_json::Value` could overflow during recursive
    /// destruction, when no error can be returned.
    TooDeep { depth: usize },
    /// JSON expansion would exceed [`MAX_JSON_OUTPUT_NODES`].
    OutputTooLarge { nodes: usize },
    /// The graph contains a cycle and cannot be emitted as reference-free JSON.
    Cycle,
    /// Reading a script-side source value failed, such as an accessor or Proxy
    /// trap exception. The original message is not classified.
    Source(String),
    /// The sink refused to create or populate a destination value. The original
    /// message is not classified.
    Sink(String),
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundaryError::TooManyItems { length } => write!(
                formatter,
                "Boundary array has {length} items, exceeding the limit of {MAX_BOUNDARY_ITEMS}"
            ),
            BoundaryError::NonSafeIntegerLength { raw } => {
                write!(formatter, "Array length is not a non-negative safe integer: {raw:?}")
            }
            BoundaryError::TooDeep { depth } => write!(
                formatter,
                "Boundary value is nested {depth} levels deep, exceeding the limit of {MAX_BOUNDARY_DEPTH}"
            ),
            BoundaryError::OutputTooLarge { nodes } => write!(
                formatter,
                "JSON expansion would produce more than {MAX_JSON_OUTPUT_NODES} nodes ({nodes} counted)"
            ),
            BoundaryError::Cycle => write!(formatter, "Boundary graph contains a cycle, which JSON cannot represent"),
            BoundaryError::Source(message) => write!(formatter, "Failed to read boundary source value: {message}"),
            BoundaryError::Sink(message) => write!(formatter, "Boundary sink failed: {message}"),
        }
    }
}

impl std::error::Error for BoundaryError {}

/// Engine interface for inbound materialization.
///
/// Containers are created empty and filled later so cycles retain identity: the
/// shell is memoized by `NodeId` before a child edge can point back to it.
pub trait SinkBuilder {
    type Value: Clone;

    fn null(&mut self) -> Result<Self::Value, String>;
    fn boolean(&mut self, value: bool) -> Result<Self::Value, String>;
    fn number(&mut self, value: f64) -> Result<Self::Value, String>;
    fn string(&mut self, value: &str) -> Result<Self::Value, String>;

    /// Creates an unfilled array whose identity remains stable when later passed
    /// to `push_element`.
    fn empty_array(&mut self) -> Result<Self::Value, String>;

    /// Creates an unfilled object. Safe implementations use a null prototype; the
    /// boundary also filters `"__proto__"` for defense in depth.
    fn empty_object(&mut self) -> Result<Self::Value, String>;

    fn push_element(
        &mut self,
        array: &Self::Value,
        element: &Self::Value,
    ) -> Result<(), String>;

    fn set_property(
        &mut self,
        object: &Self::Value,
        key: &str,
        value: &Self::Value,
    ) -> Result<(), String>;

    /// Materializes a trusted host error record rather than a mutable ordinary
    /// object.
    fn host_error(
        &mut self,
        name: &str,
        message: &str,
        stack: &str,
    ) -> Result<Self::Value, String>;
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_INTEGER_JSON_MAGNITUDE: f64 = 9_007_199_254_740_992.0;

enum OutWork<S> {
    Array {
        id: NodeId,
        source: S,
        length: usize,
        depth: usize,
    },
    Object {
        id: NodeId,
        source: S,
        depth: usize,
    },
}

fn allocate_source<S: SourceValue>(
    source: &S,
    depth: usize,
    nodes: &mut Vec<BoundaryNode>,
    visited: &mut HashMap<usize, NodeId>,
    work: &mut Vec<OutWork<S>>,
) -> Result<NodeId, BoundaryError> {
    let kind = source.kind();
    if matches!(kind, SourceKind::Array | SourceKind::Object) {
        if let Some(identity) = source.identity() {
            if let Some(id) = visited.get(&identity) {
                return Ok(*id);
            }
        }
        // Depth constrains containers only: scalars do not nest, and visited back
        // edges do not increase depth.
        if depth > MAX_BOUNDARY_DEPTH {
            return Err(BoundaryError::TooDeep { depth });
        }
    }

    let id = nodes.len();
    match kind {
        SourceKind::Undefined | SourceKind::Function | SourceKind::Null => {
            nodes.push(BoundaryNode::Null)
        }
        SourceKind::Bool => nodes.push(BoundaryNode::Bool(source.boolean())),
        SourceKind::Number => nodes.push(BoundaryNode::Number(source.number())),
        SourceKind::String => nodes.push(BoundaryNode::String(source.string())),
        SourceKind::Array => {
            nodes.push(BoundaryNode::Array(Vec::new()));
            if let Some(identity) = source.identity() {
                visited.insert(identity, id);
            }
            // Register identity before reading length so an accessor exposing this
            // array indirectly closes through the existing visited entry.
            let raw = source.array_length().map_err(BoundaryError::Source)?;
            if !raw.is_finite()
                || raw < 0.0
                || raw.fract() != 0.0
                || raw > MAX_SAFE_INTEGER
            {
                return Err(BoundaryError::NonSafeIntegerLength { raw });
            }
            let length = raw as u64;
            if length > MAX_BOUNDARY_ITEMS as u64 {
                return Err(BoundaryError::TooManyItems { length });
            }
            work.push(OutWork::Array {
                id,
                source: source.clone(),
                length: length as usize,
                depth,
            });
        }
        SourceKind::Object => {
            nodes.push(BoundaryNode::Object(Vec::new()));
            if let Some(identity) = source.identity() {
                visited.insert(identity, id);
            }
            work.push(OutWork::Object {
                id,
                source: source.clone(),
                depth,
            });
        }
    }
    Ok(id)
}

/// Clones a script-side source value into a data graph without executable
/// properties.
///
/// Roots cannot be omitted, so root `Undefined` and `Function` become `Null`.
/// Each array `length` is read exactly once. A non-negative-safe-integer length
/// returns [`BoundaryError::NonSafeIntegerLength`]; a length over
/// [`crate::MAX_BOUNDARY_ITEMS`] returns [`BoundaryError::TooManyItems`] before
/// any element is read. `Undefined` and `Function` array slots become `Null`, as
/// in `JSON.stringify`.
///
/// Objects skip `"__proto__"` and properties whose values are `Undefined` or
/// `Function`, ensuring scripts cannot send executable bodies or thenables to the
/// host. Containers register [`SourceValue::identity`] before filling, preserving
/// self-cycles, mutual cycles, and shared diamonds as one [`NodeId`]. Excessive
/// container nesting returns [`BoundaryError::TooDeep`], source exceptions remain
/// [`BoundaryError::Source`], and an explicit two-phase work stack avoids native
/// recursion.
pub fn clone_out<S: SourceValue>(root: &S) -> Result<BoundaryGraph, BoundaryError> {
    let mut nodes = Vec::new();
    let mut visited = HashMap::new();
    let mut work = Vec::new();
    let root = allocate_source(root, 1, &mut nodes, &mut visited, &mut work)?;

    while let Some(task) = work.pop() {
        match task {
            OutWork::Array {
                id,
                source,
                length,
                depth,
            } => {
                let mut elements = Vec::with_capacity(length);
                for index in 0..length {
                    let child = source.array_element(index).map_err(BoundaryError::Source)?;
                    elements.push(allocate_source(
                        &child,
                        depth + 1,
                        &mut nodes,
                        &mut visited,
                        &mut work,
                    )?);
                }
                nodes[id] = BoundaryNode::Array(elements);
            }
            OutWork::Object { id, source, depth } => {
                let mut properties = Vec::new();
                for (key, child) in source.entries().map_err(BoundaryError::Source)? {
                    if key == "__proto__"
                        || matches!(child.kind(), SourceKind::Undefined | SourceKind::Function)
                    {
                        continue;
                    }
                    let child_id = allocate_source(
                        &child,
                        depth + 1,
                        &mut nodes,
                        &mut visited,
                        &mut work,
                    )?;
                    properties.push((key, child_id));
                }
                nodes[id] = BoundaryNode::Object(properties);
            }
        }
    }

    Ok(BoundaryGraph { nodes, root })
}

fn invalid_node(id: NodeId) -> BoundaryError {
    BoundaryError::Sink(format!("Boundary graph references nonexistent node {id}"))
}

fn allocate_sink<B: SinkBuilder>(
    graph: &BoundaryGraph,
    id: NodeId,
    sink: &mut B,
    memo: &mut [Option<B::Value>],
    work: &mut Vec<NodeId>,
) -> Result<B::Value, BoundaryError> {
    let Some(node) = graph.nodes.get(id) else {
        return Err(invalid_node(id));
    };
    if let Some(value) = &memo[id] {
        return Ok(value.clone());
    }

    let (value, fill_later) = match node {
        BoundaryNode::Null => (sink.null().map_err(BoundaryError::Sink)?, false),
        BoundaryNode::Bool(value) => (
            sink.boolean(*value).map_err(BoundaryError::Sink)?,
            false,
        ),
        BoundaryNode::Number(value) => (
            sink.number(*value).map_err(BoundaryError::Sink)?,
            false,
        ),
        BoundaryNode::String(value) => (
            sink.string(value).map_err(BoundaryError::Sink)?,
            false,
        ),
        BoundaryNode::Array(items) => {
            if items.len() > MAX_BOUNDARY_ITEMS {
                return Err(BoundaryError::TooManyItems {
                    length: items.len() as u64,
                });
            }
            (
                sink.empty_array().map_err(BoundaryError::Sink)?,
                true,
            )
        }
        BoundaryNode::Object(_) => (
            sink.empty_object().map_err(BoundaryError::Sink)?,
            true,
        ),
        BoundaryNode::HostError {
            name,
            message,
            stack,
        } => (
            sink.host_error(name, message, stack)
                .map_err(BoundaryError::Sink)?,
            false,
        ),
    };
    memo[id] = Some(value.clone());
    if fill_later {
        work.push(id);
    }
    Ok(value)
}

/// Materializes the root-reachable graph into a sink, preserving cycles and shared
/// identity.
///
/// Arrays and objects are created as empty shells, memoized by `NodeId`, and filled
/// by an explicit work stack, so back-edges receive the already-registered value.
/// Arrays are limited to [`crate::MAX_BOUNDARY_ITEMS`] on ingress as well. A
/// `"__proto__"` property never reaches `set_property`, including in manually
/// assembled graphs, providing defense in depth with the sink's null prototype.
/// [`BoundaryNode::HostError`] uses only [`SinkBuilder::host_error`], all sink
/// string errors become [`BoundaryError::Sink`], and traversal uses no native
/// recursion.
pub fn clone_in<B: SinkBuilder>(
    graph: &BoundaryGraph,
    sink: &mut B,
) -> Result<B::Value, BoundaryError> {
    if graph.root >= graph.nodes.len() {
        return Err(invalid_node(graph.root));
    }
    let mut memo = vec![None; graph.nodes.len()];
    let mut work = Vec::new();
    let root = allocate_sink(graph, graph.root, sink, &mut memo, &mut work)?;

    while let Some(id) = work.pop() {
        let target = memo[id]
            .as_ref()
            .expect("container shell is memoized before fill")
            .clone();
        match graph.nodes[id].clone() {
            BoundaryNode::Array(items) => {
                for child_id in items {
                    let child = allocate_sink(graph, child_id, sink, &mut memo, &mut work)?;
                    sink.push_element(&target, &child)
                        .map_err(BoundaryError::Sink)?;
                }
            }
            BoundaryNode::Object(properties) => {
                for (key, child_id) in properties {
                    if key == "__proto__" {
                        continue;
                    }
                    let child = allocate_sink(graph, child_id, sink, &mut memo, &mut work)?;
                    sink.set_property(&target, &key, &child)
                        .map_err(BoundaryError::Sink)?;
                }
            }
            _ => unreachable!("only container nodes are scheduled for fill"),
        }
    }

    Ok(root)
}

enum JsonInputWork<'a> {
    Array {
        id: NodeId,
        value: &'a Value,
        depth: usize,
    },
    Object {
        id: NodeId,
        value: &'a Value,
        depth: usize,
    },
}

fn allocate_json_input<'a>(
    value: &'a Value,
    depth: usize,
    nodes: &mut Vec<BoundaryNode>,
    work: &mut Vec<JsonInputWork<'a>>,
) -> Result<NodeId, BoundaryError> {
    if matches!(value, Value::Array(_) | Value::Object(_)) && depth > MAX_BOUNDARY_DEPTH {
        return Err(BoundaryError::TooDeep { depth });
    }
    let id = nodes.len();
    match value {
        Value::Null => nodes.push(BoundaryNode::Null),
        Value::Bool(value) => nodes.push(BoundaryNode::Bool(*value)),
        Value::Number(value) => {
            // Under normal serde_json configuration, as_f64 always succeeds. The
            // fallback supports arbitrary_precision values while retaining the
            // boundary contract that script-domain numbers are f64.
            let value = value
                .as_f64()
                .unwrap_or_else(|| value.to_string().parse::<f64>().unwrap_or(f64::NAN));
            nodes.push(BoundaryNode::Number(value));
        }
        Value::String(value) => nodes.push(BoundaryNode::String(value.clone())),
        Value::Array(items) => {
            if items.len() > MAX_BOUNDARY_ITEMS {
                return Err(BoundaryError::TooManyItems {
                    length: items.len() as u64,
                });
            }
            nodes.push(BoundaryNode::Array(Vec::new()));
            work.push(JsonInputWork::Array { id, value, depth });
        }
        Value::Object(_) => {
            nodes.push(BoundaryNode::Object(Vec::new()));
            work.push(JsonInputWork::Object { id, value, depth });
        }
    }
    Ok(id)
}

/// Iteratively expands an acyclic JSON document into a boundary graph.
///
/// JSON has no references, so every occurrence creates a new node. Arrays remain
/// limited by [`crate::MAX_BOUNDARY_ITEMS`] and container nesting by
/// [`MAX_BOUNDARY_DEPTH`]; the latter protects programmatically constructed host
/// documents beyond serde_json's parser depth limit. `"__proto__"` keys are
/// dropped. Every JSON number becomes `f64`, matching the final script numeric
/// domain rather than introducing separate serde_json integer semantics. An
/// explicit work stack avoids native recursion for nested documents.
pub fn graph_from_json(value: &Value) -> Result<BoundaryGraph, BoundaryError> {
    let mut nodes = Vec::new();
    let mut work = Vec::new();
    let root = allocate_json_input(value, 1, &mut nodes, &mut work)?;

    while let Some(task) = work.pop() {
        match task {
            JsonInputWork::Array { id, value, depth } => {
                let items = value.as_array().expect("array task keeps an array");
                let mut children = Vec::with_capacity(items.len());
                for child in items {
                    children.push(allocate_json_input(child, depth + 1, &mut nodes, &mut work)?);
                }
                nodes[id] = BoundaryNode::Array(children);
            }
            JsonInputWork::Object { id, value, depth } => {
                let object = value.as_object().expect("object task keeps an object");
                let mut properties = Vec::with_capacity(object.len());
                for (key, child) in object {
                    if key == "__proto__" {
                        continue;
                    }
                    let child = allocate_json_input(child, depth + 1, &mut nodes, &mut work)?;
                    properties.push((key.clone(), child));
                }
                nodes[id] = BoundaryNode::Object(properties);
            }
        }
    }

    Ok(BoundaryGraph { nodes, root })
}

enum JsonOutputAction {
    Enter(NodeId),
    ExitArray { id: NodeId, length: usize },
    ExitObject { id: NodeId, keys: Vec<String> },
}

fn json_number(value: f64) -> Value {
    if !value.is_finite() {
        return Value::Null;
    }
    if value.fract() == 0.0 && value.abs() <= MAX_INTEGER_JSON_MAGNITUDE {
        if value >= 0.0 {
            return Value::Number(Number::from(value as u64));
        }
        return Value::Number(Number::from(value as i64));
    }
    Value::Number(Number::from_f64(value).expect("finite f64 is a JSON number"))
}

/// Converts a boundary graph to a JSON document.
///
/// JSON has no reference semantics. A gray node encountered again on the active
/// traversal stack is a cycle and returns [`BoundaryError::Cycle`]; a shared node
/// that has left the stack expands again, so shared diamonds are copied rather
/// than misidentified as cycles. The three-color decision and value assembly use
/// an explicit action stack.
///
/// Non-finite numbers become `Null` as in `JSON.stringify`. Finite integer values
/// use JSON integer form when `fract() == 0` and their absolute value is at most
/// `2^53`; other finite values use `from_f64`. [`BoundaryNode::HostError`] becomes
/// an ordinary `{name, message, stack}` object because its special frozen form is
/// only a script-sink concern. JSON expansion is limited by
/// [`MAX_JSON_OUTPUT_NODES`] to prevent exponential output from shared diamonds.
pub fn graph_to_json(graph: &BoundaryGraph) -> Result<Value, BoundaryError> {
    if graph.root >= graph.nodes.len() {
        return Err(invalid_node(graph.root));
    }
    let mut actions = vec![JsonOutputAction::Enter(graph.root)];
    let mut values = Vec::new();
    let mut emitted = 0usize;
    // The set contains gray nodes still on the traversal stack. Removing a node
    // restores it to white and permits expansion through another shared path; a
    // permanent visited set would misidentify diamonds as cycles or drop values.
    let mut active = HashSet::new();

    while let Some(action) = actions.pop() {
        match action {
            JsonOutputAction::Enter(id) => {
                emitted += 1;
                if emitted > MAX_JSON_OUTPUT_NODES {
                    return Err(BoundaryError::OutputTooLarge { nodes: emitted });
                }
                let Some(node) = graph.nodes.get(id) else {
                    return Err(invalid_node(id));
                };
                match node {
                    BoundaryNode::Null => values.push(Value::Null),
                    BoundaryNode::Bool(value) => values.push(Value::Bool(*value)),
                    BoundaryNode::Number(value) => values.push(json_number(*value)),
                    BoundaryNode::String(value) => values.push(Value::String(value.clone())),
                    BoundaryNode::Array(items) => {
                        if !active.insert(id) {
                            return Err(BoundaryError::Cycle);
                        }
                        actions.push(JsonOutputAction::ExitArray {
                            id,
                            length: items.len(),
                        });
                        for child in items.iter().rev() {
                            actions.push(JsonOutputAction::Enter(*child));
                        }
                    }
                    BoundaryNode::Object(properties) => {
                        if !active.insert(id) {
                            return Err(BoundaryError::Cycle);
                        }
                        let keys = properties.iter().map(|(key, _)| key.clone()).collect();
                        actions.push(JsonOutputAction::ExitObject { id, keys });
                        for (_, child) in properties.iter().rev() {
                            actions.push(JsonOutputAction::Enter(*child));
                        }
                    }
                    BoundaryNode::HostError {
                        name,
                        message,
                        stack,
                    } => {
                        let mut object = Map::new();
                        object.insert("name".into(), Value::String(name.clone()));
                        object.insert("message".into(), Value::String(message.clone()));
                        object.insert("stack".into(), Value::String(stack.clone()));
                        values.push(Value::Object(object));
                    }
                }
            }
            JsonOutputAction::ExitArray { id, length } => {
                let start = values
                    .len()
                    .checked_sub(length)
                    .expect("every array child produces one JSON value");
                let items = values.split_off(start);
                active.remove(&id);
                values.push(Value::Array(items));
            }
            JsonOutputAction::ExitObject { id, keys } => {
                let start = values
                    .len()
                    .checked_sub(keys.len())
                    .expect("every object child produces one JSON value");
                let children = values.split_off(start);
                let mut object = Map::new();
                for (key, child) in keys.into_iter().zip(children) {
                    object.insert(key, child);
                }
                active.remove(&id);
                values.push(Value::Object(object));
            }
        }
    }

    Ok(values.pop().expect("a valid graph root produces one value"))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use serde_json::json;

    use super::*;

    #[derive(Clone)]
    struct MockValue(Rc<MockNode>);

    struct MockNode {
        body: RefCell<MockBody>,
        length_reads: Cell<u32>,
    }

    enum MockBody {
        Undefined,
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array { length: f64, items: Vec<MockValue> },
        Object(Vec<(String, MockValue)>),
        Function,
    }

    impl MockValue {
        fn new(body: MockBody) -> Self {
            Self(Rc::new(MockNode {
                body: RefCell::new(body),
                length_reads: Cell::new(0),
            }))
        }

        fn null() -> Self {
            Self::new(MockBody::Null)
        }

        fn number(value: f64) -> Self {
            Self::new(MockBody::Number(value))
        }

        fn string(value: &str) -> Self {
            Self::new(MockBody::String(value.into()))
        }

        fn function() -> Self {
            Self::new(MockBody::Function)
        }

        fn array(length: f64, items: Vec<MockValue>) -> Self {
            Self::new(MockBody::Array { length, items })
        }

        fn object(entries: Vec<(&str, MockValue)>) -> Self {
            Self::new(MockBody::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.into(), value))
                    .collect(),
            ))
        }
    }

    impl SourceValue for MockValue {
        fn kind(&self) -> SourceKind {
            match &*self.0.body.borrow() {
                MockBody::Undefined => SourceKind::Undefined,
                MockBody::Null => SourceKind::Null,
                MockBody::Bool(_) => SourceKind::Bool,
                MockBody::Number(_) => SourceKind::Number,
                MockBody::String(_) => SourceKind::String,
                MockBody::Array { .. } => SourceKind::Array,
                MockBody::Object(_) => SourceKind::Object,
                MockBody::Function => SourceKind::Function,
            }
        }

        fn identity(&self) -> Option<usize> {
            if matches!(
                &*self.0.body.borrow(),
                MockBody::Array { .. } | MockBody::Object(_)
            ) {
                Some(Rc::as_ptr(&self.0) as usize)
            } else {
                None
            }
        }

        fn boolean(&self) -> bool {
            match &*self.0.body.borrow() {
                MockBody::Bool(value) => *value,
                _ => panic!("not a bool"),
            }
        }

        fn number(&self) -> f64 {
            match &*self.0.body.borrow() {
                MockBody::Number(value) => *value,
                _ => panic!("not a number"),
            }
        }

        fn string(&self) -> String {
            match &*self.0.body.borrow() {
                MockBody::String(value) => value.clone(),
                _ => panic!("not a string"),
            }
        }

        fn array_length(&self) -> Result<f64, String> {
            self.0.length_reads.set(self.0.length_reads.get() + 1);
            match &*self.0.body.borrow() {
                MockBody::Array { length, .. } => Ok(*length),
                _ => panic!("not an array"),
            }
        }

        fn array_element(&self, index: usize) -> Result<Self, String> {
            match &*self.0.body.borrow() {
                MockBody::Array { items, .. } => Ok(items[index].clone()),
                _ => panic!("not an array"),
            }
        }

        fn entries(&self) -> Result<Vec<(String, Self)>, String> {
            match &*self.0.body.borrow() {
                MockBody::Object(entries) => Ok(entries.clone()),
                _ => panic!("not an object"),
            }
        }
    }

    /// Hostile source that throws when read, modeling a script getter or Proxy trap.
    #[derive(Clone)]
    struct HostileValue(HostileMode);

    #[derive(Clone, Copy)]
    enum HostileMode {
        LengthThrows,
        ElementThrows,
        EntriesThrows,
    }

    impl SourceValue for HostileValue {
        fn kind(&self) -> SourceKind {
            match self.0 {
                HostileMode::LengthThrows | HostileMode::ElementThrows => SourceKind::Array,
                HostileMode::EntriesThrows => SourceKind::Object,
            }
        }

        fn identity(&self) -> Option<usize> {
            Some(self as *const Self as usize)
        }

        fn boolean(&self) -> bool {
            panic!("not a bool")
        }

        fn number(&self) -> f64 {
            panic!("not a number")
        }

        fn string(&self) -> String {
            panic!("not a string")
        }

        fn array_length(&self) -> Result<f64, String> {
            match self.0 {
                HostileMode::LengthThrows => Err("length trap threw".into()),
                HostileMode::ElementThrows => Ok(1.0),
                HostileMode::EntriesThrows => panic!("not an array"),
            }
        }

        fn array_element(&self, _: usize) -> Result<Self, String> {
            Err("element getter threw".into())
        }

        fn entries(&self) -> Result<Vec<(String, Self)>, String> {
            Err("entries trap threw".into())
        }
    }

    #[derive(Debug)]
    enum SinkNode {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<SinkValue>),
        Object(Vec<(String, SinkValue)>),
        HostError {
            name: String,
            message: String,
            stack: String,
        },
    }

    type SinkValue = Rc<RefCell<SinkNode>>;

    #[derive(Default)]
    struct MockSink {
        property_calls: Vec<String>,
        host_error_calls: Vec<(String, String, String)>,
    }

    impl SinkBuilder for MockSink {
        type Value = SinkValue;

        fn null(&mut self) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::Null)))
        }

        fn boolean(&mut self, value: bool) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::Bool(value))))
        }

        fn number(&mut self, value: f64) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::Number(value))))
        }

        fn string(&mut self, value: &str) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::String(value.into()))))
        }

        fn empty_array(&mut self) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::Array(Vec::new()))))
        }

        fn empty_object(&mut self) -> Result<Self::Value, String> {
            Ok(Rc::new(RefCell::new(SinkNode::Object(Vec::new()))))
        }

        fn push_element(
            &mut self,
            array: &Self::Value,
            element: &Self::Value,
        ) -> Result<(), String> {
            match &mut *array.borrow_mut() {
                SinkNode::Array(items) => {
                    items.push(element.clone());
                    Ok(())
                }
                _ => Err("target is not an array".into()),
            }
        }

        fn set_property(
            &mut self,
            object: &Self::Value,
            key: &str,
            value: &Self::Value,
        ) -> Result<(), String> {
            self.property_calls.push(key.into());
            match &mut *object.borrow_mut() {
                SinkNode::Object(properties) => {
                    properties.push((key.into(), value.clone()));
                    Ok(())
                }
                _ => Err("target is not an object".into()),
            }
        }

        fn host_error(
            &mut self,
            name: &str,
            message: &str,
            stack: &str,
        ) -> Result<Self::Value, String> {
            self.host_error_calls
                .push((name.into(), message.into(), stack.into()));
            Ok(Rc::new(RefCell::new(SinkNode::HostError {
                name: name.into(),
                message: message.into(),
                stack: stack.into(),
            })))
        }
    }

    #[test]
    fn scalar_payloads_materialize_in_the_sink_with_their_values_intact() {
        let source = MockValue::object(vec![
            ("flag", MockValue::new(MockBody::Bool(true))),
            ("count", MockValue::number(6.5)),
            ("text", MockValue::string("猫")),
        ]);
        let graph = clone_out(&source).expect("scalars clone outbound");
        let value = clone_in(&graph, &mut MockSink::default()).expect("scalars clone inbound");
        let SinkNode::Object(properties) = &*value.borrow() else {
            panic!("sink root should be object");
        };
        assert!(matches!(&*properties[0].1.borrow(), SinkNode::Bool(true)));
        assert!(
            matches!(&*properties[1].1.borrow(), SinkNode::Number(found) if *found == 6.5)
        );
        assert!(matches!(&*properties[2].1.borrow(), SinkNode::String(found) if found == "猫"));
    }

    #[test]
    fn both_directions_reject_4097_items_and_accept_exactly_4096() {
        let oversized = MockValue::array((MAX_BOUNDARY_ITEMS + 1) as f64, Vec::new());
        assert_eq!(
            clone_out(&oversized),
            Err(BoundaryError::TooManyItems {
                length: (MAX_BOUNDARY_ITEMS + 1) as u64
            })
        );

        let oversized_graph = BoundaryGraph {
            nodes: vec![
                BoundaryNode::Array(vec![1; MAX_BOUNDARY_ITEMS + 1]),
                BoundaryNode::Null,
            ],
            root: 0,
        };
        assert_eq!(
            clone_in(&oversized_graph, &mut MockSink::default()).unwrap_err(),
            BoundaryError::TooManyItems {
                length: (MAX_BOUNDARY_ITEMS + 1) as u64
            }
        );

        let exact = MockValue::array(
            MAX_BOUNDARY_ITEMS as f64,
            (0..MAX_BOUNDARY_ITEMS).map(|_| MockValue::null()).collect(),
        );
        let graph = clone_out(&exact).expect("4096 items are allowed outbound");
        let value = clone_in(&graph, &mut MockSink::default())
            .expect("4096 items are allowed inbound");
        assert!(matches!(
            &*value.borrow(),
            SinkNode::Array(items) if items.len() == MAX_BOUNDARY_ITEMS
        ));
    }

    #[test]
    fn a_self_cycle_round_trips_with_the_same_sink_identity() {
        let source = MockValue::object(Vec::new());
        *source.0.body.borrow_mut() = MockBody::Object(vec![("self".into(), source.clone())]);

        let graph = clone_out(&source).expect("cycle clones outbound");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties, &vec![("self".into(), graph.root())]);

        let value = clone_in(&graph, &mut MockSink::default()).expect("cycle clones inbound");
        let child = match &*value.borrow() {
            SinkNode::Object(properties) => properties[0].1.clone(),
            _ => panic!("sink root should be object"),
        };
        assert!(Rc::ptr_eq(&value, &child));
    }

    #[test]
    fn each_array_length_is_observed_exactly_once() {
        let source = MockValue::array(2.0, vec![MockValue::null(), MockValue::null()]);
        clone_out(&source).expect("array clones");
        assert_eq!(source.0.length_reads.get(), 1);
    }

    #[test]
    fn fractional_negative_too_large_and_nan_lengths_are_rejected() {
        for raw in [
            3.5,
            -1.0,
            9_007_199_254_740_992.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let error = clone_out(&MockValue::array(raw, Vec::new()))
                .expect_err("length is not a safe integer");
            assert!(
                matches!(error, BoundaryError::NonSafeIntegerLength { raw: found } if
                    (raw.is_nan() && found.is_nan()) || raw == found),
                "unexpected error: {error:?}"
            );
        }
    }

    #[test]
    fn container_nesting_beyond_the_boundary_depth_is_rejected_in_both_entry_paths() {
        // Exactly 1,000 containers pass and 1,001 are rejected. Their native
        // recursive destruction remains bounded, so the test need not use
        // mem::forget.
        let mut deep = MockValue::null();
        for _ in 0..MAX_BOUNDARY_DEPTH {
            deep = MockValue::array(1.0, vec![deep]);
        }
        assert!(clone_out(&deep).is_ok(), "恰好 1000 层容器应通过");
        let over = MockValue::array(1.0, vec![deep]);
        assert_eq!(
            clone_out(&over),
            Err(BoundaryError::TooDeep {
                depth: MAX_BOUNDARY_DEPTH + 1
            })
        );

        let mut json = Value::Null;
        for _ in 0..MAX_BOUNDARY_DEPTH {
            json = Value::Array(vec![json]);
        }
        assert!(graph_from_json(&json).is_ok(), "恰好 1000 层 JSON 应通过");
        let json_over = Value::Array(vec![json]);
        assert_eq!(
            graph_from_json(&json_over),
            Err(BoundaryError::TooDeep {
                depth: MAX_BOUNDARY_DEPTH + 1
            })
        );
    }

    #[test]
    fn exponential_diamond_sharing_is_stopped_by_the_json_output_budget() {
        // A 19-node diamond chain expands to 2^19 - 1 JSON values, so the output
        // budget must bound it.
        let levels = 18;
        let mut nodes = Vec::with_capacity(levels + 1);
        for id in 0..levels {
            nodes.push(BoundaryNode::Array(vec![id + 1, id + 1]));
        }
        nodes.push(BoundaryNode::Null);
        let graph = BoundaryGraph { nodes, root: 0 };
        assert!(matches!(
            graph_to_json(&graph),
            Err(BoundaryError::OutputTooLarge { .. })
        ));
    }

    #[test]
    fn hostile_source_reads_surface_as_source_errors_with_their_message() {
        assert_eq!(
            clone_out(&HostileValue(HostileMode::LengthThrows)),
            Err(BoundaryError::Source("length trap threw".into()))
        );
        assert_eq!(
            clone_out(&HostileValue(HostileMode::ElementThrows)),
            Err(BoundaryError::Source("element getter threw".into()))
        );
        assert_eq!(
            clone_out(&HostileValue(HostileMode::EntriesThrows)),
            Err(BoundaryError::Source("entries trap threw".into()))
        );
    }

    #[test]
    fn function_valued_object_properties_are_structurally_removed() {
        // The structural invariant removes executable properties from the host
        // graph instead of relying on source-level thenable rewriting.
        let source = MockValue::object(vec![
            ("run", MockValue::function()),
            ("data", MockValue::number(7.0)),
        ]);
        let graph = clone_out(&source).expect("object clones");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].0, "data");
    }

    #[test]
    fn proto_properties_are_removed_on_every_boundary_path() {
        let source = MockValue::object(vec![
            ("__proto__", MockValue::string("poison")),
            ("safe", MockValue::string("ok")),
        ]);
        let graph = clone_out(&source).expect("source object clones");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].0, "safe");

        let graph = graph_from_json(&json!({"__proto__": {"polluted": true}, "safe": 1}))
            .expect("JSON object clones");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].0, "safe");

        let graph = BoundaryGraph {
            nodes: vec![
                BoundaryNode::Object(vec![("__proto__".into(), 1), ("safe".into(), 1)]),
                BoundaryNode::Null,
            ],
            root: 0,
        };
        let mut sink = MockSink::default();
        clone_in(&graph, &mut sink).expect("manual graph clones");
        assert_eq!(sink.property_calls, vec!["safe"]);
    }

    #[test]
    fn diamond_sharing_is_preserved_in_the_graph_but_copied_in_json() {
        let shared = MockValue::object(vec![("value", MockValue::number(1.0))]);
        let source = MockValue::object(vec![("left", shared.clone()), ("right", shared)]);
        let graph = clone_out(&source).expect("diamond clones");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties[0].1, properties[1].1, "shared node is not copied");
        assert_eq!(
            graph_to_json(&graph).expect("diamond is not a cycle"),
            json!({"left": {"value": 1}, "right": {"value": 1}})
        );

        let cycle = BoundaryGraph {
            nodes: vec![BoundaryNode::Array(vec![0])],
            root: 0,
        };
        assert_eq!(graph_to_json(&cycle), Err(BoundaryError::Cycle));
    }

    #[test]
    fn host_errors_use_the_dedicated_sink_and_become_plain_json_objects() {
        let graph = BoundaryGraph::host_error("TypeError", "bad input", "line 1");
        let mut sink = MockSink::default();
        let value = clone_in(&graph, &mut sink).expect("host error clones inbound");
        assert_eq!(
            sink.host_error_calls,
            vec![("TypeError".into(), "bad input".into(), "line 1".into())]
        );
        assert!(matches!(
            &*value.borrow(),
            SinkNode::HostError { name, message, stack }
                if name == "TypeError" && message == "bad input" && stack == "line 1"
        ));
        assert_eq!(
            graph_to_json(&graph).expect("host error converts to JSON"),
            json!({"name": "TypeError", "message": "bad input", "stack": "line 1"})
        );
    }

    #[test]
    fn a_nested_json_document_round_trips_with_integer_spelling_intact() {
        let document = json!({
            "object": {"integer": 42, "fraction": 1.25, "text": "猫", "none": null},
            "array": [true, false, 0, -7, 2.5, {"nested": [1, 2, 3]}]
        });
        let graph = graph_from_json(&document).expect("JSON converts to graph");
        let output = graph_to_json(&graph).expect("graph converts to JSON");
        assert_eq!(
            serde_json::to_vec(&output).expect("output serializes"),
            serde_json::to_vec(&document).expect("input serializes")
        );
    }

    #[test]
    fn a_six_thousand_level_chain_does_not_use_the_native_stack() {
        let depth = 6_000;
        let mut nodes = Vec::with_capacity(depth + 1);
        for id in 0..depth {
            nodes.push(BoundaryNode::Array(vec![id + 1]));
        }
        nodes.push(BoundaryNode::Null);
        let graph = BoundaryGraph { nodes, root: 0 };

        let sink_value = clone_in(&graph, &mut MockSink::default())
            .expect("deep graph clones without recursive descent");
        assert!(matches!(&*sink_value.borrow(), SinkNode::Array(items) if items.len() == 1));
        let json_value =
            graph_to_json(&graph).expect("deep graph converts without recursive descent");
        assert!(matches!(json_value, Value::Array(_)));

        // Rc chains and serde_json::Value use recursive destruction. Leaking test
        // outputs prevents dependency Drop behavior from being counted as traversal
        // behavior under test.
        std::mem::forget(sink_value);
        std::mem::forget(json_value);
    }

    #[test]
    fn undefined_and_function_array_slots_become_null_nodes() {
        let source = MockValue::array(
            3.0,
            vec![
                MockValue::new(MockBody::Undefined),
                MockValue::function(),
                MockValue::new(MockBody::Bool(true)),
            ],
        );
        let graph = clone_out(&source).expect("array clones");
        assert_eq!(
            graph_to_json(&graph).expect("graph converts"),
            json!([null, null, true])
        );
    }

    #[test]
    fn sink_string_errors_are_wrapped_without_losing_the_message() {
        struct FailingSink;
        impl SinkBuilder for FailingSink {
            type Value = ();
            fn null(&mut self) -> Result<(), String> {
                Err("engine stopped".into())
            }
            fn boolean(&mut self, _: bool) -> Result<(), String> {
                unreachable!()
            }
            fn number(&mut self, _: f64) -> Result<(), String> {
                unreachable!()
            }
            fn string(&mut self, _: &str) -> Result<(), String> {
                unreachable!()
            }
            fn empty_array(&mut self) -> Result<(), String> {
                unreachable!()
            }
            fn empty_object(&mut self) -> Result<(), String> {
                unreachable!()
            }
            fn push_element(&mut self, _: &(), _: &()) -> Result<(), String> {
                unreachable!()
            }
            fn set_property(&mut self, _: &(), _: &str, _: &()) -> Result<(), String> {
                unreachable!()
            }
            fn host_error(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
                unreachable!()
            }
        }

        let graph = graph_from_json(&Value::Null).expect("null graph");
        assert_eq!(
            clone_in(&graph, &mut FailingSink),
            Err(BoundaryError::Sink("engine stopped".into()))
        );
    }

    #[test]
    fn graph_from_json_rejects_4097_items_and_accepts_exactly_4096() {
        let oversized = Value::Array(vec![Value::Null; MAX_BOUNDARY_ITEMS + 1]);
        assert_eq!(
            graph_from_json(&oversized),
            Err(BoundaryError::TooManyItems {
                length: (MAX_BOUNDARY_ITEMS + 1) as u64
            })
        );

        let exact = Value::Array(vec![Value::Null; MAX_BOUNDARY_ITEMS]);
        let graph = graph_from_json(&exact).expect("4096 JSON items are allowed");
        assert!(matches!(
            graph.node(graph.root()),
            BoundaryNode::Array(items) if items.len() == MAX_BOUNDARY_ITEMS
        ));
    }

    #[test]
    fn diamond_sharing_materializes_as_the_same_sink_identity() {
        let shared = MockValue::object(vec![("value", MockValue::number(1.0))]);
        let source = MockValue::object(vec![("left", shared.clone()), ("right", shared)]);
        let graph = clone_out(&source).expect("diamond clones outbound");
        let value = clone_in(&graph, &mut MockSink::default()).expect("diamond clones inbound");
        let (left, right) = {
            let root = value.borrow();
            let SinkNode::Object(properties) = &*root else {
                panic!("sink root should be object");
            };
            (properties[0].1.clone(), properties[1].1.clone())
        };
        assert!(Rc::ptr_eq(&left, &right));
    }

    #[test]
    fn a_shared_array_referenced_twice_has_its_length_observed_once() {
        let shared = MockValue::array(0.0, Vec::new());
        let source = MockValue::object(vec![
            ("left", shared.clone()),
            ("right", shared.clone()),
        ]);
        let graph = clone_out(&source).expect("shared array clones");

        assert_eq!(shared.0.length_reads.get(), 1);
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };
        assert_eq!(properties[0].1, properties[1].1);
    }

    #[test]
    fn undefined_and_function_roots_become_null_nodes() {
        for root in [
            MockValue::new(MockBody::Undefined),
            MockValue::function(),
        ] {
            let graph = clone_out(&root).expect("root clones");
            assert_eq!(graph.node(graph.root()), &BoundaryNode::Null);
        }
    }

    #[test]
    fn undefined_valued_object_properties_are_structurally_removed() {
        let source = MockValue::object(vec![
            ("gone", MockValue::new(MockBody::Undefined)),
            ("kept", MockValue::number(1.0)),
        ]);
        let graph = clone_out(&source).expect("object clones");
        let BoundaryNode::Object(properties) = graph.node(graph.root()) else {
            panic!("root should be object");
        };

        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].0, "kept");
        assert_eq!(graph.node(properties[0].1), &BoundaryNode::Number(1.0));
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FailingSinkMethod {
        EmptyArray,
        PushElement,
        SetProperty,
        HostError,
    }

    struct ContainerPathFailingSink {
        method: FailingSinkMethod,
        message: &'static str,
    }

    impl ContainerPathFailingSink {
        fn fail_at(method: FailingSinkMethod, message: &'static str) -> Self {
            Self { method, message }
        }

        fn fail_if(&self, method: FailingSinkMethod) -> Result<(), String> {
            if self.method == method {
                Err(self.message.into())
            } else {
                Ok(())
            }
        }
    }

    impl SinkBuilder for ContainerPathFailingSink {
        type Value = ();

        fn null(&mut self) -> Result<Self::Value, String> {
            Ok(())
        }

        fn boolean(&mut self, _: bool) -> Result<Self::Value, String> {
            Ok(())
        }

        fn number(&mut self, _: f64) -> Result<Self::Value, String> {
            Ok(())
        }

        fn string(&mut self, _: &str) -> Result<Self::Value, String> {
            Ok(())
        }

        fn empty_array(&mut self) -> Result<Self::Value, String> {
            self.fail_if(FailingSinkMethod::EmptyArray)
        }

        fn empty_object(&mut self) -> Result<Self::Value, String> {
            Ok(())
        }

        fn push_element(&mut self, _: &Self::Value, _: &Self::Value) -> Result<(), String> {
            self.fail_if(FailingSinkMethod::PushElement)
        }

        fn set_property(
            &mut self,
            _: &Self::Value,
            _: &str,
            _: &Self::Value,
        ) -> Result<(), String> {
            self.fail_if(FailingSinkMethod::SetProperty)
        }

        fn host_error(
            &mut self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Self::Value, String> {
            self.fail_if(FailingSinkMethod::HostError)
        }
    }

    #[test]
    fn container_path_sink_errors_are_wrapped_without_losing_their_messages() {
        let cases = [
            (
                graph_from_json(&json!([])).expect("empty array graph"),
                FailingSinkMethod::EmptyArray,
                "empty_array failed verbatim",
            ),
            (
                graph_from_json(&json!([null])).expect("nonempty array graph"),
                FailingSinkMethod::PushElement,
                "push_element failed verbatim",
            ),
            (
                graph_from_json(&json!({"value": null})).expect("object graph"),
                FailingSinkMethod::SetProperty,
                "set_property failed verbatim",
            ),
            (
                BoundaryGraph::host_error("Error", "message", "stack"),
                FailingSinkMethod::HostError,
                "host_error failed verbatim",
            ),
        ];

        for (graph, method, message) in cases {
            let mut sink = ContainerPathFailingSink::fail_at(method, message);
            assert_eq!(
                clone_in(&graph, &mut sink),
                Err(BoundaryError::Sink(message.into()))
            );
        }
    }

    fn cloned_number_as_json(value: f64) -> Value {
        let graph = clone_out(&MockValue::number(value)).expect("number clones outbound");
        graph_to_json(&graph).expect("number graph converts to JSON")
    }

    #[test]
    fn json_number_conversion_nulls_non_finite_values_and_uses_the_two_to_the_53_integer_boundary() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(cloned_number_as_json(value), Value::Null);
        }

        let Value::Number(positive_boundary) = cloned_number_as_json(9_007_199_254_740_992.0)
        else {
            panic!("positive 2^53 should be a JSON number");
        };
        assert_eq!(positive_boundary.as_u64(), Some(9_007_199_254_740_992));
        assert_eq!(positive_boundary.as_i64(), Some(9_007_199_254_740_992));

        let Value::Number(negative_boundary) = cloned_number_as_json(-9_007_199_254_740_992.0)
        else {
            panic!("negative 2^53 should be a JSON number");
        };
        assert_eq!(negative_boundary.as_i64(), Some(-9_007_199_254_740_992));
        assert_eq!(negative_boundary.as_u64(), None);

        let Value::Number(beyond_boundary) = cloned_number_as_json(18_014_398_509_481_984.0)
        else {
            panic!("2^54 should be a JSON number");
        };
        assert_eq!(beyond_boundary.as_i64(), None);
        assert_eq!(beyond_boundary.as_u64(), None);
        assert_eq!(beyond_boundary.as_f64(), Some(18_014_398_509_481_984.0));
    }
}
