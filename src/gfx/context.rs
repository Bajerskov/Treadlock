//! Vulkan instance, device and allocator setup.

use std::ffi::{c_char, CStr};

use ash::{khr, vk};
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub struct Context {
    /// Owns the loaded Vulkan library. Every other handle here is invalid once
    /// this is dropped, so it is held for its lifetime rather than its use.
    #[allow(dead_code)]
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface_loader: khr::surface::Instance,
    pub surface: vk::SurfaceKHR,
    pub physical_device: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    #[allow(dead_code)]
    pub queue_family: u32,
    pub allocator: Option<Allocator>,
    pub command_pool: vk::CommandPool,
    pub device_name: String,
    /// True when the driver exposes ray queries. On the BC-250 this decides
    /// whether reflections can trace real rays or must fall back to
    /// screen-space only, so it is detected rather than assumed.
    pub ray_query: bool,
}

impl Context {
    pub fn new(window: &winit::window::Window) -> Context {
        unsafe { Self::create(window) }
    }

    unsafe fn create(window: &winit::window::Window) -> Context {
        let entry = ash::Entry::load().expect("no Vulkan loader found (install a Vulkan driver)");

        let app_name = CStr::from_bytes_with_nul(b"Treadlock\0").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(app_name)
            .api_version(vk::API_VERSION_1_3);

        let display_handle = window.display_handle().unwrap().as_raw();
        let mut extensions = ash_window::enumerate_required_extensions(display_handle)
            .expect("unsupported windowing system")
            .to_vec();
        extensions.push(khr::get_physical_device_properties2::NAME.as_ptr());

        let instance = entry
            .create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app_info)
                    .enabled_extension_names(&extensions),
                None,
            )
            .expect("failed to create Vulkan instance");

        let surface = ash_window::create_surface(
            &entry,
            &instance,
            display_handle,
            window.window_handle().unwrap().as_raw(),
            None,
        )
        .expect("failed to create window surface");
        let surface_loader = khr::surface::Instance::new(&entry, &instance);

        let (physical_device, queue_family) =
            pick_device(&instance, &surface_loader, surface).expect("no suitable GPU found");

        let props = instance.get_physical_device_properties(physical_device);
        let device_name = CStr::from_ptr(props.device_name.as_ptr())
            .to_string_lossy()
            .into_owned();

        let supported = instance
            .enumerate_device_extension_properties(physical_device)
            .unwrap_or_default();
        let has = |name: &CStr| {
            supported
                .iter()
                .any(|e| CStr::from_ptr(e.extension_name.as_ptr()) == name)
        };

        // Ray queries need the acceleration-structure extension too; taking one
        // without the other would fail device creation.
        let ray_query = has(khr::ray_query::NAME)
            && has(khr::acceleration_structure::NAME)
            && has(khr::deferred_host_operations::NAME);

        let mut device_extensions: Vec<*const c_char> = vec![khr::swapchain::NAME.as_ptr()];
        if ray_query {
            device_extensions.push(khr::ray_query::NAME.as_ptr());
            device_extensions.push(khr::acceleration_structure::NAME.as_ptr());
            device_extensions.push(khr::deferred_host_operations::NAME.as_ptr());
        }

        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];

        let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let mut features12 = vk::PhysicalDeviceVulkan12Features::default()
            .buffer_device_address(ray_query);
        let mut accel = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default()
            .acceleration_structure(true);
        let mut rq = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);

        let mut create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut features13)
            .push_next(&mut features12);
        if ray_query {
            create_info = create_info.push_next(&mut accel).push_next(&mut rq);
        }

        let device = instance
            .create_device(physical_device, &create_info, None)
            .expect("failed to create logical device");
        let queue = device.get_device_queue(queue_family, 0);

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device,
            debug_settings: Default::default(),
            buffer_device_address: ray_query,
            allocation_sizes: Default::default(),
        })
        .expect("failed to create GPU allocator");

        let command_pool = device
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
            .expect("failed to create command pool");

        Context {
            entry,
            instance,
            surface_loader,
            surface,
            physical_device,
            device,
            queue,
            queue_family,
            allocator: Some(allocator),
            command_pool,
            device_name,
            ray_query,
        }
    }

    /// Run a one-shot command buffer and wait for it. Used for uploads during
    /// load, where a stall costs nothing.
    pub unsafe fn submit_now(&self, record: impl FnOnce(vk::CommandBuffer)) {
        let cmd = self
            .device
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
            .expect("allocate command buffer")[0];

        self.device
            .begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .unwrap();
        record(cmd);
        self.device.end_command_buffer(cmd).unwrap();

        let buffers = [cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&buffers);
        self.device
            .queue_submit(self.queue, &[submit], vk::Fence::null())
            .unwrap();
        self.device.queue_wait_idle(self.queue).unwrap();
        self.device.free_command_buffers(self.command_pool, &buffers);
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            self.device.device_wait_idle().ok();
            // The allocator must release its memory before the device goes away.
            drop(self.allocator.take());
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

unsafe fn pick_device(
    instance: &ash::Instance,
    surface_loader: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Option<(vk::PhysicalDevice, u32)> {
    let devices = instance.enumerate_physical_devices().ok()?;
    let mut best: Option<(u32, vk::PhysicalDevice, u32)> = None;

    for device in devices {
        let families = instance.get_physical_device_queue_family_properties(device);
        let family = families.iter().enumerate().position(|(i, f)| {
            f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                && surface_loader
                    .get_physical_device_surface_support(device, i as u32, surface)
                    .unwrap_or(false)
        });
        let Some(family) = family else { continue };

        let props = instance.get_physical_device_properties(device);
        // The BC-250 reports as integrated because it is an APU, so integrated
        // must not be treated as a last resort.
        let score = match props.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            _ => 1,
        };
        if best.as_ref().map_or(true, |(s, _, _)| score > *s) {
            best = Some((score, device, family as u32));
        }
    }
    best.map(|(_, d, f)| (d, f))
}
