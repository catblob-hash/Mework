//! Development helper: writes the Neural Engine `.mlpackage` from a model
//! directory. `cargo run --release -p mework-local-model --example build_coreml -- <model dir> <out.mlpackage> [slots chunk context]`
use std::path::Path;
use std::sync::atomic::AtomicBool;

use local_model::coreml::graph::Shapes;
use local_model::coreml::package::{write_package, PackagePlan};
use local_model::qwen35::Config;
use local_model::safetensors::SafeTensors;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let num = |i: usize, d: usize| args.get(i).map(|s| s.parse().unwrap()).unwrap_or(d);
    let shapes = Shapes { slots: num(3, 4), chunk: num(4, 16), context: num(5, 1024) };
    let config = Config::load(&dir.join("config.json"))?;
    let weights = SafeTensors::open(&dir.join("model.safetensors"))?;
    let mut plan = PackagePlan::standard(&config, shapes);
    if let Ok(parts) = std::env::var("PARTS") {
        let n: usize = parts.parse().unwrap();
        let per = config.layers.len() / n;
        plan.parts = (0..n).map(|i| i * per..if i + 1 == n { config.layers.len() } else { (i + 1) * per }).collect();
    }
    let start = std::time::Instant::now();
    let layout = write_package(&config, &weights, &plan, out, &AtomicBool::new(false), &mut |_| {})?;
    layout.save(&out.with_extension("embedding.json"))?;
    eprintln!("wrote {} in {:.1}s", out.display(), start.elapsed().as_secs_f32());
    Ok(())
}
