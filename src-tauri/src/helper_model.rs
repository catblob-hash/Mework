//! The local helper model inside the app: a small model (Qwen3.5-0.8B, see
//! the `mework-local-model` crate) that names conversations and explains
//! shell commands without calling any provider.
//!
//! There is one build of the model per inference backend (`catalog`): on a
//! Mac the Neural Engine build and the MLX build, offered by what the chip
//! and system can run (`machine`); elsewhere the llama.cpp build. The user
//! downloads one or more and picks the one in use. This module installs them
//! (download, verify, finish on the device), owns the running service of the
//! active one, reports status to the renderer, and holds the two uses in
//! `uses`. Everything is opt-in from Appearance settings; nothing is
//! downloaded or loaded until the user turns a use on and picks a build.

mod catalog;
mod download;
mod machine;
pub(crate) mod uses;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use local_model::prompts::{default_prompt, Task};
use local_model::scheduler::Limits;
use local_model::service::{PromptInfo, Service, ServiceConfig};
use serde::{Deserialize, Serialize};

pub use catalog::VariantId;
pub use machine::{Machine, Unavailable};

use crate::model::{GlobalSettings, ResolvedLanguage};
use crate::push_events::{AppEventHub, AppPushEvent};

const CACHE_DIR: &str = "prompt-cache";
/// In a variant's directory once it is fully installed: its catalog version.
const MARKER: &str = "installed.json";
/// In the root: the variant in use.
const ACTIVE: &str = "active.json";
/// Positions per sequence: the prompt (up to ~750 tokens), a 256-token
/// request and the reply.
const CONTEXT: usize = 1024;

/// Where one variant's install is, for the renderer.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum Phase {
    /// Not downloaded.
    Missing,
    /// This machine cannot run it.
    Unsupported { reason: Unavailable },
    #[serde(rename_all = "camelCase")]
    Downloading { received: u64, total: u64, source: String },
    /// Finishing the download into the backend's form (compile, convert, verify).
    #[serde(rename_all = "camelCase")]
    Preparing { step: String, done: u64, total: u64 },
    Ready,
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantStatus {
    pub id: VariantId,
    #[serde(flatten)]
    pub phase: Phase,
    pub download_bytes: u64,
    /// Bytes on disk, prompt caches included.
    pub disk_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub machine: Machine,
    /// Every build this app knows, best first.
    pub variants: Vec<VariantStatus>,
    /// The build in use, if one is installed and picked.
    pub active: Option<VariantId>,
    /// The build to offer first on this machine.
    pub recommended: Option<VariantId>,
    /// The active build is loading and caching its prompts (the first load
    /// of the Neural Engine build compiles it for this Mac: minutes).
    pub warming: bool,
    /// Where the active build runs once loaded, e.g. "Apple Neural Engine".
    pub device: Option<String>,
    pub loaded: bool,
    pub running: usize,
    pub queued: usize,
    pub slots: usize,
    pub context: usize,
    pub disk_bytes: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptReport {
    pub tokens: usize,
    pub cache_bytes: u64,
    pub max_tokens: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultPrompts {
    pub title: String,
    pub shell: String,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    version: String,
}

#[derive(Serialize, Deserialize)]
struct ActiveFile {
    variant: VariantId,
}

struct Inner {
    root: Option<PathBuf>,
    machine: Machine,
    phases: BTreeMap<VariantId, Phase>,
    active: Option<VariantId>,
    /// The active variant's service, started on first use.
    service: Option<Service>,
    installing: Option<(VariantId, Arc<AtomicBool>)>,
    warming: bool,
    /// Bumped on every switch, so a warm-up of a model no longer active
    /// does not clear the flag of the new one.
    generation: u64,
    _pressure: Option<local_model::pressure::Watch>,
    last_status_push: Option<Instant>,
}

/// A title the host just wrote, so a renderer commit built before the
/// renderer heard about it does not put the old title back.
struct RecentTitle {
    replaced: String,
    title: String,
    at: Instant,
}

pub struct HelperModel {
    inner: Mutex<Inner>,
    recent_titles: Mutex<HashMap<String, RecentTitle>>,
    /// Conversations whose title is being generated.
    titles_in_flight: Mutex<std::collections::HashSet<String>>,
}

impl Default for HelperModel {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                root: None,
                machine: Machine::default(),
                phases: BTreeMap::new(),
                active: None,
                service: None,
                installing: None,
                warming: false,
                generation: 0,
                _pressure: None,
                last_status_push: None,
            }),
            recent_titles: Mutex::new(HashMap::new()),
            titles_in_flight: Mutex::new(Default::default()),
        }
    }
}

// ---------------------------------------------------------------- backends

/// Per-variant steps that differ by backend.
mod backends {
    use std::path::{Path, PathBuf};

    use local_model::gguf::GGUF_FILE;
    use local_model::mlx::METALLIB_FILE;
    use local_model::scheduler::Loader;

    use super::catalog::{self, VariantId};
    use super::CONTEXT;

    /// The backend's own files are in place (the marker says the rest is).
    pub fn is_built(id: VariantId, dir: &Path) -> bool {
        let common = dir.join("tokenizer.json").exists() && dir.join("config.json").exists();
        common
            && match id {
                VariantId::Ane => dir.join("model.mlmodelc").join("coremldata.bin").exists() && dir.join("embedding.json").exists(),
                VariantId::Mlx => {
                    use local_model::mlx::weights::{INDEX_FILE, WEIGHTS_FILE};
                    [INDEX_FILE, WEIGHTS_FILE, METALLIB_FILE].iter().all(|name| dir.join(name).exists())
                }
                VariantId::Llama => dir.join(GGUF_FILE).exists(),
            }
    }

    /// The package's weight file, kept only if the compiled copy differs.
    const ANE_EMBEDDING_FALLBACK: &str = "embedding-weights.bin";
    const ANE_PACKAGE: &str = "model.mlpackage";
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    const ANE_PACKAGE_WEIGHTS: &str = "model.mlpackage/Data/com.apple.CoreML/weights/weight.bin";
    #[cfg(target_os = "macos")]
    const SHAPES: local_model::coreml::graph::Shapes =
        local_model::coreml::graph::Shapes { slots: 4, chunk: 16, context: CONTEXT };

    /// Turns the downloaded files into what the backend loads.
    pub fn prepare(id: VariantId, dir: &Path, progress: &mut dyn FnMut(&str, u64, u64)) -> Result<(), String> {
        match id {
            VariantId::Ane => prepare_ane(dir, progress),
            VariantId::Mlx | VariantId::Llama => Ok(()),
        }
    }

    /// Core ML compiles the package for this Mac (seconds). The compiled
    /// model's weight file is normally the package's byte for byte; then the
    /// package goes, and embedding lookups read the compiled copy.
    #[cfg(target_os = "macos")]
    fn prepare_ane(dir: &Path, progress: &mut dyn FnMut(&str, u64, u64)) -> Result<(), String> {
        use local_model::coreml::backend::compiled_weights;
        let package = dir.join(ANE_PACKAGE);
        let compiled = dir.join("model.mlmodelc");
        progress("compile", 0, 1);
        local_model::coreml::runtime::compile(&package, &compiled)?;
        progress("verify", 0, 1);
        let expected = catalog::files(VariantId::Ane)
            .files
            .into_iter()
            .find(|file| file.local == ANE_PACKAGE_WEIGHTS)
            .map(|file| file.sha256);
        let actual = super::download::sha256_file(&compiled_weights(&compiled)).map_err(|error| format!("无法校验编译结果: {error}"))?;
        if expected.as_deref() != Some(actual.as_str()) {
            std::fs::rename(dir.join(ANE_PACKAGE_WEIGHTS), dir.join(ANE_EMBEDDING_FALLBACK))
                .map_err(|error| format!("无法保留词嵌入: {error}"))?;
        }
        let _ = std::fs::remove_dir_all(&package);
        progress("verify", 1, 1);
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn prepare_ane(_dir: &Path, _progress: &mut dyn FnMut(&str, u64, u64)) -> Result<(), String> {
        Err("此平台不支持神经网络引擎版".into())
    }

    /// Puts this Mac's own copy of the MLX build's `mlx.metallib` (`file`,
    /// its catalog entry) in `dir` if it has one, so it need not be
    /// downloaded: Python installs of the same MLX release carry the file,
    /// and a development checkout keeps it beside the MLX library. Only a
    /// byte-identical file counts.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub fn provide_metallib(dir: &Path, file: &super::download::RemoteFile) -> bool {
        let is_it = |path: &Path| {
            std::fs::metadata(path).is_ok_and(|meta| meta.len() == file.size)
                && super::download::sha256_file(path).is_ok_and(|digest| digest == file.sha256)
        };
        let target = dir.join(&file.local);
        if is_it(&target) {
            return true;
        }
        let Some(found) = metallib_candidates().into_iter().find(|candidate| is_it(candidate)) else {
            return false;
        };
        let staging = dir.join(format!("{METALLIB_FILE}.copying"));
        let copied = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::copy(&found, &staging))
            .and_then(|_| std::fs::rename(&staging, &target));
        match copied {
            Ok(()) => {
                eprintln!("[local-model] using {} instead of downloading it", found.display());
                true
            }
            Err(error) => {
                eprintln!("[local-model] could not copy {}: {error}", found.display());
                let _ = std::fs::remove_file(&staging);
                false
            }
        }
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    pub fn provide_metallib(_dir: &Path, _file: &super::download::RemoteFile) -> bool {
        false
    }

    /// Where `mlx.metallib` of a pip-installed MLX would be.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn metallib_candidates() -> Vec<PathBuf> {
        const SITE: &str = "lib/python3*/site-packages/mlx/lib/mlx.metallib";
        let mut patterns = vec![
            format!("/opt/homebrew/{SITE}"),
            format!("/usr/local/{SITE}"),
            format!("/Library/Frameworks/Python.framework/Versions/*/{SITE}"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            let under_home = [
                "Library/Python/*/lib/python/site-packages/mlx/lib/mlx.metallib".to_string(),
                format!(".venv/{SITE}"),
                format!(".pyenv/versions/*/{SITE}"),
                format!(".local/share/uv/python/*/{SITE}"),
                ".cache/uv/archive-v0/*/mlx/lib/mlx.metallib".to_string(),
                format!("miniconda3/{SITE}"),
                format!("miniconda3/envs/*/{SITE}"),
                format!("miniforge3/{SITE}"),
                format!("miniforge3/envs/*/{SITE}"),
                format!("anaconda3/{SITE}"),
                format!("anaconda3/envs/*/{SITE}"),
            ];
            patterns.extend(under_home.iter().map(|pattern| home.join(pattern).to_string_lossy().into_owned()));
        }
        let mut found: Vec<PathBuf> =
            local_model::mlx::shim_path().map(|shim| shim.with_file_name(METALLIB_FILE)).into_iter().collect();
        for pattern in patterns {
            found.extend(expand(Path::new(&pattern)));
        }
        found
    }

    /// The existing paths matching `pattern`, where a `*` in a component
    /// matches any run of characters.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn expand(pattern: &Path) -> Vec<PathBuf> {
        let mut found = vec![PathBuf::new()];
        for component in pattern.components() {
            let name = component.as_os_str().to_string_lossy();
            found = match name.split_once('*') {
                None => found.into_iter().map(|path| path.join(component)).filter(|path| path.exists()).collect(),
                Some((prefix, suffix)) => found
                    .iter()
                    .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        let name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
                        name.len() >= prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
                    })
                    .collect(),
            };
        }
        found
    }

    /// Files a finished install no longer needs (an interrupted cleanup).
    pub fn tidy(id: VariantId, dir: &Path) {
        if id == VariantId::Ane {
            let _ = std::fs::remove_dir_all(dir.join(ANE_PACKAGE));
        }
    }

    pub fn loader(id: VariantId, dir: PathBuf) -> Loader {
        Box::new(move || match id {
            #[cfg(target_os = "macos")]
            VariantId::Ane => {
                use local_model::coreml::backend::{compiled_weights, AneBackend};
                use local_model::coreml::package::{EmbeddingLayout, PackagePlan};
                let config = local_model::qwen35::Config::load(&dir.join("config.json"))?;
                let plan = PackagePlan::standard(&config, SHAPES);
                let layout = EmbeddingLayout::load(&dir.join("embedding.json"))?;
                let compiled = dir.join("model.mlmodelc");
                let fallback = dir.join(ANE_EMBEDDING_FALLBACK);
                let weights = if fallback.exists() { fallback } else { compiled_weights(&compiled) };
                let backend = AneBackend::load(&compiled, &weights, &layout, config, &plan, &mut |_, _| {})?;
                Ok(Box::new(backend) as Box<dyn local_model::engine::Backend>)
            }
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            VariantId::Mlx => {
                let backend = local_model::mlx::MlxBackend::load(&dir, SHAPES.slots, CONTEXT)?;
                Ok(Box::new(backend) as Box<dyn local_model::engine::Backend>)
            }
            #[cfg(not(target_os = "macos"))]
            VariantId::Llama => {
                use local_model::llama::{LlamaBackend, LlamaOptions};
                let options = LlamaOptions { context: CONTEXT, ..LlamaOptions::default() };
                Ok(Box::new(LlamaBackend::load(&dir.join(GGUF_FILE), options)?) as Box<dyn local_model::engine::Backend>)
            }
            #[allow(unreachable_patterns)]
            _ => Err("此平台不支持这个模型版本".into()),
        })
    }

    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    mod tests {
        use super::*;

        #[test]
        fn expands_wildcards_to_existing_paths() {
            let root = tempfile::tempdir().unwrap();
            for dir in ["py/3.12/lib/python3.12/site-packages/mlx/lib", "py/3.13/lib/python3.13/site-packages/other", "py/x"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            std::fs::write(root.path().join("py/3.12/lib/python3.12/site-packages/mlx/lib/mlx.metallib"), b"").unwrap();
            let pattern = root.path().join("py/*/lib/python3*/site-packages/mlx/lib/mlx.metallib");
            assert_eq!(expand(&pattern), [root.path().join("py/3.12/lib/python3.12/site-packages/mlx/lib/mlx.metallib")]);
            assert!(expand(&root.path().join("nothing/*/here")).is_empty());
        }
    }
}

// ---------------------------------------------------------------- settings

/// The effective prompt for `task`: the user's text, or the built-in one in
/// the app's language.
pub fn prompt_for(settings: &GlobalSettings, task: Task) -> String {
    let prefs = &settings.appearance.local_model;
    let custom = match task {
        Task::Title => &prefs.title_prompt,
        Task::Shell => &prefs.shell_prompt,
    };
    if custom.trim().is_empty() {
        default_prompt(task, language(settings)).to_string()
    } else {
        custom.clone()
    }
}

fn language(settings: &GlobalSettings) -> &'static str {
    match settings.resolved_app_language {
        ResolvedLanguage::ZhCn => "zh-CN",
        ResolvedLanguage::EnUs => "en-US",
    }
}

pub fn default_prompts(settings: &GlobalSettings) -> DefaultPrompts {
    let language = language(settings);
    DefaultPrompts {
        title: default_prompt(Task::Title, language).to_string(),
        shell: default_prompt(Task::Shell, language).to_string(),
    }
}

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|entries| entries.flatten().map(|entry| dir_size(&entry.path())).sum()).unwrap_or(0)
}

fn is_installed(id: VariantId, dir: &Path) -> bool {
    let version = std::fs::read_to_string(dir.join(MARKER))
        .ok()
        .and_then(|text| serde_json::from_str::<Marker>(&text).ok())
        .map(|marker| marker.version);
    version.as_deref() == Some(catalog::files(id).version.as_str()) && backends::is_built(id, dir)
}

fn is_cancel(error: &str) -> bool {
    error == download::CANCELLED
}

// ---------------------------------------------------------------- state

impl HelperModel {
    fn root(&self) -> Result<PathBuf, String> {
        self.inner.lock().expect("helper model").root.clone().ok_or_else(|| "本地模型尚未初始化".into())
    }

    fn resting_phase(machine: &Machine, id: VariantId, root: &Path) -> Phase {
        if let Err(reason) = machine.check(id) {
            Phase::Unsupported { reason }
        } else if is_installed(id, &root.join(id.dir())) {
            Phase::Ready
        } else {
            Phase::Missing
        }
    }

    /// Called once at startup with `<app local data>/local-model`.
    pub fn initialize(&self, root: PathBuf) {
        let machine = Machine::detect();
        let mut phases = BTreeMap::new();
        for id in catalog::platform_variants() {
            let phase = Self::resting_phase(&machine, *id, &root);
            if phase == Phase::Ready {
                backends::tidy(*id, &root.join(id.dir()));
            }
            phases.insert(*id, phase);
        }
        let saved = std::fs::read_to_string(root.join(ACTIVE))
            .ok()
            .and_then(|text| serde_json::from_str::<ActiveFile>(&text).ok())
            .map(|file| file.variant);
        let ready = |id: &VariantId| phases.get(id) == Some(&Phase::Ready);
        let active = saved.filter(ready).or_else(|| phases.keys().copied().find(ready));
        let mut inner = self.inner.lock().expect("helper model");
        inner.root = Some(root);
        inner.machine = machine;
        inner.phases = phases;
        inner.active = active;
    }

    /// A build is installed and in use.
    pub fn is_ready(&self) -> bool {
        let inner = self.inner.lock().expect("helper model");
        inner.active.is_some_and(|id| inner.phases.get(&id) == Some(&Phase::Ready))
    }

    fn set_phase(&self, hub: &AppEventHub, id: VariantId, phase: Phase) {
        let throttled = matches!(phase, Phase::Downloading { .. } | Phase::Preparing { .. });
        {
            let mut inner = self.inner.lock().expect("helper model");
            inner.phases.insert(id, phase);
            if throttled && inner.last_status_push.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
                return;
            }
            inner.last_status_push = Some(Instant::now());
        }
        self.publish(hub);
    }

    /// Pushes the install state. The runtime's fields stay empty: asking the
    /// service would wait behind a load, and downloads push several times a
    /// second.
    fn publish(&self, hub: &AppEventHub) {
        Self::push(hub, &self.snapshot(None));
    }

    /// Pushes the whole status, the runtime's included; for when the service
    /// has just finished loading.
    pub(crate) fn publish_with_runtime(&self, hub: &AppEventHub) {
        Self::push(hub, &self.status());
    }

    fn push(hub: &AppEventHub, status: &Status) {
        hub.publish(AppPushEvent::LocalModelChanged { status: serde_json::to_value(status).unwrap_or_default() });
    }

    fn snapshot(&self, runtime: Option<local_model::scheduler::Status>) -> Status {
        let inner = self.inner.lock().expect("helper model");
        let runtime = runtime.unwrap_or_default();
        let variants: Vec<VariantStatus> = catalog::platform_variants()
            .iter()
            .map(|id| VariantStatus {
                id: *id,
                phase: inner.phases.get(id).cloned().unwrap_or(Phase::Missing),
                download_bytes: catalog::download_bytes(*id),
                disk_bytes: inner.root.as_deref().map(|root| dir_size(&root.join(id.dir()))).unwrap_or(0),
            })
            .collect();
        Status {
            recommended: inner.machine.recommended(catalog::platform_variants()),
            machine: inner.machine.clone(),
            disk_bytes: variants.iter().map(|variant| variant.disk_bytes).sum(),
            variants,
            active: inner.active,
            warming: inner.warming,
            device: runtime.device,
            loaded: runtime.loaded,
            running: runtime.running,
            queued: runtime.queued,
            slots: runtime.slots,
            context: runtime.context,
            last_error: runtime.last_error,
        }
    }

    /// Current status, including the runtime's when a service is running.
    pub fn status(&self) -> Status {
        let service = self.inner.lock().expect("helper model").service.clone();
        let runtime = service.and_then(|service| {
            let (tx, rx) = std::sync::mpsc::channel();
            service.status(Box::new(move |status| {
                let _ = tx.send(status);
            }));
            rx.recv_timeout(Duration::from_secs(2)).ok().and_then(Result::ok)
        });
        self.snapshot(runtime)
    }

    /// The active build's service, started on first use.
    pub fn service(&self) -> Result<Service, String> {
        let mut inner = self.inner.lock().expect("helper model");
        Self::start_service(&mut inner)
    }

    fn start_service(inner: &mut Inner) -> Result<Service, String> {
        if let Some(service) = &inner.service {
            return Ok(service.clone());
        }
        let id = inner.active.filter(|id| inner.phases.get(id) == Some(&Phase::Ready)).ok_or("本地模型尚未安装")?;
        let dir = inner.root.clone().ok_or("本地模型尚未初始化")?.join(id.dir());
        let service = Service::start(
            ServiceConfig {
                model_dir: dir.clone(),
                cache_dir: dir.join(CACHE_DIR),
                context: CONTEXT,
                limits: Limits::default(),
            },
            backends::loader(id, dir),
        )?;
        let weak = service.clone();
        inner._pressure = local_model::pressure::watch(move |_critical| weak.unload());
        inner.service = Some(service.clone());
        Ok(service)
    }

    /// Downloads `id` (from the mirrors in mainland China with
    /// `china_mirror`) and finishes it on the device, on a background thread,
    /// publishing progress; returns at once. With no build in use yet, `id`
    /// becomes the one in use when it is ready.
    pub fn install(
        self: &Arc<Self>,
        id: VariantId,
        china_mirror: bool,
        hub: AppEventHub,
        settings: GlobalSettings,
    ) -> Result<(), String> {
        let root = self.root()?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut inner = self.inner.lock().expect("helper model");
            if let Err(reason) = inner.machine.check(id) {
                return Err(format!("这台设备不能运行这个版本（{reason:?}）"));
            }
            if let Some((running, _)) = inner.installing {
                return if running == id { Ok(()) } else { Err("已有模型在下载，请等它完成或取消".into()) };
            }
            if inner.phases.get(&id) == Some(&Phase::Ready) {
                return Ok(());
            }
            let files = catalog::files(id).files;
            inner.phases.insert(
                id,
                Phase::Downloading {
                    received: 0,
                    total: files.iter().map(|file| file.size).sum(),
                    source: download::Source::of(&catalog::PREBUILT_REPO, china_mirror).label.into(),
                },
            );
            inner.installing = Some((id, cancel.clone()));
        }
        self.publish(&hub);
        let this = self.clone();
        std::thread::Builder::new()
            .name("mework-local-model-install".into())
            .spawn(move || {
                let result = this.run_install(id, china_mirror, &root, &hub, &cancel);
                let activate = {
                    let mut inner = this.inner.lock().expect("helper model");
                    inner.installing = None;
                    let phase = match &result {
                        Ok(()) => Phase::Ready,
                        Err(error) if is_cancel(error) => Phase::Missing,
                        Err(message) => Phase::Failed { message: message.clone() },
                    };
                    inner.phases.insert(id, phase);
                    result.is_ok() && !inner.active.is_some_and(|active| inner.phases.get(&active) == Some(&Phase::Ready))
                };
                this.publish(&hub);
                if activate {
                    if let Err(error) = this.activate(id, hub.clone(), settings) {
                        eprintln!("[local-model] could not switch to {id:?}: {error}");
                    }
                }
            })
            .map_err(|error| format!("无法启动安装: {error}"))?;
        Ok(())
    }

    fn run_install(
        &self,
        id: VariantId,
        china_mirror: bool,
        root: &Path,
        hub: &AppEventHub,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let dir = root.join(id.dir());
        let mut files = catalog::files(id);
        if id == VariantId::Mlx {
            let metallib = files.files.iter().position(|file| file.local == local_model::mlx::METALLIB_FILE);
            if let Some(index) = metallib.filter(|index| backends::provide_metallib(&dir, &files.files[*index])) {
                files.files.remove(index);
            }
        }
        let source = download::Source::of(&catalog::PREBUILT_REPO, china_mirror);
        download::download(&dir, &files.files, &source, cancel, &mut |p| {
            self.set_phase(hub, id, Phase::Downloading { received: p.received, total: p.total, source: p.source.label.into() });
        })?;
        if cancel.load(Ordering::Acquire) {
            return Err(download::CANCELLED.into());
        }
        backends::prepare(id, &dir, &mut |step, done, total| {
            self.set_phase(hub, id, Phase::Preparing { step: step.into(), done, total });
        })?;
        let marker = serde_json::to_string(&Marker { version: files.version }).expect("marker");
        std::fs::write(dir.join(MARKER), marker).map_err(|error| format!("无法写入安装记录: {error}"))?;
        Ok(())
    }

    pub fn cancel_install(&self) {
        if let Some((_, cancel)) = &self.inner.lock().expect("helper model").installing {
            cancel.store(true, Ordering::Release);
        }
    }

    /// Makes `id` (installed) the build in use, then loads it and caches both
    /// prompts in the background: on a Mac the Neural Engine build's first
    /// load compiles it for this chip, which should happen now rather than
    /// when the first title is due.
    pub fn activate(self: &Arc<Self>, id: VariantId, hub: AppEventHub, settings: GlobalSettings) -> Result<(), String> {
        let generation = {
            let mut inner = self.inner.lock().expect("helper model");
            if inner.phases.get(&id) != Some(&Phase::Ready) {
                return Err("这个模型还没有安装好".into());
            }
            let root = inner.root.clone().ok_or("本地模型尚未初始化")?;
            let text = serde_json::to_string(&ActiveFile { variant: id }).expect("active");
            std::fs::write(root.join(ACTIVE), text).map_err(|error| format!("无法保存选择: {error}"))?;
            if inner.active != Some(id) || inner.service.is_none() {
                inner.active = Some(id);
                inner.service = None;
                inner._pressure = None;
            }
            inner.generation += 1;
            inner.warming = true;
            inner.generation
        };
        self.publish(&hub);
        let this = self.clone();
        std::thread::Builder::new()
            .name("mework-local-model-warm".into())
            .spawn(move || {
                let result = this.service().and_then(|service| {
                    for task in [Task::Title, Task::Shell] {
                        wait_prompt_info(&service, &prompt_for(&settings, task))?;
                    }
                    Ok(())
                });
                if let Err(error) = result {
                    eprintln!("[local-model] loading {id:?} failed: {error}");
                }
                {
                    let mut inner = this.inner.lock().expect("helper model");
                    if inner.generation == generation {
                        inner.warming = false;
                    }
                }
                this.publish_with_runtime(&hub);
            })
            .map_err(|error| format!("无法加载模型: {error}"))?;
        Ok(())
    }

    /// Deletes `id` from disk. If it was in use, another installed build
    /// takes over (without loading it until needed), or none.
    pub fn remove(&self, id: VariantId, hub: &AppEventHub) -> Result<(), String> {
        let root = self.root()?;
        {
            let mut inner = self.inner.lock().expect("helper model");
            if inner.installing.as_ref().is_some_and(|(running, _)| *running == id) {
                return Err("请先取消正在进行的下载".into());
            }
            if inner.active == Some(id) {
                inner.service = None;
                inner._pressure = None;
                inner.warming = false;
                inner.generation += 1;
                let next = inner.phases.iter().find(|(other, phase)| **other != id && **phase == Phase::Ready).map(|(other, _)| *other);
                inner.active = next;
                match next {
                    Some(next) => {
                        let text = serde_json::to_string(&ActiveFile { variant: next }).expect("active");
                        let _ = std::fs::write(root.join(ACTIVE), text);
                    }
                    None => {
                        let _ = std::fs::remove_file(root.join(ACTIVE));
                    }
                }
            }
            inner.phases.insert(id, Phase::Missing);
        }
        let dir = root.join(id.dir());
        let result = if dir.exists() { std::fs::remove_dir_all(&dir).map_err(|error| format!("无法删除本地模型: {error}")) } else { Ok(()) };
        {
            let mut inner = self.inner.lock().expect("helper model");
            let phase = Self::resting_phase(&inner.machine, id, &root);
            inner.phases.insert(id, phase);
        }
        self.publish(hub);
        result
    }

    /// Caches `prompt`'s prefix state (loading the model if needed) and
    /// reports its token count and size.
    pub fn prompt_report(&self, prompt: &str) -> Result<PromptReport, String> {
        let service = self.service()?;
        let info = wait_prompt_info(&service, prompt)?;
        Ok(PromptReport { tokens: info.tokens, cache_bytes: info.cache_bytes, max_tokens: service.max_prefix_tokens() })
    }

    /// Drops prompt caches for prompts no longer in effect.
    pub fn prune_prompt_caches(&self, settings: &GlobalSettings) {
        let Some(service) = self.inner.lock().expect("helper model").service.clone() else { return };
        let title = prompt_for(settings, Task::Title);
        let shell = prompt_for(settings, Task::Shell);
        service.prune_cache(&[&title, &shell]);
    }

    pub(crate) fn remember_title(&self, conversation_id: &str, replaced: String, title: String) {
        let mut recent = self.recent_titles.lock().expect("recent titles");
        recent.retain(|_, entry| entry.at.elapsed() < Duration::from_secs(30));
        recent.insert(conversation_id.to_string(), RecentTitle { replaced, title, at: Instant::now() });
    }

    /// Whether `proposed` is a renderer commit carrying the title the host
    /// replaced moments ago, while the stored title is the host's.
    pub(crate) fn is_stale_title(&self, conversation_id: &str, proposed: &str, stored: &str) -> bool {
        let recent = self.recent_titles.lock().expect("recent titles");
        recent.get(conversation_id).is_some_and(|entry| {
            entry.at.elapsed() < Duration::from_secs(30) && entry.replaced == proposed && entry.title == stored
        })
    }
}

fn wait_prompt_info(service: &Service, prompt: &str) -> Result<PromptInfo, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    service.prompt_info(
        prompt,
        Box::new(move |result| {
            let _ = tx.send(result);
        }),
    );
    // The first load on a Mac compiles for the Neural Engine: allow minutes.
    rx.recv_timeout(Duration::from_secs(900)).map_err(|_| "本地模型没有响应".to_string())?
}

/// `<app local data>/local-model`.
pub fn root_dir(app_local_data: &Path) -> PathBuf {
    app_local_data.join("local-model")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_from_another_version_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let id = catalog::platform_variants()[0];
        std::fs::write(dir.path().join(MARKER), r#"{"version":"old"}"#).unwrap();
        assert!(!is_installed(id, dir.path()));
    }

    #[test]
    fn status_lists_every_platform_variant() {
        let model = HelperModel::default();
        let dir = tempfile::tempdir().unwrap();
        model.initialize(dir.path().to_path_buf());
        let status = model.snapshot(None);
        assert_eq!(status.variants.len(), catalog::platform_variants().len());
        assert_eq!(status.active, None);
        assert!(!model.is_ready());
        let json = serde_json::to_value(&status).unwrap();
        assert!(json["variants"][0]["phase"].is_string());
        assert!(json["variants"][0]["downloadBytes"].as_u64().unwrap() > 1_000_000_000);
    }

    #[test]
    fn remembers_the_active_variant_only_if_installed() {
        let dir = tempfile::tempdir().unwrap();
        let id = catalog::platform_variants()[0];
        std::fs::write(dir.path().join(ACTIVE), serde_json::to_string(&ActiveFile { variant: id }).unwrap()).unwrap();
        let model = HelperModel::default();
        model.initialize(dir.path().to_path_buf());
        assert_eq!(model.snapshot(None).active, None, "a saved choice of a missing build is dropped");
    }
}
