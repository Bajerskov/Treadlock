//! Textures: upload, mipmaps, sampler and descriptor set.

use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};
use gpu_allocator::MemoryLocation;

use super::buffer::Buffer;
use super::context::Context;

pub struct Texture {
    image: vk::Image,
    view: vk::ImageView,
    sampler: vk::Sampler,
    allocation: Option<Allocation>,
    /// Bound as set 1 when drawing anything that uses this texture.
    pub descriptor_set: vk::DescriptorSet,
}

impl Texture {
    /// Upload RGBA8 pixels, generate a full mip chain, and build the descriptor
    /// set that binds it. Mips matter more than usual here: the track recedes
    /// for hundreds of metres, and an unmipped texture would alias badly at
    /// speed, which is exactly when it is most distracting.
    pub fn new(
        ctx: &mut Context,
        layout: vk::DescriptorSetLayout,
        pool: vk::DescriptorPool,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Texture {
        unsafe { Self::create(ctx, layout, pool, width, height, rgba, true) }
    }

    /// Nearest filtering and a single mip level, for the bitmap font atlas.
    /// Smoothing a pixel font turns it to mush, and mips would blend glyphs
    /// into their neighbours.
    pub fn new_pixel_art(
        ctx: &mut Context,
        layout: vk::DescriptorSetLayout,
        pool: vk::DescriptorPool,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Texture {
        unsafe { Self::create(ctx, layout, pool, width, height, rgba, false) }
    }

    /// A single white pixel, bound wherever a draw has no texture of its own, so
    /// the shader never has to branch on whether one is present.
    pub fn white(
        ctx: &mut Context,
        layout: vk::DescriptorSetLayout,
        pool: vk::DescriptorPool,
    ) -> Texture {
        Texture::new(ctx, layout, pool, 1, 1, &[255, 255, 255, 255])
    }

    unsafe fn create(
        ctx: &mut Context,
        layout: vk::DescriptorSetLayout,
        pool: vk::DescriptorPool,
        width: u32,
        height: u32,
        rgba: &[u8],
        smooth: bool,
    ) -> Texture {
        let mip_levels = if smooth {
            (width.max(height) as f32).log2().floor() as u32 + 1
        } else {
            1
        };
        let filter = if smooth { vk::Filter::LINEAR } else { vk::Filter::NEAREST };

        let image = ctx
            .device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(mip_levels)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    // TRANSFER_SRC as well as DST, because generating mips blits
                    // each level from the one above it.
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .expect("create texture image");

        let requirements = ctx.device.get_image_memory_requirements(image);
        let allocation = ctx
            .allocator
            .as_mut()
            .unwrap()
            .allocate(&AllocationCreateDesc {
                name: "texture",
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::DedicatedImage(image),
            })
            .expect("allocate texture memory");
        ctx.device
            .bind_image_memory(image, allocation.memory(), allocation.offset())
            .unwrap();

        let mut staging = Buffer::new(
            ctx,
            "texture staging",
            rgba.len() as vk::DeviceSize,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
        );
        staging.write(rgba);

        ctx.submit_now(|cmd| {
            transition(ctx, cmd, image, 0, mip_levels, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            ctx.device.cmd_copy_buffer_to_image(
                cmd,
                staging.handle,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .mip_level(0)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D { width, height, depth: 1 })],
            );
            generate_mips(ctx, cmd, image, width, height, mip_levels);
        });
        staging.destroy(ctx);

        let view = ctx
            .device
            .create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(mip_levels)
                            .layer_count(1),
                    ),
                None,
            )
            .expect("texture view");

        let sampler = ctx
            .device
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(filter)
                    .min_filter(filter)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    // Clamp for the atlas, so a glyph at the edge cannot wrap
                    // round and pick up the opposite side.
                    .address_mode_u(if smooth { vk::SamplerAddressMode::REPEAT } else { vk::SamplerAddressMode::CLAMP_TO_EDGE })
                    .address_mode_v(if smooth { vk::SamplerAddressMode::REPEAT } else { vk::SamplerAddressMode::CLAMP_TO_EDGE })
                    .address_mode_w(vk::SamplerAddressMode::REPEAT)
                    .max_lod(mip_levels as f32)
                    // Anisotropy is what keeps a road surface readable at a
                    // glancing angle, which is the whole view in a racer.
                    .anisotropy_enable(smooth)
                    .max_anisotropy(8.0),
                None,
            )
            .expect("sampler");

        let set_layouts = [layout];
        let descriptor_set = ctx
            .device
            .allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&set_layouts),
            )
            .expect("texture descriptor set")[0];

        // Image and sampler are written separately, matching how naga lowers a
        // WGSL texture and sampler pair.
        let image_info = [vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(view)];
        let sampler_info = [vk::DescriptorImageInfo::default().sampler(sampler)];
        ctx.device.update_descriptor_sets(
            &[
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&image_info),
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&sampler_info),
            ],
            &[],
        );

        Texture { image, view, sampler, allocation: Some(allocation), descriptor_set }
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        unsafe {
            ctx.device.destroy_sampler(self.sampler, None);
            ctx.device.destroy_image_view(self.view, None);
            ctx.device.destroy_image(self.image, None);
        }
        if let (Some(alloc), Some(allocation)) = (ctx.allocator.as_mut(), self.allocation.take()) {
            alloc.free(allocation).ok();
        }
    }
}

/// Successively halve the image, blitting each level from the one above.
unsafe fn generate_mips(
    ctx: &Context,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    width: u32,
    height: u32,
    mip_levels: u32,
) {
    let (mut w, mut h) = (width as i32, height as i32);
    for level in 1..mip_levels {
        // The source level has just been written, so move it to TRANSFER_SRC.
        transition(
            ctx, cmd, image, level - 1, 1,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        );

        let next_w = (w / 2).max(1);
        let next_h = (h / 2).max(1);
        let blit = [vk::ImageBlit::default()
            .src_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(level - 1)
                    .layer_count(1),
            )
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: w, y: h, z: 1 },
            ])
            .dst_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(level)
                    .layer_count(1),
            )
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: next_w, y: next_h, z: 1 },
            ])];
        ctx.device.cmd_blit_image(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &blit,
            vk::Filter::LINEAR,
        );

        // Done reading from it, so it can take its final layout.
        transition(
            ctx, cmd, image, level - 1, 1,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        w = next_w;
        h = next_h;
    }
    // The last level was never blitted from, so it is still TRANSFER_DST.
    transition(
        ctx, cmd, image, mip_levels - 1, 1,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    );
}

unsafe fn transition(
    ctx: &Context,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    base_level: u32,
    level_count: u32,
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
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(base_level)
                .level_count(level_count)
                .layer_count(1),
        )];
    ctx.device.cmd_pipeline_barrier2(
        cmd,
        &vk::DependencyInfo::default().image_memory_barriers(&barrier),
    );
}
