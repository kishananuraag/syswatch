//! GPU utilisation from the IOAccelerator node, the same unprivileged source
//! the Python dashboard reads (`read_gpu_util()` in dashboard/server.py).
//!
//! `ioreg -r -d 1 -w0 -c IOAccelerator` prints one block whose
//! `PerformanceStatistics` dictionary carries:
//!
//!   * `Device Utilization %`, `Renderer Utilization %`, `Tiler Utilization %`
//!   * `In use system memory`, `Alloc system memory` (bytes)
//!   * `gpu-core-count`, `model` (siblings of the statistics dict)
//!
//! Verified live on this M2: `Device Utilization %` 2–20, renderer == tiler,
//! `In use system memory` ~570 MB, `gpu-core-count` 10. GPU **frequency** is
//! not in PerformanceStatistics on Apple Silicon, so it stays `null` with a
//! reason — never a guessed clock.
//!
//! One spawn per [`GPU_REFRESH`]: `ioreg` walks the service plane, and GPU
//! utilisation does not need 1 s resolution. The HTTP handlers never call this
//! — they read the sampler's cached value, so a 1 s poll costs no spawn.

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const GPU_REFRESH: Duration = Duration::from_secs(5);
const IOREG: &str = "/usr/sbin/ioreg";

pub struct GpuProbe {
    cached: Option<(Instant, Value)>,
}

impl GpuProbe {
    pub fn new() -> Self {
        Self { cached: None }
    }

    /// Latest sample, refeshed at most every [`GPU_REFRESH`].
    pub fn sample(&mut self) -> Value {
        if let Some((at, v)) = &self.cached
            && at.elapsed() < GPU_REFRESH
        {
            return v.clone();
        }
        let v = probe();
        self.cached = Some((Instant::now(), v.clone()));
        v
    }
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self::new()
    }
}

fn probe() -> Value {
    let mut out = json!({
        "available": false,
        "reason": Value::Null,
        "util_pct": Value::Null,
        "renderer_pct": Value::Null,
        "tiler_pct": Value::Null,
        "mem_used_mb": Value::Null,
        "mem_alloc_mb": Value::Null,
        "freq_mhz": Value::Null,
        "cores": Value::Null,
        "model": Value::Null,
        "source": "ioreg IOAccelerator PerformanceStatistics",
        "gpu_model": Value::Null,
        "sampled_ts": Value::Null,
    });

    let text = match Command::new(IOREG)
        .args(["-r", "-d", "1", "-w0", "-c", "IOAccelerator"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(_) => {
            out["reason"] = json!("ioreg IOAccelerator exited non-zero");
            return out;
        }
        Err(e) => {
            out["reason"] = json!(format!("could not run ioreg: {e}"));
            return out;
        }
    };
    if !text.contains("IOAccelerator") && !text.contains("PerformanceStatistics") {
        out["reason"] = json!("ioreg IOAccelerator returned no object");
        return out;
    }

    // Only the first accelerator block is read: on this machine there is one.
    let block = first_accelerator_block(&text);
    let Some(util) = field(block, "Device Utilization %").and_then(|s| num(s)) else {
        out["reason"] = json!(
            "IOAccelerator exposes PerformanceStatistics but no Device Utilization % on this GPU"
        );
        return out;
    };

    out["available"] = json!(true);
    out["util_pct"] = json!(util);
    out["renderer_pct"] = json!(field(block, "Renderer Utilization %").and_then(num));
    out["tiler_pct"] = json!(field(block, "Tiler Utilization %").and_then(num));
    if let Some(used) = field(block, "In use system memory").and_then(num) {
        out["mem_used_mb"] = json!(round1(used / 1_048_576.0));
    }
    if let Some(alloc) = field(block, "Alloc system memory").and_then(num) {
        out["mem_alloc_mb"] = json!(round1(alloc / 1_048_576.0));
    }
    // Some GPUs do carry a clock here; this one does not.
    for key in ["GPUFrequency", "Core Clock", "gpu-core-clock"] {
        if let Some(v) = field(block, key).and_then(num)
            && v > 0.0
        {
            out["freq_mhz"] = json!(if v > 1e6 { round2(v / 1e6) } else { v });
            break;
        }
    }
    if out["freq_mhz"].is_null() {
        out["freq_reason"] =
            json!("PerformanceStatistics carries no GPU clock on Apple Silicon; needs powermetrics (root)");
    }
    if let Some(cores) = field(block, "gpu-core-count").and_then(|s| s.parse::<i64>().ok()) {
        out["cores"] = json!(cores);
    }
    if let Some(model) = field_str(block, "model") {
        out["gpu_model"] = json!(model);
    }
    out["sampled_ts"] = json!(crate::timefmt::now_unix() as f64);
    out
}

/// The text before the second `+-o ` header — i.e. one object's properties.
fn first_accelerator_block(text: &str) -> &str {
    let mut seen = 0usize;
    for (idx, _) in text.match_indices("+-o ") {
        seen += 1;
        if seen == 2 {
            return &text[..idx];
        }
    }
    text
}

/// Value text after `"key"=` (nested dictionaries print without spaces) or
/// `"key" = ` (top-level properties print with them).
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let tight = format!("\"{key}\"=");
    let spaced = format!("\"{key}\" = ");
    let rest = match text.find(&tight) {
        Some(i) => &text[i + tight.len()..],
        None => {
            let i = text.find(&spaced)?;
            &text[i + spaced.len()..]
        }
    };
    let rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some(&stripped[..end]);
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.' || c == 'e' || c == '+'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

fn field_str(text: &str, key: &str) -> Option<String> {
    let spaced = format!("\"{key}\" = \"");
    let i = text.find(&spaced)?;
    let rest = &text[i + spaced.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the live `ioreg -r -d 1 -w0 -c IOAccelerator` output on
    /// this M2.
    const LIVE: &str = r#"+-o AGXAcceleratorG14G  <class AGXAcceleratorG14G, id 0x100000518>
    {
      "IOMatchedAtBoot" = Yes
      "PerformanceStatistics" = {"In use system memory (driver)"=0,"Alloc system memory"=1886633984,"Tiler Utilization %"=6,"Renderer Utilization %"=6,"Device Utilization %"=6,"In use system memory"=570376192}
      "model" = "Apple M2"
      "gpu-core-count" = 10
      "IONameMatched" = "gpu,t8112"
    }
"#;

    #[test]
    fn parses_the_live_block() {
        let b = first_accelerator_block(LIVE);
        assert_eq!(field(b, "Device Utilization %").and_then(num), Some(6.0));
        assert_eq!(field(b, "In use system memory").and_then(num), Some(570376192.0));
        // The driver row is a different key and must not shadow the real one.
        assert_eq!(field(b, "In use system memory (driver)").and_then(num), Some(0.0));
        assert_eq!(field_str(b, "model").as_deref(), Some("Apple M2"));
        assert_eq!(field(b, "gpu-core-count"), Some("10"));
    }

    #[test]
    fn second_accelerator_block_is_not_read() {
        let two = format!("{LIVE}+-o SecondGPU  {{ \"Device Utilization %\"=99 }}");
        assert_eq!(
            field(first_accelerator_block(&two), "Device Utilization %").and_then(num),
            Some(6.0)
        );
    }

    #[test]
    fn absent_utilisation_is_unavailable_not_zero() {
        let text = "+-o X {\n  \"PerformanceStatistics\" = {\"Alloc system memory\"=1}\n}";
        assert!(field(text, "Device Utilization %").is_none());
    }
}
