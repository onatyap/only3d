//! Which graphics adapter the window draws with. A real GPU wins when there is one (a discrete
//! card before an integrated one), and software rendering is the last resort. Among adapters of
//! the same kind, the platform's own API wins: Metal on macOS, DirectX 12 on Windows (where
//! Vulkan drivers vary more), Vulkan on Linux, and OpenGL last.
//!
//! `ONLY3D_GPU` overrides the choice: `software` forces the software adapter (to test the
//! no-GPU path on a machine that has a GPU), any other text picks the first adapter whose name
//! contains it.

use std::sync::Arc;

use egui_wgpu::wgpu;

/// What the choice looks at, so it can be tested without real adapters.
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    pub name: &'a str,
    pub kind: wgpu::DeviceType,
    pub backend: wgpu::Backend,
}

impl<'a> Candidate<'a> {
    pub fn of(info: &'a wgpu::AdapterInfo) -> Self {
        Candidate { name: &info.name, kind: info.device_type, backend: info.backend }
    }
}

/// Lower is better: the kind of device first, then the API.
fn rank(c: &Candidate) -> (u8, u8) {
    let kind = match c.kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    let api = match c.backend {
        wgpu::Backend::Metal => 0,
        wgpu::Backend::Dx12 => if cfg!(windows) { 0 } else { 1 },
        wgpu::Backend::Vulkan => if cfg!(windows) { 1 } else { 0 },
        wgpu::Backend::Gl => 2,
        _ => 3,
    };
    (kind, api)
}

/// Whether `c` is what the `ONLY3D_GPU` override asks for.
fn matches(c: &Candidate, want: &str) -> bool {
    match want.trim().to_lowercase().as_str() {
        "" => false,
        "software" | "cpu" => c.kind == wgpu::DeviceType::Cpu,
        w => c.name.to_lowercase().contains(w),
    }
}

/// The adapter to draw with, as an index into `list`. `want` is the `ONLY3D_GPU` override; when
/// it matches nothing, the normal order applies.
pub fn choose(list: &[Candidate], want: Option<&str>) -> Option<usize> {
    let best = |want: Option<&str>| list.iter().enumerate().filter(|(_, c)| want.is_none_or(|w| matches(c, w))).min_by_key(|(_, c)| rank(c)).map(|(i, _)| i);
    want.and_then(|w| best(Some(w))).or_else(|| best(None))
}

fn override_env() -> Option<String> {
    std::env::var("ONLY3D_GPU").ok().filter(|v| !v.trim().is_empty())
}

/// For eframe: picks among the adapters that can draw to the window.
pub fn selector() -> egui_wgpu::NativeAdapterSelectorMethod {
    Arc::new(|adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        let usable: Vec<&wgpu::Adapter> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).collect();
        let infos: Vec<wgpu::AdapterInfo> = usable.iter().map(|a| a.get_info()).collect();
        let list: Vec<Candidate> = infos.iter().map(Candidate::of).collect();
        choose(&list, override_env().as_deref()).map(|i| usable[i].clone()).ok_or_else(|| {
            if adapters.is_empty() {
                "no graphics adapter found (not even software rendering): update the graphics driver".to_string()
            } else {
                format!("none of the {} graphics adapters can draw to this window", adapters.len())
            }
        })
    })
}

pub fn api_name(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        _ => "no API",
    }
}

pub fn kind_name(k: wgpu::DeviceType) -> &'static str {
    match k {
        wgpu::DeviceType::DiscreteGpu => "graphics card",
        wgpu::DeviceType::IntegratedGpu => "built-in GPU",
        wgpu::DeviceType::VirtualGpu => "virtual GPU",
        wgpu::DeviceType::Other => "other device",
        wgpu::DeviceType::Cpu => "software",
    }
}

/// "NVIDIA A10-4Q · DirectX 12", with "(software)" when it is the CPU.
pub fn summary(info: &wgpu::AdapterInfo) -> String {
    let name = if info.name.trim().is_empty() { "Unnamed adapter" } else { info.name.trim() };
    let soft = if info.device_type == wgpu::DeviceType::Cpu { " (software)" } else { "" };
    format!("{name} · {}{soft}", api_name(info.backend))
}

/// Every adapter this machine offers, with the one the window would use marked. For
/// `only3d gpu`; it can't check against a window, so it may list adapters the window can't use.
pub fn report() -> String {
    let setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let backends = setup.instance_descriptor.backends;
    let instance = wgpu::Instance::new(setup.instance_descriptor);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let infos: Vec<wgpu::AdapterInfo> = adapters.iter().map(|a| a.get_info()).collect();
    let list: Vec<Candidate> = infos.iter().map(Candidate::of).collect();
    let want = override_env();
    let pick = choose(&list, want.as_deref());
    let mut out = String::new();
    if infos.is_empty() {
        out.push_str("No graphics adapters found, not even software rendering. The window can't open; update the graphics driver.\n");
        return out;
    }
    out.push_str("Graphics adapters (▸ = the one only3d draws with):\n");
    for (i, info) in infos.iter().enumerate() {
        let mark = if Some(i) == pick { "▸" } else { " " };
        let driver = [info.driver.trim(), info.driver_info.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        let driver = if driver.is_empty() { String::new() } else { format!(" · driver {driver}") };
        out.push_str(&format!("{mark} {} · {}{driver}\n", summary(info), kind_name(info.device_type)));
    }
    if let Some(w) = want {
        if list.iter().any(|c| matches(c, &w)) {
            out.push_str(&format!("ONLY3D_GPU={w} chose it.\n"));
        } else {
            out.push_str(&format!("ONLY3D_GPU={w} matches no adapter here, so the usual order applies.\n"));
        }
    }
    if pick.is_some_and(|i| infos[i].device_type == wgpu::DeviceType::Cpu) {
        out.push_str("No GPU found: the 3D view uses software rendering and will be slower.\n");
    }
    out
}

/// `only3d gpu --bench`: the viewport benchmark on the adapter the window would use.
pub fn bench_report() -> String {
    let setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let backends = setup.instance_descriptor.backends;
    let instance = wgpu::Instance::new(setup.instance_descriptor);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let infos: Vec<wgpu::AdapterInfo> = adapters.iter().map(|a| a.get_info()).collect();
    let list: Vec<Candidate> = infos.iter().map(Candidate::of).collect();
    let Some(i) = choose(&list, override_env().as_deref()) else { return "No graphics adapter found.".into() };
    let soft = infos[i].device_type == wgpu::DeviceType::Cpu;
    let b = crate::gpu::bench(&adapters[i], crate::gpu::samples_for(&infos[i]), soft);
    let worst = b.mesh_ms.max(b.toolpath_ms);
    let verdict = if worst < 16.0 {
        "The 3D view will stay smooth."
    } else if worst < 50.0 {
        "The 3D view is usable; big models and toolpaths will feel slower."
    } else if b.part_ms < 100.0 {
        "Everyday parts stay usable; big models and toolpaths will be slow."
    } else {
        "Expect a slow 3D view, even with small parts."
    };
    format!("{}\n{verdict} (Each frame here is drawn and waited for; real frames overlap and are faster.)\n", b.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::{Backend as B, DeviceType as D};

    fn c(name: &str, kind: D, backend: B) -> Candidate<'_> {
        Candidate { name, kind, backend }
    }

    #[test]
    fn a_real_gpu_beats_software_and_a_card_beats_built_in() {
        let list = [c("Microsoft Basic Render Driver", D::Cpu, B::Dx12), c("Intel UHD 630", D::IntegratedGpu, B::Dx12), c("NVIDIA T4", D::DiscreteGpu, B::Dx12)];
        assert_eq!(choose(&list, None), Some(2));
        assert_eq!(choose(&list[..2], None), Some(1));
        assert_eq!(choose(&list[..1], None), Some(0));
        assert_eq!(choose(&[], None), None);
    }

    #[test]
    fn the_native_api_beats_opengl_on_the_same_card() {
        let list = [c("NVIDIA T4", D::DiscreteGpu, B::Gl), c("NVIDIA T4", D::DiscreteGpu, B::Vulkan), c("llvmpipe", D::Cpu, B::Gl)];
        assert_eq!(choose(&list, None), Some(1));
    }

    #[test]
    fn windows_prefers_directx_12_and_the_rest_vulkan() {
        let list = [c("NVIDIA T4", D::DiscreteGpu, B::Vulkan), c("NVIDIA T4", D::DiscreteGpu, B::Dx12)];
        assert_eq!(choose(&list, None), Some(if cfg!(windows) { 1 } else { 0 }));
    }

    #[test]
    fn the_override_forces_software_or_a_name_and_falls_back() {
        let list = [c("NVIDIA T4", D::DiscreteGpu, B::Vulkan), c("llvmpipe (LLVM 15)", D::Cpu, B::Vulkan), c("AMD Radeon Pro V520", D::DiscreteGpu, B::Vulkan)];
        assert_eq!(choose(&list, Some("software")), Some(1));
        assert_eq!(choose(&list, Some(" Radeon ")), Some(2));
        assert_eq!(choose(&list, Some("nothing like it")), Some(0));
        assert_eq!(choose(&list, Some("")), Some(0));
    }
}
