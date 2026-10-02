//! A small builder for Core ML "ML Program" (MIL) functions and their
//! serialization into the Model protobuf.
//!
//! Every op carries its output type, so shapes are inferred here for the ops
//! the Qwen3.5 graph uses. Parameters are emitted as `const` ops, like
//! coremltools does; large tensors live in the weight blob file and are
//! referenced by offset.

use std::collections::HashMap;

use super::proto::Msg;

pub const OPSET: &str = "CoreML8"; // iOS 18 / macOS 15: states and multifunction models
pub const SPEC_VERSION: u64 = 9;
pub const WEIGHT_FILE: &str = "@model_path/weights/weight.bin";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    F16,
    F32,
    I32,
    Bool,
    Str,
}

impl DType {
    fn mil(self) -> i64 {
        match self {
            Self::Bool => 1,
            Self::Str => 2,
            Self::F16 => 10,
            Self::F32 => 11,
            Self::I32 => 23,
        }
    }

    /// `ArrayFeatureType.ArrayDataType` for model inputs and outputs.
    fn feature(self) -> i64 {
        match self {
            Self::F16 => 65552,
            Self::F32 => 65568,
            Self::I32 => 131104,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Var(usize);

#[derive(Clone, Debug)]
struct VarInfo {
    name: String,
    dtype: DType,
    shape: Vec<usize>,
    state: bool,
}

#[derive(Clone, Debug)]
pub enum Value {
    F16(Vec<u16>),
    I32(Vec<i32>),
    Bool(Vec<bool>),
    Str(String),
    /// Offset of the blob's metadata record in the weight file.
    Blob(u64),
}

struct Op {
    kind: &'static str,
    name: String,
    inputs: Vec<(&'static str, Vec<Var>)>,
    outputs: Vec<Var>,
    value: Option<Value>,
}

pub struct Function {
    pub name: String,
    vars: Vec<VarInfo>,
    ops: Vec<Op>,
    inputs: Vec<Var>,
    states: Vec<Var>,
    outputs: Vec<Var>,
    counters: HashMap<&'static str, usize>,
}

fn broadcast(a: &[usize], b: &[usize]) -> Vec<usize> {
    let rank = a.len().max(b.len());
    (0..rank)
        .map(|i| {
            let da = if i + a.len() >= rank { a[i + a.len() - rank] } else { 1 };
            let db = if i + b.len() >= rank { b[i + b.len() - rank] } else { 1 };
            assert!(da == db || da == 1 || db == 1, "cannot broadcast {a:?} with {b:?}");
            da.max(db)
        })
        .collect()
}

fn axis(axis: i64, rank: usize) -> usize {
    if axis < 0 {
        (rank as i64 + axis) as usize
    } else {
        axis as usize
    }
}

impl Function {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            vars: Vec::new(),
            ops: Vec::new(),
            inputs: Vec::new(),
            states: Vec::new(),
            outputs: Vec::new(),
            counters: HashMap::new(),
        }
    }

    pub fn shape(&self, var: Var) -> &[usize] {
        &self.vars[var.0].shape
    }

    pub fn dtype(&self, var: Var) -> DType {
        self.vars[var.0].dtype
    }

    fn fresh(&mut self, prefix: &'static str) -> String {
        let n = self.counters.entry(prefix).or_insert(0);
        let name = format!("{prefix}_{n}");
        *n += 1;
        name
    }

    fn var(&mut self, name: String, dtype: DType, shape: Vec<usize>, state: bool) -> Var {
        self.vars.push(VarInfo { name, dtype, shape, state });
        Var(self.vars.len() - 1)
    }

    pub fn input(&mut self, name: &str, shape: &[usize]) -> Var {
        let var = self.var(name.to_string(), DType::F16, shape.to_vec(), false);
        self.inputs.push(var);
        var
    }

    pub fn state(&mut self, name: &str, shape: &[usize]) -> Var {
        let var = self.var(name.to_string(), DType::F16, shape.to_vec(), true);
        self.states.push(var);
        var
    }

    /// Marks `var` as a function output under `name` (through an identity op).
    pub fn output(&mut self, var: Var, name: &str) {
        let out = self.var(name.to_string(), self.dtype(var), self.shape(var).to_vec(), false);
        self.ops.push(Op { kind: "identity", name: name.to_string(), inputs: vec![("x", vec![var])], outputs: vec![out], value: None });
        self.outputs.push(out);
    }

    fn constant(&mut self, dtype: DType, shape: Vec<usize>, value: Value) -> Var {
        let name = self.fresh("c");
        let out = self.var(name.clone(), dtype, shape, false);
        self.ops.push(Op { kind: "const", name, inputs: Vec::new(), outputs: vec![out], value: Some(value) });
        out
    }

    pub fn scalar(&mut self, value: f32) -> Var {
        self.constant(DType::F16, Vec::new(), Value::F16(vec![crate::safetensors::f32_to_f16(value)]))
    }

    pub fn tensor_f16(&mut self, shape: &[usize], values: Vec<u16>) -> Var {
        assert_eq!(shape.iter().product::<usize>(), values.len());
        self.constant(DType::F16, shape.to_vec(), Value::F16(values))
    }

    pub fn blob(&mut self, shape: &[usize], offset: u64) -> Var {
        self.constant(DType::F16, shape.to_vec(), Value::Blob(offset))
    }

    fn ints(&mut self, values: &[i64]) -> Var {
        self.constant(DType::I32, vec![values.len()], Value::I32(values.iter().map(|v| *v as i32).collect()))
    }

    fn int(&mut self, value: i64) -> Var {
        self.constant(DType::I32, Vec::new(), Value::I32(vec![value as i32]))
    }

    fn flag(&mut self, value: bool) -> Var {
        self.constant(DType::Bool, Vec::new(), Value::Bool(vec![value]))
    }

    fn text(&mut self, value: &str) -> Var {
        self.constant(DType::Str, Vec::new(), Value::Str(value.to_string()))
    }

    fn op(&mut self, kind: &'static str, inputs: Vec<(&'static str, Vec<Var>)>, shapes: Vec<Vec<usize>>) -> Vec<Var> {
        let name = self.fresh(kind);
        let dtype = inputs.first().map(|(_, vars)| self.dtype(vars[0])).unwrap_or(DType::F16);
        let outputs: Vec<Var> = shapes
            .into_iter()
            .enumerate()
            .map(|(i, shape)| {
                let var_name = if i == 0 { name.clone() } else { format!("{name}_{i}") };
                self.var(var_name, dtype, shape, false)
            })
            .collect();
        self.ops.push(Op { kind, name, inputs, outputs: outputs.clone(), value: None });
        outputs
    }

    fn one(&mut self, kind: &'static str, inputs: Vec<(&'static str, Vec<Var>)>, shape: Vec<usize>) -> Var {
        self.op(kind, inputs, vec![shape])[0]
    }

    fn binary(&mut self, kind: &'static str, x: Var, y: Var) -> Var {
        let shape = broadcast(self.shape(x), self.shape(y));
        self.one(kind, vec![("x", vec![x]), ("y", vec![y])], shape)
    }

    pub fn mul(&mut self, x: Var, y: Var) -> Var {
        self.binary("mul", x, y)
    }

    pub fn add(&mut self, x: Var, y: Var) -> Var {
        self.binary("add", x, y)
    }

    pub fn sub(&mut self, x: Var, y: Var) -> Var {
        self.binary("sub", x, y)
    }

    pub fn minimum(&mut self, x: Var, y: Var) -> Var {
        self.binary("minimum", x, y)
    }

    pub fn mul_s(&mut self, x: Var, value: f32) -> Var {
        let c = self.scalar(value);
        self.mul(x, c)
    }

    pub fn add_s(&mut self, x: Var, value: f32) -> Var {
        let c = self.scalar(value);
        self.add(x, c)
    }

    fn unary(&mut self, kind: &'static str, x: Var) -> Var {
        let shape = self.shape(x).to_vec();
        self.one(kind, vec![("x", vec![x])], shape)
    }

    pub fn tanh(&mut self, x: Var) -> Var {
        self.unary("tanh", x)
    }

    pub fn exp(&mut self, x: Var) -> Var {
        self.unary("exp", x)
    }

    pub fn relu(&mut self, x: Var) -> Var {
        self.unary("relu", x)
    }

    pub fn softplus(&mut self, x: Var) -> Var {
        self.unary("softplus", x)
    }

    pub fn softmax(&mut self, x: Var, axis: i64) -> Var {
        let shape = self.shape(x).to_vec();
        let a = self.int(axis);
        self.one("softmax", vec![("x", vec![x]), ("axis", vec![a])], shape)
    }

    pub fn concat(&mut self, values: &[Var], ax: i64) -> Var {
        let rank = self.shape(values[0]).len();
        let a = axis(ax, rank);
        let mut shape = self.shape(values[0]).to_vec();
        shape[a] = values.iter().map(|v| self.shape(*v)[a]).sum();
        let axis_var = self.int(ax);
        let interleave = self.flag(false);
        self.one("concat", vec![("values", values.to_vec()), ("axis", vec![axis_var]), ("interleave", vec![interleave])], shape)
    }

    pub fn split(&mut self, x: Var, sizes: &[usize], ax: i64) -> Vec<Var> {
        let rank = self.shape(x).len();
        let a = axis(ax, rank);
        assert_eq!(sizes.iter().sum::<usize>(), self.shape(x)[a]);
        let shapes = sizes
            .iter()
            .map(|size| {
                let mut shape = self.shape(x).to_vec();
                shape[a] = *size;
                shape
            })
            .collect();
        let sizes_var = self.ints(&sizes.iter().map(|s| *s as i64).collect::<Vec<_>>());
        let axis_var = self.int(ax);
        self.op("split", vec![("x", vec![x]), ("split_sizes", vec![sizes_var]), ("axis", vec![axis_var])], shapes)
    }

    pub fn layer_norm(&mut self, x: Var, ax: i64, epsilon: f32) -> Var {
        let shape = self.shape(x).to_vec();
        let axes = self.ints(&[ax]);
        let eps = self.scalar(epsilon);
        self.one("layer_norm", vec![("x", vec![x]), ("axes", vec![axes]), ("epsilon", vec![eps])], shape)
    }

    /// 2-D convolution, stride 1, valid padding. `weight` is [Cout, Cin/groups, kh, kw].
    pub fn conv(&mut self, x: Var, weight: Var, bias: Option<Var>, groups: usize) -> Var {
        let xs = self.shape(x).to_vec();
        let ws = self.shape(weight).to_vec();
        assert_eq!(xs.len(), 4);
        assert_eq!(xs[1], ws[1] * groups, "conv input channels");
        let shape = vec![xs[0], ws[0], xs[2] - ws[2] + 1, xs[3] - ws[3] + 1];
        let strides = self.ints(&[1, 1]);
        let pad_type = self.text("valid");
        let pad = self.ints(&[0, 0, 0, 0]);
        let dilations = self.ints(&[1, 1]);
        let groups_var = self.int(groups as i64);
        let mut inputs = vec![
            ("x", vec![x]),
            ("weight", vec![weight]),
            ("strides", vec![strides]),
            ("pad_type", vec![pad_type]),
            ("pad", vec![pad]),
            ("dilations", vec![dilations]),
            ("groups", vec![groups_var]),
        ];
        if let Some(bias) = bias {
            inputs.push(("bias", vec![bias]));
        }
        self.one("conv", inputs, shape)
    }

    pub fn reshape(&mut self, x: Var, shape: &[usize]) -> Var {
        assert_eq!(shape.iter().product::<usize>(), self.shape(x).iter().product::<usize>(), "reshape size");
        let s = self.ints(&shape.iter().map(|v| *v as i64).collect::<Vec<_>>());
        self.one("reshape", vec![("x", vec![x]), ("shape", vec![s])], shape.to_vec())
    }

    pub fn transpose(&mut self, x: Var, perm: &[usize]) -> Var {
        let xs = self.shape(x).to_vec();
        let shape = perm.iter().map(|p| xs[*p]).collect();
        let p = self.ints(&perm.iter().map(|v| *v as i64).collect::<Vec<_>>());
        self.one("transpose", vec![("x", vec![x]), ("perm", vec![p])], shape)
    }

    pub fn slice(&mut self, x: Var, begin: &[usize], end: &[usize]) -> Var {
        let shape = begin.iter().zip(end).map(|(b, e)| e - b).collect();
        let b = self.ints(&begin.iter().map(|v| *v as i64).collect::<Vec<_>>());
        let e = self.ints(&end.iter().map(|v| *v as i64).collect::<Vec<_>>());
        self.one("slice_by_index", vec![("x", vec![x]), ("begin", vec![b]), ("end", vec![e])], shape)
    }

    pub fn reduce_sum(&mut self, x: Var, ax: i64) -> Var {
        let rank = self.shape(x).len();
        let mut shape = self.shape(x).to_vec();
        shape[axis(ax, rank)] = 1;
        let axes = self.ints(&[ax]);
        let keep = self.flag(true);
        self.one("reduce_sum", vec![("x", vec![x]), ("axes", vec![axes]), ("keep_dims", vec![keep])], shape)
    }

    pub fn matmul(&mut self, x: Var, y: Var, transpose_x: bool, transpose_y: bool) -> Var {
        let xs = self.shape(x).to_vec();
        let ys = self.shape(y).to_vec();
        let (m, kx) = if transpose_x { (xs[xs.len() - 1], xs[xs.len() - 2]) } else { (xs[xs.len() - 2], xs[xs.len() - 1]) };
        let (ky, n) = if transpose_y { (ys[ys.len() - 1], ys[ys.len() - 2]) } else { (ys[ys.len() - 2], ys[ys.len() - 1]) };
        assert_eq!(kx, ky, "matmul inner dimension");
        let mut shape = broadcast(&xs[..xs.len() - 2], &ys[..ys.len() - 2]);
        shape.push(m);
        shape.push(n);
        let tx = self.flag(transpose_x);
        let ty = self.flag(transpose_y);
        self.one("matmul", vec![("x", vec![x]), ("y", vec![y]), ("transpose_x", vec![tx]), ("transpose_y", vec![ty])], shape)
    }

    pub fn read_state(&mut self, state: Var) -> Var {
        let shape = self.shape(state).to_vec();
        self.one("read_state", vec![("input", vec![state])], shape)
    }

    /// Writes `value` into `state` and returns a fresh read of it. On the Neural
    /// Engine the written value may feed nothing but the write, and the read
    /// back must be consumed, or the program fails to compile.
    pub fn update_state(&mut self, state: Var, value: Var) -> Var {
        assert_eq!(self.shape(state), self.shape(value), "state shape");
        self.op("write_state", vec![("data", vec![value]), ("input", vec![state])], Vec::new());
        self.read_state(state)
    }
}

// ---------------------------------------------------------------- serialization

fn tensor_type(dtype: DType, shape: &[usize]) -> Msg {
    let mut tensor = Msg::new();
    tensor.int(1, dtype.mil());
    if !shape.is_empty() {
        tensor.int(2, shape.len() as i64);
        for dim in shape {
            let mut constant = Msg::new();
            constant.uint(1, *dim as u64);
            let mut dimension = Msg::new();
            dimension.message(1, &constant);
            tensor.message(3, &dimension);
        }
    }
    let mut value_type = Msg::new();
    value_type.message(1, &tensor);
    value_type
}

fn value_type(info: &VarInfo) -> Msg {
    let tensor = tensor_type(info.dtype, &info.shape);
    if !info.state {
        return tensor;
    }
    let mut state = Msg::new();
    state.message(1, &tensor);
    let mut value_type = Msg::new();
    value_type.message(5, &state);
    value_type
}

fn named_value_type(info: &VarInfo) -> Msg {
    let mut msg = Msg::new();
    msg.string(1, &info.name).message(2, &value_type(info));
    msg
}

fn string_value(text: &str) -> Msg {
    let mut strings = Msg::new();
    strings.string(1, text);
    let mut tensor = Msg::new();
    tensor.message(4, &strings);
    let mut immediate = Msg::new();
    immediate.message(1, &tensor);
    let mut value = Msg::new();
    value.message(2, &tensor_type(DType::Str, &[])).message(3, &immediate);
    value
}

fn const_value(info: &VarInfo, data: &Value) -> Msg {
    let mut value = Msg::new();
    value.message(2, &tensor_type(info.dtype, &info.shape));
    if let Value::Blob(offset) = data {
        let mut blob = Msg::new();
        blob.string(1, WEIGHT_FILE).uint(2, *offset);
        value.message(5, &blob);
        return value;
    }
    let mut tensor = Msg::new();
    match data {
        Value::F16(values) => {
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let mut repeated = Msg::new();
            repeated.bytes(1, &bytes);
            tensor.message(7, &repeated);
        }
        Value::I32(values) => {
            let mut repeated = Msg::new();
            repeated.packed_i32(1, values);
            tensor.message(2, &repeated);
        }
        Value::Bool(values) => {
            let mut repeated = Msg::new();
            repeated.packed_bool(1, values);
            tensor.message(3, &repeated);
        }
        Value::Str(text) => {
            let mut repeated = Msg::new();
            repeated.string(1, text);
            tensor.message(4, &repeated);
        }
        Value::Blob(_) => unreachable!(),
    }
    let mut immediate = Msg::new();
    immediate.message(1, &tensor);
    value.message(3, &immediate);
    value
}

impl Function {
    fn operation(&self, op: &Op) -> Msg {
        let mut msg = Msg::new();
        msg.string(1, op.kind);
        for (param, args) in &op.inputs {
            let mut argument = Msg::new();
            for arg in args {
                let mut binding = Msg::new();
                binding.string(1, &self.vars[arg.0].name);
                argument.message(1, &binding);
            }
            msg.map_entry(2, param, &argument);
        }
        for out in &op.outputs {
            msg.message(3, &named_value_type(&self.vars[out.0]));
        }
        msg.map_entry(5, "name", &string_value(&op.name));
        if let Some(data) = &op.value {
            msg.map_entry(5, "val", &const_value(&self.vars[op.outputs[0].0], data));
        }
        msg
    }

    fn to_proto(&self) -> Msg {
        let mut block = Msg::new();
        for out in &self.outputs {
            block.string(2, &self.vars[out.0].name);
        }
        for op in &self.ops {
            block.message(3, &self.operation(op));
        }
        let mut function = Msg::new();
        for var in self.inputs.iter().chain(&self.states) {
            function.message(1, &named_value_type(&self.vars[var.0]));
        }
        function.string(2, OPSET);
        function.map_entry(3, OPSET, &block);
        function
    }

    fn feature(&self, var: Var) -> Msg {
        let info = &self.vars[var.0];
        let mut array = Msg::new();
        array.packed_i64(1, &info.shape.iter().map(|d| *d as i64).collect::<Vec<_>>());
        array.int(2, info.dtype.feature());
        let mut feature_type = Msg::new();
        if info.state {
            let mut state = Msg::new();
            state.message(1, &array);
            feature_type.message(8, &state);
        } else {
            feature_type.message(5, &array);
        }
        let mut description = Msg::new();
        description.string(1, &info.name).message(3, &feature_type);
        description
    }

    fn description(&self) -> Msg {
        let mut msg = Msg::new();
        msg.string(1, &self.name);
        for var in &self.inputs {
            msg.message(2, &self.feature(*var));
        }
        for var in &self.outputs {
            msg.message(3, &self.feature(*var));
        }
        for var in &self.states {
            msg.message(6, &self.feature(*var));
        }
        msg
    }
}

/// The model specification for `functions` (the first one is the default).
pub fn model_spec(functions: &[Function], metadata: &[(&str, &str)]) -> Vec<u8> {
    let default = &functions[0];
    let mut description = Msg::new();
    if functions.len() == 1 {
        for var in &default.inputs {
            description.message(1, &default.feature(*var));
        }
        for var in &default.outputs {
            description.message(10, &default.feature(*var));
        }
        for var in &default.states {
            description.message(13, &default.feature(*var));
        }
    } else {
        // Multifunction models describe features per function only.
        for function in functions {
            description.message(20, &function.description());
        }
        description.string(21, &default.name);
    }
    let mut meta = Msg::new();
    for (key, value) in metadata {
        let mut entry = Msg::new();
        entry.string(1, key).string(2, value);
        meta.message(100, &entry);
    }
    description.message(100, &meta);

    let mut program = Msg::new();
    program.int(1, 1);
    for function in functions {
        program.map_entry(2, &function.name, &function.to_proto());
    }

    let mut model = Msg::new();
    model.uint(1, SPEC_VERSION).message(2, &description).message(502, &program);
    model.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_shapes() {
        let mut f = Function::new("main");
        let x = f.input("x", &[4, 1024, 1, 1]);
        let w = f.blob(&[2048, 1024, 1, 1], 64);
        let y = f.conv(x, w, None, 1);
        assert_eq!(f.shape(y), [4, 2048, 1, 1]);
        let r = f.reshape(y, &[4, 16, 1, 128]);
        let m = f.matmul(r, r, true, false);
        assert_eq!(f.shape(m), [4, 16, 128, 128]);
        let parts = f.split(y, &[1024, 1024], 1);
        assert_eq!(f.shape(parts[1]), [4, 1024, 1, 1]);
        let s = f.state("s", &[4, 16, 128, 128]);
        let read = f.update_state(s, m);
        let half = f.mul_s(read, 0.5);
        assert_eq!(f.shape(half), [4, 16, 128, 128]);
        let row = f.slice(read, &[0, 0, 0, 0], &[4, 16, 1, 128]);
        let summed = f.reduce_sum(row, -1);
        assert_eq!(f.shape(summed), [4, 16, 1, 1]);
        let joined = f.concat(&[row, row], 2);
        assert_eq!(f.shape(joined), [4, 16, 2, 128]);
        f.output(summed, "out");
        assert!(!model_spec(&[f], &[("k", "v")]).is_empty());
    }
}
