//! GPU buffers with staged uploads.

use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};
use gpu_allocator::MemoryLocation;

use super::context::Context;

pub struct Buffer {
    pub handle: vk::Buffer,
    pub size: vk::DeviceSize,
    allocation: Option<Allocation>,
}

impl Buffer {
    pub fn new(
        ctx: &mut Context,
        name: &str,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
    ) -> Buffer {
        unsafe {
            let handle = ctx
                .device
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(size)
                        .usage(usage)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE),
                    None,
                )
                .expect("create buffer");
            let requirements = ctx.device.get_buffer_memory_requirements(handle);
            let allocation = ctx
                .allocator
                .as_mut()
                .unwrap()
                .allocate(&AllocationCreateDesc {
                    name,
                    requirements,
                    location,
                    linear: true,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .expect("allocate buffer memory");
            ctx.device
                .bind_buffer_memory(handle, allocation.memory(), allocation.offset())
                .unwrap();
            Buffer { handle, size, allocation: Some(allocation) }
        }
    }

    /// Upload POD data to a device-local buffer via a temporary staging copy.
    pub fn device_local<T: bytemuck::Pod>(
        ctx: &mut Context,
        name: &str,
        data: &[T],
        usage: vk::BufferUsageFlags,
    ) -> Buffer {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        let size = bytes.len() as vk::DeviceSize;

        let mut staging = Buffer::new(
            ctx,
            "staging",
            size,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
        );
        staging.write(bytes);

        let target = Buffer::new(
            ctx,
            name,
            size,
            usage | vk::BufferUsageFlags::TRANSFER_DST,
            MemoryLocation::GpuOnly,
        );

        unsafe {
            ctx.submit_now(|cmd| {
                ctx.device.cmd_copy_buffer(
                    cmd,
                    staging.handle,
                    target.handle,
                    &[vk::BufferCopy::default().size(size)],
                );
            });
        }
        staging.destroy(ctx);
        target
    }

    /// Copy bytes into a host-visible buffer.
    pub fn write(&mut self, bytes: &[u8]) {
        let slice = self
            .allocation
            .as_mut()
            .unwrap()
            .mapped_slice_mut()
            .expect("buffer is not host visible");
        slice[..bytes.len()].copy_from_slice(bytes);
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        unsafe {
            ctx.device.destroy_buffer(self.handle, None);
        }
        if let (Some(alloc), Some(allocation)) = (ctx.allocator.as_mut(), self.allocation.take()) {
            alloc.free(allocation).ok();
        }
    }
}
