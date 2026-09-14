//! WGSL to SPIR-V compilation.
//!
//! Shaders are authored in WGSL and translated with naga, which is pure Rust.
//! That keeps the build free of shaderc or glslc, so the game cross-compiles
//! without a Vulkan SDK installed.

use ash::vk;

use super::context::Context;

pub fn compile(source: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|e| panic!("WGSL parse error: {}", e.emit_to_string(source)));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::PUSH_CONSTANT,
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("WGSL validation error: {e:?}"));

    let mut options = naga::back::spv::Options::default();
    options.lang_version = (1, 3);
    naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V generation failed")
}

pub unsafe fn module(ctx: &Context, spirv: &[u32]) -> vk::ShaderModule {
    ctx.device
        .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(spirv), None)
        .expect("create shader module")
}
