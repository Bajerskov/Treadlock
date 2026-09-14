//! Swapchain and depth buffer, recreated whenever the window resizes.

use ash::{khr, vk};
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};
use gpu_allocator::MemoryLocation;

use super::context::Context;

pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

pub struct Swapchain {
    pub loader: khr::swapchain::Device,
    pub handle: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    pub depth_image: vk::Image,
    pub depth_view: vk::ImageView,
    depth_allocation: Option<Allocation>,
}

impl Swapchain {
    pub fn new(ctx: &mut Context, width: u32, height: u32, vsync: bool) -> Swapchain {
        unsafe { Self::create(ctx, width, height, vsync, vk::SwapchainKHR::null()) }
    }

    pub fn recreate(&mut self, ctx: &mut Context, width: u32, height: u32, vsync: bool) {
        unsafe {
            ctx.device.device_wait_idle().ok();
            let old = self.handle;
            let fresh = Self::create(ctx, width, height, vsync, old);
            self.destroy_resources(ctx);
            self.loader.destroy_swapchain(old, None);
            let loader = std::mem::replace(&mut self.loader, fresh.loader);
            drop(loader);
            self.handle = fresh.handle;
            self.format = fresh.format;
            self.extent = fresh.extent;
            self.images = fresh.images;
            self.views = fresh.views;
            self.depth_image = fresh.depth_image;
            self.depth_view = fresh.depth_view;
            self.depth_allocation = fresh.depth_allocation;
        }
    }

    unsafe fn create(
        ctx: &mut Context,
        width: u32,
        height: u32,
        vsync: bool,
        old: vk::SwapchainKHR,
    ) -> Swapchain {
        let caps = ctx
            .surface_loader
            .get_physical_device_surface_capabilities(ctx.physical_device, ctx.surface)
            .expect("surface capabilities");
        let formats = ctx
            .surface_loader
            .get_physical_device_surface_formats(ctx.physical_device, ctx.surface)
            .expect("surface formats");
        let present_modes = ctx
            .surface_loader
            .get_physical_device_surface_present_modes(ctx.physical_device, ctx.surface)
            .expect("present modes");

        let surface_format = formats
            .iter()
            .find(|f| {
                f.format == vk::Format::B8G8R8A8_SRGB
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .copied()
            .unwrap_or(formats[0]);

        // FIFO is always available. Mailbox tears less than IMMEDIATE when
        // vsync is off, so prefer it before falling back.
        let present_mode = if vsync {
            vk::PresentModeKHR::FIFO
        } else {
            [vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::IMMEDIATE]
                .into_iter()
                .find(|m| present_modes.contains(m))
                .unwrap_or(vk::PresentModeKHR::FIFO)
        };

        // A current_extent of u32::MAX means the surface defers to our choice.
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };

        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 && image_count > caps.max_image_count {
            image_count = caps.max_image_count;
        }

        let loader = khr::swapchain::Device::new(&ctx.instance, &ctx.device);
        let handle = loader
            .create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(ctx.surface)
                    .min_image_count(image_count)
                    .image_format(surface_format.format)
                    .image_color_space(surface_format.color_space)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(caps.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(present_mode)
                    .clipped(true)
                    .old_swapchain(old),
                None,
            )
            .expect("failed to create swapchain");

        let images = loader.get_swapchain_images(handle).unwrap();
        let views = images
            .iter()
            .map(|&image| {
                ctx.device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(surface_format.format)
                            .subresource_range(color_range()),
                        None,
                    )
                    .unwrap()
            })
            .collect();

        let (depth_image, depth_view, depth_allocation) = create_depth(ctx, extent);

        Swapchain {
            loader,
            handle,
            format: surface_format.format,
            extent,
            images,
            views,
            depth_image,
            depth_view,
            depth_allocation: Some(depth_allocation),
        }
    }

    unsafe fn destroy_resources(&mut self, ctx: &mut Context) {
        for &view in &self.views {
            ctx.device.destroy_image_view(view, None);
        }
        self.views.clear();
        ctx.device.destroy_image_view(self.depth_view, None);
        ctx.device.destroy_image(self.depth_image, None);
        if let (Some(alloc), Some(allocation)) = (ctx.allocator.as_mut(), self.depth_allocation.take())
        {
            alloc.free(allocation).ok();
        }
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        unsafe {
            self.destroy_resources(ctx);
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}

unsafe fn create_depth(
    ctx: &mut Context,
    extent: vk::Extent2D,
) -> (vk::Image, vk::ImageView, Allocation) {
    let image = ctx
        .device
        .create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(DEPTH_FORMAT)
                .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )
        .expect("create depth image");

    let requirements = ctx.device.get_image_memory_requirements(image);
    let allocation = ctx
        .allocator
        .as_mut()
        .unwrap()
        .allocate(&AllocationCreateDesc {
            name: "depth",
            requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::DedicatedImage(image),
        })
        .expect("allocate depth memory");
    ctx.device
        .bind_image_memory(image, allocation.memory(), allocation.offset())
        .unwrap();

    let view = ctx
        .device
        .create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(DEPTH_FORMAT)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::DEPTH)
                        .level_count(1)
                        .layer_count(1),
                ),
            None,
        )
        .unwrap();
    (image, view, allocation)
}

pub fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}
