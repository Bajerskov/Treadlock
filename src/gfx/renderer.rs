//! Forward renderer built on Vulkan 1.3 dynamic rendering.

use ash::vk;
use glam::{Mat4, Vec3, Vec4};
use gpu_allocator::MemoryLocation;

use super::buffer::Buffer;
use super::context::Context;
use super::shader;
use super::swapchain::{color_range, Swapchain, DEPTH_FORMAT};
use super::texture::Texture;
use crate::mesh::Mesh;
use crate::particles::{ParticleVertex, Particles, MAX_PARTICLES};
use crate::marks;
use crate::track::Vertex;
use crate::ui::UiVertex;

/// Frames recorded ahead of the GPU. Two keeps latency low, which matters more
/// than throughput for a twitchy racer.
const FRAMES_IN_FLIGHT: usize = 2;

// Skid marks are drawn with the particle quad index buffer rather than one of
// their own. Growing either pool past the other would silently draw only part
// of the larger one, so it is a build error instead.
const _: () = assert!(marks::MAX_MARKS <= MAX_PARTICLES);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameData {
    view_proj: Mat4,
    camera_pos: Vec4,
    sun: Vec4,
    fog: Vec4,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Push {
    model: Mat4,
    tint: Vec4,
    params: Vec4,
}

pub struct GpuMesh {
    vertex: Buffer,
    index: Buffer,
    count: u32,
}

impl GpuMesh {
    pub fn upload(ctx: &mut Context, name: &str, vertices: &[Vertex], indices: &[u32]) -> GpuMesh {
        GpuMesh {
            vertex: Buffer::device_local(ctx, name, vertices, vk::BufferUsageFlags::VERTEX_BUFFER),
            index: Buffer::device_local(ctx, name, indices, vk::BufferUsageFlags::INDEX_BUFFER),
            count: indices.len() as u32,
        }
    }

    pub fn from_mesh(ctx: &mut Context, name: &str, mesh: &Mesh) -> GpuMesh {
        GpuMesh::upload(ctx, name, &mesh.vertices, &mesh.indices)
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        self.vertex.destroy(ctx);
        self.index.destroy(ctx);
    }
}

struct Frame {
    command_buffer: vk::CommandBuffer,
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    in_flight: vk::Fence,
    uniform: Buffer,
    descriptor_set: vk::DescriptorSet,
    /// Rebuilt every frame, so both are host visible.
    ui_vertices: Buffer,
    particle_vertices: Buffer,
    mark_vertices: Buffer,
}

/// One object to draw this frame.
pub struct Draw<'a> {
    pub mesh: &'a GpuMesh,
    pub model: Mat4,
    pub tint: Vec3,
    /// 0 = car body, 1 = track surface.
    pub surface: f32,
    pub emissive: f32,
    pub metallic: f32,
    /// Base colour map. None binds the white fallback, so the shader never has
    /// to branch on whether a texture exists.
    pub texture: Option<&'a Texture>,
}

pub struct Renderer {
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    descriptor_layout: vk::DescriptorSetLayout,
    pub texture_layout: vk::DescriptorSetLayout,
    pub texture_pool: vk::DescriptorPool,
    white: Option<Texture>,
    particle_pipeline: vk::Pipeline,
    decal_pipeline: vk::Pipeline,
    ui_pipeline: vk::Pipeline,
    /// Quad indices for the whole pool, uploaded once and reused. Skid marks
    /// are quads too and share it, which only works while the two pools are the
    /// same size.
    particle_indices: Buffer,
    frames: Vec<Frame>,
    frame_index: usize,
    /// Signalled by the swapchain when it no longer matches the window.
    pub needs_resize: bool,
}

impl Renderer {
    pub fn new(ctx: &mut Context, swapchain: &Swapchain) -> Renderer {
        unsafe {
            let descriptor_layout = ctx
                .device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                        vk::DescriptorSetLayoutBinding::default()
                            .binding(0)
                            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                            .descriptor_count(1)
                            .stage_flags(
                                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                            ),
                    ]),
                    None,
                )
                .expect("descriptor set layout");

            // Set 1 holds the material's base colour map, separate from the
            // per-frame set so it can be rebound per draw. naga lowers a WGSL
            // texture and sampler to two distinct bindings rather than one
            // combined descriptor, so the layout has to match that.
            let texture_layout = ctx
                .device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                        vk::DescriptorSetLayoutBinding::default()
                            .binding(0)
                            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                            .descriptor_count(1)
                            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                        vk::DescriptorSetLayoutBinding::default()
                            .binding(1)
                            .descriptor_type(vk::DescriptorType::SAMPLER)
                            .descriptor_count(1)
                            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                    ]),
                    None,
                )
                .expect("texture set layout");

            let texture_pool_sizes = [
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(16),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLER)
                    .descriptor_count(16),
            ];
            let texture_pool = ctx
                .device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .pool_sizes(&texture_pool_sizes)
                        .max_sets(16),
                    None,
                )
                .expect("texture descriptor pool");

            let push_range = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                .offset(0)
                .size(std::mem::size_of::<Push>() as u32)];
            let set_layouts = [descriptor_layout, texture_layout];
            let pipeline_layout = ctx
                .device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&set_layouts)
                        .push_constant_ranges(&push_range),
                    None,
                )
                .expect("pipeline layout");

            let pipeline = build_pipeline(ctx, swapchain, pipeline_layout);

            let pool_sizes = [vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(FRAMES_IN_FLIGHT as u32)];
            let descriptor_pool = ctx
                .device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .pool_sizes(&pool_sizes)
                        .max_sets(FRAMES_IN_FLIGHT as u32),
                    None,
                )
                .expect("descriptor pool");

            let command_buffers = ctx
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(ctx.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(FRAMES_IN_FLIGHT as u32),
                )
                .expect("command buffers");

            let mut frames = Vec::with_capacity(FRAMES_IN_FLIGHT);
            for i in 0..FRAMES_IN_FLIGHT {
                let uniform = Buffer::new(
                    ctx,
                    "frame uniforms",
                    std::mem::size_of::<FrameData>() as vk::DeviceSize,
                    vk::BufferUsageFlags::UNIFORM_BUFFER,
                    MemoryLocation::CpuToGpu,
                );
                let layouts = [descriptor_layout];
                let descriptor_set = ctx
                    .device
                    .allocate_descriptor_sets(
                        &vk::DescriptorSetAllocateInfo::default()
                            .descriptor_pool(descriptor_pool)
                            .set_layouts(&layouts),
                    )
                    .expect("descriptor set")[0];

                let info = [vk::DescriptorBufferInfo::default()
                    .buffer(uniform.handle)
                    .offset(0)
                    .range(std::mem::size_of::<FrameData>() as vk::DeviceSize)];
                ctx.device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(&info)],
                    &[],
                );

                let particle_vertices = Buffer::new(
                    ctx,
                    "particle vertices",
                    (MAX_PARTICLES * 4 * std::mem::size_of::<ParticleVertex>()) as vk::DeviceSize,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    MemoryLocation::CpuToGpu,
                );

                let mark_vertices = Buffer::new(
                    ctx,
                    "skid mark vertices",
                    (marks::MAX_MARKS * 4 * std::mem::size_of::<ParticleVertex>()) as vk::DeviceSize,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    MemoryLocation::CpuToGpu,
                );

                // Generous: the HUD is a few hundred quads, the minimap most of
                // them, and overrunning would silently truncate the overlay.
                let ui_vertices = Buffer::new(
                    ctx,
                    "ui vertices",
                    (12_000 * std::mem::size_of::<UiVertex>()) as vk::DeviceSize,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    MemoryLocation::CpuToGpu,
                );

                frames.push(Frame {
                    command_buffer: command_buffers[i],
                    ui_vertices,
                    particle_vertices,
                    mark_vertices,
                    image_available: ctx
                        .device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        .unwrap(),
                    render_finished: ctx
                        .device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        .unwrap(),
                    in_flight: ctx
                        .device
                        .create_fence(
                            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                            None,
                        )
                        .unwrap(),
                    uniform,
                    descriptor_set,
                });
            }

            let particle_indices = Buffer::device_local(
                ctx,
                "particle indices",
                &Particles::indices(),
                vk::BufferUsageFlags::INDEX_BUFFER,
            );

            let white = Texture::white(ctx, texture_layout, texture_pool);

            Renderer {
                texture_layout,
                texture_pool,
                white: Some(white),
                particle_pipeline: build_particle_pipeline(ctx, swapchain, pipeline_layout),
                decal_pipeline: build_decal_pipeline(ctx, swapchain, pipeline_layout),
                ui_pipeline: build_ui_pipeline(ctx, swapchain, pipeline_layout),
                particle_indices,
                pipeline_layout,
                pipeline,
                descriptor_pool,
                descriptor_layout,
                frames,
                frame_index: 0,
                needs_resize: false,
            }
        }
    }

    pub fn draw(
        &mut self,
        ctx: &mut Context,
        swapchain: &Swapchain,
        view_proj: Mat4,
        camera_pos: Vec3,
        time: f32,
        draws: &[Draw],
        particle_vertices: &[ParticleVertex],
        mark_vertices: &[ParticleVertex],
        ui_vertices: &[UiVertex],
        font: Option<&Texture>,
    ) {
        unsafe {
            let frame = &mut self.frames[self.frame_index];
            ctx.device
                .wait_for_fences(&[frame.in_flight], true, u64::MAX)
                .unwrap();

            let acquired = swapchain.loader.acquire_next_image(
                swapchain.handle,
                u64::MAX,
                frame.image_available,
                vk::Fence::null(),
            );
            let image_index = match acquired {
                Ok((index, suboptimal)) => {
                    if suboptimal {
                        self.needs_resize = true;
                    }
                    index
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.needs_resize = true;
                    return;
                }
                Err(e) => panic!("acquire failed: {e}"),
            };

            // Only reset once we know we are going to submit, or a skipped frame
            // would leave the fence unsignalled and deadlock the next wait.
            ctx.device.reset_fences(&[frame.in_flight]).unwrap();

            let fog = Vec4::new(0.05, 0.07, 0.12, 0.0016);
            frame.uniform.write(bytemuck::bytes_of(&FrameData {
                view_proj,
                camera_pos: camera_pos.extend(1.0),
                sun: Vec3::new(0.35, 0.86, 0.36).normalize().extend(time),
                fog,
            }));

            let cmd = frame.command_buffer;
            ctx.device
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
                .unwrap();
            ctx.device
                .begin_command_buffer(
                    cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .unwrap();

            let image = swapchain.images[image_index as usize];
            transition(
                ctx,
                cmd,
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            );

            let color_attachment = [vk::RenderingAttachmentInfo::default()
                .image_view(swapchain.views[image_index as usize])
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: [fog.x, fog.y, fog.z, 1.0] },
                })];
            let depth_attachment = vk::RenderingAttachmentInfo::default()
                .image_view(swapchain.depth_view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 },
                });

            ctx.device.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: swapchain.extent })
                    .layer_count(1)
                    .color_attachments(&color_attachment)
                    .depth_attachment(&depth_attachment),
            );

            ctx.device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: swapchain.extent.width as f32,
                    height: swapchain.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            ctx.device.cmd_set_scissor(
                cmd,
                0,
                &[vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: swapchain.extent }],
            );
            ctx.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            ctx.device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[frame.descriptor_set],
                &[],
            );

            for d in draws {
                let push = Push {
                    model: d.model,
                    tint: d.tint.extend(1.0),
                    params: Vec4::new(d.surface, d.emissive, d.metallic, 0.0),
                };
                ctx.device.cmd_push_constants(
                    cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytemuck::bytes_of(&push),
                );
                let texture = d.texture.or(self.white.as_ref()).expect("white fallback");
                ctx.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    1,
                    &[texture.descriptor_set],
                    &[],
                );
                ctx.device
                    .cmd_bind_vertex_buffers(cmd, 0, &[d.mesh.vertex.handle], &[0]);
                ctx.device.cmd_bind_index_buffer(
                    cmd,
                    d.mesh.index.handle,
                    0,
                    vk::IndexType::UINT32,
                );
                ctx.device.cmd_draw_indexed(cmd, d.mesh.count, 1, 0, 0, 0);
            }

            // Skid marks after the solid world and before the particles: they
            // darken the road, so they need the road already drawn, and smoke
            // drifting over a skid should light it rather than the other way
            // round.
            if !mark_vertices.is_empty() {
                let quads = (mark_vertices.len() / 4).min(marks::MAX_MARKS);
                frame
                    .mark_vertices
                    .write(bytemuck::cast_slice(&mark_vertices[..quads * 4]));

                ctx.device.cmd_bind_pipeline(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.decal_pipeline,
                );
                let push = Push {
                    model: Mat4::IDENTITY,
                    tint: Vec4::ONE,
                    params: Vec4::ZERO,
                };
                ctx.device.cmd_push_constants(
                    cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytemuck::bytes_of(&push),
                );
                ctx.device
                    .cmd_bind_vertex_buffers(cmd, 0, &[frame.mark_vertices.handle], &[0]);
                // Marks are quads like particles, so they share that index
                // buffer rather than duplicating it.
                ctx.device.cmd_bind_index_buffer(
                    cmd,
                    self.particle_indices.handle,
                    0,
                    vk::IndexType::UINT32,
                );
                ctx.device.cmd_draw_indexed(cmd, quads as u32 * 6, 1, 0, 0, 0);
            }

            // Particles last, so they blend over finished geometry.
            if !particle_vertices.is_empty() {
                let quads = (particle_vertices.len() / 4).min(MAX_PARTICLES);
                frame
                    .particle_vertices
                    .write(bytemuck::cast_slice(&particle_vertices[..quads * 4]));

                ctx.device.cmd_bind_pipeline(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.particle_pipeline,
                );
                let push = Push {
                    model: Mat4::IDENTITY,
                    tint: Vec4::ONE,
                    params: Vec4::ZERO,
                };
                ctx.device.cmd_push_constants(
                    cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytemuck::bytes_of(&push),
                );
                ctx.device.cmd_bind_vertex_buffers(
                    cmd,
                    0,
                    &[frame.particle_vertices.handle],
                    &[0],
                );
                ctx.device.cmd_bind_index_buffer(
                    cmd,
                    self.particle_indices.handle,
                    0,
                    vk::IndexType::UINT32,
                );
                ctx.device.cmd_draw_indexed(cmd, quads as u32 * 6, 1, 0, 0, 0);
            }

            // HUD last of all, over the finished frame.
            if !ui_vertices.is_empty() {
                let capacity =
                    (frame.ui_vertices.size as usize) / std::mem::size_of::<UiVertex>();
                let count = ui_vertices.len().min(capacity);
                frame
                    .ui_vertices
                    .write(bytemuck::cast_slice(&ui_vertices[..count]));

                ctx.device
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.ui_pipeline);
                let push = Push {
                    model: Mat4::IDENTITY,
                    tint: Vec4::ONE,
                    // The UI shader turns pixels into clip space with this.
                    params: Vec4::new(
                        swapchain.extent.width as f32,
                        swapchain.extent.height as f32,
                        0.0,
                        0.0,
                    ),
                };
                ctx.device.cmd_push_constants(
                    cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytemuck::bytes_of(&push),
                );
                let atlas = font.or(self.white.as_ref()).expect("white fallback");
                ctx.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    1,
                    &[atlas.descriptor_set],
                    &[],
                );
                ctx.device
                    .cmd_bind_vertex_buffers(cmd, 0, &[frame.ui_vertices.handle], &[0]);
                ctx.device.cmd_draw(cmd, count as u32, 1, 0, 0);
            }

            ctx.device.cmd_end_rendering(cmd);
            transition(
                ctx,
                cmd,
                image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );
            ctx.device.end_command_buffer(cmd).unwrap();

            let wait = [frame.image_available];
            let signal = [frame.render_finished];
            let buffers = [cmd];
            let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            ctx.device
                .queue_submit(
                    ctx.queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&wait)
                        .wait_dst_stage_mask(&stages)
                        .command_buffers(&buffers)
                        .signal_semaphores(&signal)],
                    frame.in_flight,
                )
                .unwrap();

            let swapchains = [swapchain.handle];
            let indices = [image_index];
            let result = swapchain.loader.queue_present(
                ctx.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signal)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            );
            match result {
                Ok(false) => {}
                Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_resize = true,
                Err(e) => panic!("present failed: {e}"),
            }

            self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
        }
    }

    /// The pipeline bakes in the colour format, so it is rebuilt if that changes.
    pub fn rebuild_pipeline(&mut self, ctx: &mut Context, swapchain: &Swapchain) {
        unsafe {
            ctx.device.device_wait_idle().ok();
            // Every pipeline bakes in the colour format, so every one is
            // rebuilt. The index buffer does not, and must survive.
            ctx.device.destroy_pipeline(self.pipeline, None);
            ctx.device.destroy_pipeline(self.particle_pipeline, None);
            ctx.device.destroy_pipeline(self.decal_pipeline, None);
            ctx.device.destroy_pipeline(self.ui_pipeline, None);
            self.pipeline = build_pipeline(ctx, swapchain, self.pipeline_layout);
            self.ui_pipeline = build_ui_pipeline(ctx, swapchain, self.pipeline_layout);
            self.particle_pipeline =
                build_particle_pipeline(ctx, swapchain, self.pipeline_layout);
            self.decal_pipeline = build_decal_pipeline(ctx, swapchain, self.pipeline_layout);
        }
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        unsafe {
            ctx.device.device_wait_idle().ok();
            for frame in &mut self.frames {
                ctx.device.destroy_semaphore(frame.image_available, None);
                ctx.device.destroy_semaphore(frame.render_finished, None);
                ctx.device.destroy_fence(frame.in_flight, None);
                frame.uniform.destroy(ctx);
                frame.particle_vertices.destroy(ctx);
                frame.mark_vertices.destroy(ctx);
                frame.ui_vertices.destroy(ctx);
            }
            self.frames.clear();
            if let Some(mut white) = self.white.take() {
                white.destroy(ctx);
            }
            ctx.device.destroy_descriptor_pool(self.descriptor_pool, None);
            ctx.device.destroy_descriptor_pool(self.texture_pool, None);
            ctx.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            ctx.device
                .destroy_descriptor_set_layout(self.texture_layout, None);
            ctx.device.destroy_pipeline(self.pipeline, None);
            ctx.device.destroy_pipeline(self.particle_pipeline, None);
            ctx.device.destroy_pipeline(self.decal_pipeline, None);
            ctx.device.destroy_pipeline(self.ui_pipeline, None);
            self.particle_indices.destroy(ctx);
            ctx.device.destroy_pipeline_layout(self.pipeline_layout, None);
        }
    }
}

unsafe fn build_pipeline(
    ctx: &Context,
    swapchain: &Swapchain,
    layout: vk::PipelineLayout,
) -> vk::Pipeline {
    let spirv = shader::compile(include_str!("../shaders/forward.wgsl"));
    let module = shader::module(ctx, &spirv);
    let vs_name = c"vs_main";
    let fs_name = c"fs_main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(vs_name),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(fs_name),
    ];

    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(std::mem::size_of::<Vertex>() as u32)
        .input_rate(vk::VertexInputRate::VERTEX)];
    let attributes = [
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(12),
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(24),
        vk::VertexInputAttributeDescription::default()
            .location(3)
            .binding(0)
            .format(vk::Format::R32_SFLOAT)
            .offset(32),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        // The track is viewed from inside the tube and the car from outside, so
        // a single cull mode cannot suit both. Disabling it costs little here
        // and avoids winding bugs swallowing whole surfaces.
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let depth = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::LESS);
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(false)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [swapchain.format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_formats)
        .depth_attachment_format(DEPTH_FORMAT);

    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);

    let pipeline = ctx
        .device
        .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        .expect("create graphics pipeline")[0];
    ctx.device.destroy_shader_module(module, None);
    pipeline
}

/// The UI draws last, in screen space: alpha blended over the finished frame,
/// with depth entirely out of the way.
unsafe fn build_ui_pipeline(
    ctx: &Context,
    swapchain: &Swapchain,
    layout: vk::PipelineLayout,
) -> vk::Pipeline {
    let spirv = shader::compile(include_str!("../shaders/ui.wgsl"));
    let module = shader::module(ctx, &spirv);
    let vs_name = c"vs_main";
    let fs_name = c"fs_main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(vs_name),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(fs_name),
    ];

    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(std::mem::size_of::<UiVertex>() as u32)
        .input_rate(vk::VertexInputRate::VERTEX)];
    let attributes = [
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(8),
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R32G32B32A32_SFLOAT)
            .offset(16),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let depth = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(false)
        .depth_write_enable(false);
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [swapchain.format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_formats)
        .depth_attachment_format(DEPTH_FORMAT);

    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);

    let pipeline = ctx
        .device
        .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        .expect("create ui pipeline")[0];
    ctx.device.destroy_shader_module(module, None);
    pipeline
}

/// Particles share the frame uniform and pipeline layout but need their own
/// vertex format, additive blending, and depth writes off: they are transparent,
/// so writing depth would make them occlude each other in draw order.
unsafe fn build_particle_pipeline(
    ctx: &Context,
    swapchain: &Swapchain,
    layout: vk::PipelineLayout,
) -> vk::Pipeline {
    let spirv = shader::compile(include_str!("../shaders/particles.wgsl"));
    let module = shader::module(ctx, &spirv);
    let vs_name = c"vs_main";
    let fs_name = c"fs_main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(vs_name),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(fs_name),
    ];

    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(std::mem::size_of::<ParticleVertex>() as u32)
        .input_rate(vk::VertexInputRate::VERTEX)];
    let attributes = [
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32B32A32_SFLOAT)
            .offset(12),
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(28),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    // Tested against the scene so particles hide behind geometry, but not
    // written, so they accumulate with each other in any order.
    let depth = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(false)
        .depth_compare_op(vk::CompareOp::LESS);
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ZERO)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
        .alpha_blend_op(vk::BlendOp::ADD)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [swapchain.format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_formats)
        .depth_attachment_format(DEPTH_FORMAT);

    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);

    let pipeline = ctx
        .device
        .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        .expect("create particle pipeline")[0];
    ctx.device.destroy_shader_module(module, None);
    pipeline
}

/// Layout transition using synchronization2, with conservative stage masks.
unsafe fn transition(
    ctx: &Context,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
) {
    let barrier = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        .old_layout(from)
        .new_layout(to)
        .image(image)
        .subresource_range(color_range())];
    ctx.device
        .cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&barrier));
}

unsafe fn build_decal_pipeline(
    ctx: &Context,
    swapchain: &Swapchain,
    layout: vk::PipelineLayout,
) -> vk::Pipeline {
    let spirv = shader::compile(include_str!("../shaders/decal.wgsl"));
    let module = shader::module(ctx, &spirv);
    let vs_name = c"vs_main";
    let fs_name = c"fs_main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(vs_name),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(fs_name),
    ];

    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(std::mem::size_of::<ParticleVertex>() as u32)
        .input_rate(vk::VertexInputRate::VERTEX)];
    let attributes = [
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32B32A32_SFLOAT)
            .offset(12),
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(28),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    // Tested against the scene so a mark is hidden by anything in front of it,
    // but not written, because a mark lies on the road rather than occluding
    // it and several may overlap in any order.
    let depth = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(false)
        .depth_compare_op(vk::CompareOp::LESS);
    // Multiply: result = destination * source. Rubber darkens what is under it
    // rather than covering it, so the track's own markings stay visible through
    // a skid, and a mark that has faded to white leaves the road untouched.
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ZERO)
        .dst_color_blend_factor(vk::BlendFactor::SRC_COLOR)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ZERO)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
        .alpha_blend_op(vk::BlendOp::ADD)];

    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [swapchain.format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_formats)
        .depth_attachment_format(DEPTH_FORMAT);

    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);

    let pipeline = ctx
        .device
        .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        .expect("create decal pipeline")[0];
    ctx.device.destroy_shader_module(module, None);
    pipeline
}
