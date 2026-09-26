//! Core ML calls: compile a package, load one function of it for the Neural
//! Engine, run predictions with states, and read or write state buffers.
//!
//! Everything here is used from the scheduler's single thread; the wrappers
//! are `Send` so the backend can move to that thread, never shared.

use std::cell::RefCell;
use std::path::Path;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration,
    MLMultiArray, MLMultiArrayDataType, MLState,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};

/// A dense fp16 tensor on the host.
#[derive(Clone, Debug)]
pub struct Tensor16 {
    pub shape: Vec<usize>,
    pub data: Vec<u16>,
}

impl Tensor16 {
    pub fn zeros(shape: &[usize]) -> Self {
        Self { shape: shape.to_vec(), data: vec![0; shape.iter().product()] }
    }

    pub fn filled(shape: &[usize], value: u16) -> Self {
        Self { shape: shape.to_vec(), data: vec![value; shape.iter().product()] }
    }
}

fn describe(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::from_file_path(path).expect("absolute path")
}

/// Cores of this Mac's Neural Engine, if Core ML can use one (none on Intel
/// Macs and in virtual machines).
///
/// `MLAllComputeDevices` is macOS 14+; it is looked up at run time so the app
/// still starts on macOS 13, where this answers `None`.
pub fn neural_engine_cores() -> Option<usize> {
    use objc2_core_ml::{MLComputeDeviceProtocol, MLNeuralEngineComputeDevice};
    type AllDevices = unsafe extern "C-unwind" fn() -> *mut NSArray<ProtocolObject<dyn MLComputeDeviceProtocol>>;
    let name = c"MLAllComputeDevices";
    // SAFETY: looks up a symbol of the already-loaded Core ML framework.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    autoreleasepool(|_| {
        // SAFETY: the symbol is Core ML's `MLAllComputeDevices`, which takes no
        // arguments and returns an autoreleased array.
        let all: AllDevices = unsafe { std::mem::transmute::<*mut std::ffi::c_void, AllDevices>(symbol) };
        let devices = unsafe { Retained::retain_autoreleased(all()) }?;
        devices.iter().find_map(|device| {
            let object: &AnyObject = device.as_ref();
            let engine = object.downcast_ref::<MLNeuralEngineComputeDevice>()?;
            // SAFETY: a property read on a live device object.
            Some(unsafe { engine.totalCoreCount() }.max(0) as usize)
        })
    })
}

/// Compiles `package` (an `.mlpackage`) into `dest` (an `.mlmodelc`),
/// replacing what is there.
pub fn compile(package: &Path, dest: &Path) -> Result<(), String> {
    autoreleasepool(|_| {
        #[allow(deprecated)]
        let compiled = unsafe { MLModel::compileModelAtURL_error(&url(package)) }
            .map_err(|error| format!("Core ML 编译模型失败: {}", describe(&error)))?;
        let temp = compiled.to_file_path().ok_or("Core ML 返回的编译路径无效")?;
        let _ = std::fs::remove_dir_all(dest);
        if std::fs::rename(&temp, dest).is_err() {
            // The temporary directory may be on another volume.
            copy_dir(&temp, dest).map_err(|error| format!("无法放置编译后的模型: {error}"))?;
            let _ = std::fs::remove_dir_all(&temp);
        }
        Ok(())
    })
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub struct Function {
    model: Retained<MLModel>,
    pub name: String,
}

// SAFETY: a Function is created, used and dropped on the scheduler thread
// only; `Send` lets the owning backend be moved there once.
unsafe impl Send for Function {}

pub struct State(Retained<MLState>);

// SAFETY: as for `Function`.
unsafe impl Send for State {}

fn numbers(values: &[usize]) -> Retained<NSArray<NSNumber>> {
    let items: Vec<Retained<NSNumber>> = values.iter().map(|v| NSNumber::new_isize(*v as isize)).collect();
    NSArray::from_retained_slice(&items)
}

fn read_numbers(array: &NSArray<NSNumber>) -> Vec<usize> {
    array.iter().map(|n| n.as_isize() as usize).collect()
}

/// Copies between a dense buffer and a strided fp16 buffer of the same shape.
fn strided_copy(shape: &[usize], strides: &[usize], strided: *mut u16, dense: &mut [u16], to_strided: bool) {
    let rank = shape.len();
    let count: usize = shape.iter().product();
    assert_eq!(dense.len(), count);
    if count == 0 {
        return;
    }
    let inner = shape[rank - 1];
    let inner_stride = strides[rank - 1];
    let mut index = vec![0usize; rank];
    let mut offset_dense = 0;
    loop {
        let base: usize = index.iter().zip(strides).map(|(i, s)| i * s).sum();
        // SAFETY: `base + j * inner_stride` indexes inside the array Core ML
        // described with these shape and strides.
        unsafe {
            if inner_stride == 1 {
                let p = strided.add(base);
                if to_strided {
                    std::ptr::copy_nonoverlapping(dense.as_ptr().add(offset_dense), p, inner);
                } else {
                    std::ptr::copy_nonoverlapping(p, dense.as_mut_ptr().add(offset_dense), inner);
                }
            } else {
                for j in 0..inner {
                    let p = strided.add(base + j * inner_stride);
                    if to_strided {
                        *p = dense[offset_dense + j];
                    } else {
                        dense[offset_dense + j] = *p;
                    }
                }
            }
        }
        offset_dense += inner;
        // Advance the multi-index over all axes but the last.
        let mut axis = rank - 1;
        loop {
            if axis == 0 {
                return;
            }
            axis -= 1;
            index[axis] += 1;
            if index[axis] < shape[axis] {
                break;
            }
            index[axis] = 0;
        }
    }
}

fn to_multiarray(tensor: &Tensor16) -> Result<Retained<MLMultiArray>, String> {
    let array = unsafe {
        MLMultiArray::initWithShape_dataType_error(MLMultiArray::alloc(), &numbers(&tensor.shape), MLMultiArrayDataType::Float16)
    }
    .map_err(|error| format!("无法创建输入张量: {}", describe(&error)))?;
    let data = RefCell::new(tensor.data.clone());
    let shape = tensor.shape.clone();
    let block = RcBlock::new(move |bytes: NonNull<std::ffi::c_void>, _size: isize, strides: NonNull<NSArray<NSNumber>>| {
        let strides = read_numbers(unsafe { strides.as_ref() });
        strided_copy(&shape, &strides, bytes.as_ptr().cast(), &mut data.borrow_mut(), true);
    });
    unsafe { array.getMutableBytesWithHandler(&block) };
    Ok(array)
}

fn from_multiarray(array: &MLMultiArray) -> Tensor16 {
    let shape = read_numbers(&*unsafe { array.shape() });
    let strides = read_numbers(&*unsafe { array.strides() });
    let out = RefCell::new(Tensor16::zeros(&shape));
    let block = RcBlock::new(|bytes: NonNull<std::ffi::c_void>, _size: isize| {
        let mut out = out.borrow_mut();
        let shape = out.shape.clone();
        strided_copy(&shape, &strides, bytes.as_ptr().cast(), &mut out.data, false);
    });
    unsafe { array.getBytesWithHandler(&block) };
    drop(block);
    out.into_inner()
}

impl Function {
    pub fn load(compiled: &Path, name: &str) -> Result<Self, String> {
        autoreleasepool(|_| {
            let config = unsafe { MLModelConfiguration::new() };
            unsafe {
                config.setComputeUnits(MLComputeUnits::CPUAndNeuralEngine);
                config.setFunctionName(Some(&NSString::from_str(name)));
            }
            let model = unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url(compiled), &config) }
                .map_err(|error| format!("Core ML 加载 {name} 失败: {}", describe(&error)))?;
            Ok(Self { model, name: name.to_string() })
        })
    }

    pub fn new_state(&self) -> State {
        State(unsafe { self.model.newState() })
    }

    pub fn predict(&self, inputs: &[(&str, &Tensor16)], state: Option<&State>, output: &str) -> Result<Tensor16, String> {
        autoreleasepool(|_| {
            let mut keys: Vec<Retained<NSString>> = Vec::new();
            let mut values: Vec<Retained<AnyObject>> = Vec::new();
            for (name, tensor) in inputs {
                let array = to_multiarray(tensor)?;
                let value = unsafe { MLFeatureValue::featureValueWithMultiArray(&array) };
                keys.push(NSString::from_str(name));
                values.push(Retained::into_super(Retained::into_super(value)));
            }
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let dictionary = NSDictionary::from_retained_objects(&key_refs, &values);
            let provider = unsafe {
                MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dictionary)
            }
            .map_err(|error| format!("Core ML 输入无效: {}", describe(&error)))?;
            let provider = ProtocolObject::<dyn MLFeatureProvider>::from_ref(&*provider);
            let result = match state {
                Some(state) => unsafe { self.model.predictionFromFeatures_usingState_error(provider, &state.0) },
                None => unsafe { self.model.predictionFromFeatures_error(provider) },
            }
            .map_err(|error| format!("Core ML 推理失败（{}）: {}", self.name, describe(&error)))?;
            let value = unsafe { result.featureValueForName(&NSString::from_str(output)) }
                .ok_or_else(|| format!("Core ML 没有输出 {output}"))?;
            let array = unsafe { value.multiArrayValue() }.ok_or_else(|| format!("{output} 不是张量"))?;
            Ok(from_multiarray(&array))
        })
    }
}

impl State {
    /// Calls `f(pointer, shape, strides)` on the state buffer `name`.
    fn with_buffer(&self, name: &str, f: &mut dyn FnMut(*mut u16, &[usize], &[usize])) {
        autoreleasepool(|_| {
            let f = RefCell::new(f);
            let handler = RcBlock::new(|array: NonNull<MLMultiArray>| {
                let array = unsafe { array.as_ref() };
                let shape = read_numbers(&*unsafe { array.shape() });
                let block = RcBlock::new(|bytes: NonNull<std::ffi::c_void>, _size: isize, strides: NonNull<NSArray<NSNumber>>| {
                    let strides = read_numbers(unsafe { strides.as_ref() });
                    (f.borrow_mut())(bytes.as_ptr().cast(), &shape, &strides);
                });
                unsafe { array.getMutableBytesWithHandler(&block) };
            });
            unsafe { self.0.getMultiArrayForStateNamed_handler(&NSString::from_str(name), &handler) };
        })
    }

    /// Rows `0..rows` along axis 2 of batch entry `batch` of a rank-4 state,
    /// as a dense `[1, d1, rows, d3]` tensor.
    pub fn read_block(&self, name: &str, batch: usize, rows: usize) -> Tensor16 {
        let mut out = None;
        self.with_buffer(name, &mut |ptr, shape, strides| {
            assert_eq!(shape.len(), 4);
            let rows = rows.min(shape[2]);
            let mut tensor = Tensor16::zeros(&[1, shape[1], rows, shape[3]]);
            let base = unsafe { ptr.add(batch * strides[0]) };
            strided_copy(&[shape[1], rows, shape[3]], &strides[1..], base, &mut tensor.data, false);
            out = Some(tensor);
        });
        out.expect("state handler ran")
    }

    /// Writes a dense `[1, d1, r, d3]` block into batch entry `batch`, rows `0..r`.
    pub fn write_block(&self, name: &str, batch: usize, block: &Tensor16) {
        let mut data = block.data.clone();
        self.with_buffer(name, &mut |ptr, shape, strides| {
            assert_eq!(shape.len(), 4);
            assert!(block.shape[1] == shape[1] && block.shape[3] == shape[3] && block.shape[2] <= shape[2], "state block shape");
            let base = unsafe { ptr.add(batch * strides[0]) };
            strided_copy(&[block.shape[1], block.shape[2], block.shape[3]], &strides[1..], base, &mut data, true);
        });
    }

    /// Zeroes batch entry `batch` entirely.
    pub fn clear(&self, name: &str, batch: usize) {
        self.with_buffer(name, &mut |ptr, shape, strides| {
            let mut zeros = vec![0u16; shape[1..].iter().product()];
            let base = unsafe { ptr.add(batch * strides[0]) };
            strided_copy(&shape[1..], &strides[1..], base, &mut zeros, true);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::strided_copy;

    #[test]
    fn copies_through_padded_strides() {
        // shape [2, 3] stored with a row stride of 4
        let mut storage = vec![0u16; 8];
        let mut dense = vec![1, 2, 3, 4, 5, 6];
        strided_copy(&[2, 3], &[4, 1], storage.as_mut_ptr(), &mut dense, true);
        assert_eq!(storage, [1, 2, 3, 0, 4, 5, 6, 0]);
        let mut back = vec![0u16; 6];
        strided_copy(&[2, 3], &[4, 1], storage.as_mut_ptr(), &mut back, false);
        assert_eq!(back, [1, 2, 3, 4, 5, 6]);
        // a non-unit innermost stride
        let mut storage = vec![0u16; 6];
        let mut dense = vec![7, 8, 9];
        strided_copy(&[3], &[2], storage.as_mut_ptr(), &mut dense, true);
        assert_eq!(storage, [7, 0, 8, 0, 9, 0]);
    }
}
