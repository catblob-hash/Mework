//! What this machine can run: the model builds on offer depend on the chip
//! (a Neural Engine, Apple silicon at all) and the system version.

use serde::Serialize;

use super::catalog::VariantId;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// e.g. "Apple M4 Pro".
    pub chip: Option<String>,
    /// e.g. "Mac16,7".
    pub model: Option<String>,
    /// e.g. "15.4".
    pub os_version: Option<String>,
    pub apple_silicon: bool,
    /// Neural Engine cores, when Core ML reports one.
    pub neural_engine_cores: Option<usize>,
}

/// Why a build cannot run here; the renderer words it.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Unavailable {
    NeedsAppleSilicon,
    NoNeuralEngine,
    NeedsMacos14,
    NeedsMacos15,
    /// This build of the app does not include the backend.
    NotInThisBuild,
}

impl Machine {
    #[cfg(target_os = "macos")]
    pub fn detect() -> Self {
        let sysctl = |name: &str| {
            let output = std::process::Command::new("/usr/sbin/sysctl").args(["-n", name]).output().ok()?;
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (output.status.success() && !text.is_empty()).then_some(text)
        };
        let os_version = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty());
        Self {
            chip: sysctl("machdep.cpu.brand_string"),
            model: sysctl("hw.model"),
            os_version,
            apple_silicon: sysctl("hw.optional.arm64").as_deref() == Some("1"),
            neural_engine_cores: local_model::coreml::runtime::neural_engine_cores(),
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn detect() -> Self {
        Self::default()
    }

    fn os_major(&self) -> u32 {
        self.os_version.as_deref().and_then(|v| v.split('.').next()).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// Whether `id` can run here, or why not.
    pub fn check(&self, id: VariantId) -> Result<(), Unavailable> {
        match id {
            VariantId::Ane => {
                if !cfg!(target_os = "macos") {
                    Err(Unavailable::NotInThisBuild)
                } else if self.neural_engine_cores.is_none() {
                    Err(if self.apple_silicon { Unavailable::NoNeuralEngine } else { Unavailable::NeedsAppleSilicon })
                } else if self.os_major() < 15 {
                    // Core ML states and multifunction models.
                    Err(Unavailable::NeedsMacos15)
                } else {
                    Ok(())
                }
            }
            VariantId::Mlx => {
                if !self.apple_silicon {
                    Err(Unavailable::NeedsAppleSilicon)
                } else if self.os_major() < 14 {
                    Err(Unavailable::NeedsMacos14)
                } else if !mlx_built() {
                    Err(Unavailable::NotInThisBuild)
                } else {
                    Ok(())
                }
            }
            VariantId::Llama => {
                if llama_built() {
                    Ok(())
                } else {
                    Err(Unavailable::NotInThisBuild)
                }
            }
        }
    }

    /// The build to offer first: the Neural Engine when there is one (low
    /// power, leaves the GPU alone), else MLX, else llama.cpp.
    pub fn recommended(&self, variants: &[VariantId]) -> Option<VariantId> {
        variants.iter().copied().find(|id| self.check(*id).is_ok())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn mlx_built() -> bool {
    local_model::mlx::shim_path().is_some()
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn mlx_built() -> bool {
    false
}

/// llama.cpp publishes a release build for this platform (`local_model::llama::runtime`).
#[cfg(not(target_os = "macos"))]
fn llama_built() -> bool {
    !local_model::llama::runtime::archives().is_empty()
}

#[cfg(target_os = "macos")]
fn llama_built() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(os: &str, apple_silicon: bool, ane: Option<usize>) -> Machine {
        Machine {
            chip: Some("Apple M1".into()),
            model: Some("MacBookAir10,1".into()),
            os_version: Some(os.into()),
            apple_silicon,
            neural_engine_cores: ane,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn offers_what_the_chip_and_system_can_run() {
        let current = mac("15.4", true, Some(16));
        assert_eq!(current.check(VariantId::Ane), Ok(()));
        assert_eq!(current.recommended(&[VariantId::Ane, VariantId::Mlx]), Some(VariantId::Ane));
        assert_eq!(mac("14.6", true, Some(16)).check(VariantId::Ane), Err(Unavailable::NeedsMacos15));
        // A virtual machine on Apple silicon: no Neural Engine, MLX only.
        let vm = mac("15.4", true, None);
        assert_eq!(vm.check(VariantId::Ane), Err(Unavailable::NoNeuralEngine));
        assert_eq!(mac("15.4", false, None).check(VariantId::Ane), Err(Unavailable::NeedsAppleSilicon));
        assert_eq!(mac("15.4", false, None).check(VariantId::Mlx), Err(Unavailable::NeedsAppleSilicon));
        assert_eq!(mac("13.6", true, None).check(VariantId::Mlx), Err(Unavailable::NeedsMacos14));
        assert_eq!(current.check(VariantId::Llama), Err(Unavailable::NotInThisBuild));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn offers_llama_elsewhere() {
        let machine = Machine::default();
        assert_eq!(machine.check(VariantId::Llama), Ok(()));
        assert_eq!(machine.recommended(&[VariantId::Llama]), Some(VariantId::Llama));
        assert_eq!(machine.check(VariantId::Ane), Err(Unavailable::NotInThisBuild));
    }
}
