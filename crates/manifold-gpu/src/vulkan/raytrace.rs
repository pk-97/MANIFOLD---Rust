//! Ray tracing on Vulkan (`VK_KHR_acceleration_structure` and
//! `VK_KHR_ray_query`) is P7. Until then only the shared data types exist
//! and `GpuDevice::supports_ray_tracing` is false.

pub use crate::raytrace_types::*;
