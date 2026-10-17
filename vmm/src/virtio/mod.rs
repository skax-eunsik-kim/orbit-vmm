//! virtio 1.x over MMIO: transport, split virtqueue, and the devices.

pub mod balloon;
pub mod blk;
pub mod fs;
pub mod hostfs;
pub mod mmio;
pub mod net;
pub mod queue;
pub mod vsock;

use std::sync::Arc;

use crate::memory::GuestMemory;
pub use queue::{Descriptor, Queue};

pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

pub const DEVICE_ID_BLOCK: u32 = 2;
pub const DEVICE_ID_BALLOON: u32 = 5;

/// Raised by a device to interrupt the guest; implemented by the MMIO transport
/// (sets InterruptStatus bits and drives the IrqLine).
pub trait VirtioInterrupt: Send + Sync {
    fn signal_used_queue(&self);
    #[allow(dead_code)] // part of the transport; no device changes its config yet
    fn signal_config_change(&self);
}

/// A virtio device behind the MMIO transport.
///
/// Call order from the transport: features() / read_config() any time;
/// activate() once the driver sets DRIVER_OK; queue_notify() on each
/// QueueNotify write after that; reset() when the driver writes Status = 0.
pub trait VirtioDevice: Send {
    fn device_id(&self) -> u32;
    /// Offered feature bits. Must include VIRTIO_F_VERSION_1.
    fn features(&self) -> u64;
    /// Number of virtqueues the device exposes (the transport offers this many).
    fn num_queues(&self) -> usize;
    fn queue_max_size(&self) -> u16 {
        256
    }
    fn read_config(&self, offset: u64, data: &mut [u8]);
    fn write_config(&mut self, offset: u64, data: &[u8]);
    /// `queues[i]` is virtqueue index i as the driver configured it (ready or not).
    fn activate(
        &mut self,
        mem: Arc<GuestMemory>,
        queues: Vec<Queue>,
        interrupt: Arc<dyn VirtioInterrupt>,
        acked_features: u64,
    );
    fn queue_notify(&mut self, queue_index: u16);
    fn reset(&mut self) {}
}
