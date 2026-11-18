//! Shared-memory buffers (wl_shm): apps draw pixels into memory they share
//! with us through a file descriptor.

use memmap2::{Mmap, MmapOptions};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Mutex;
use wayland_server::protocol::wl_shm;
use wayland_server::WEnum;

pub struct Pool {
    fd: OwnedFd,
    map: Mutex<Option<Mmap>>,
}

impl Pool {
    pub fn new(fd: OwnedFd, size: i32) -> Self {
        let map = Self::map(&fd, size);
        Self { fd, map: Mutex::new(map) }
    }

    fn map(fd: &OwnedFd, size: i32) -> Option<Mmap> {
        if size <= 0 {
            return None;
        }
        // Safety: the memory belongs to the client and can change under us,
        // which is fine for pixels. (A client shrinking the file could still
        // fault us; guarding against that with SIGBUS handling comes later.)
        unsafe { MmapOptions::new().len(size as usize).map(fd.as_raw_fd()) }.ok()
    }

    pub fn is_mapped(&self) -> bool {
        self.map.lock().unwrap().is_some()
    }

    /// Pools may only grow.
    pub fn resize(&self, size: i32) {
        let mut m = self.map.lock().unwrap();
        if m.as_ref().map_or(true, |m| size as usize > m.len()) {
            if let Some(new) = Self::map(&self.fd, size) {
                *m = Some(new);
            }
        }
    }
}

pub struct BufferData {
    pool: std::sync::Arc<Pool>,
    offset: usize,
    pub width: u32,
    pub height: u32,
    stride: usize,
    /// XRGB: ignore alpha. ARGB: blend.
    pub opaque: bool,
    supported: bool,
}

impl BufferData {
    pub fn new(pool: std::sync::Arc<Pool>, offset: i32, width: i32, height: i32, stride: i32, format: WEnum<wl_shm::Format>) -> Self {
        let (opaque, supported) = match format {
            WEnum::Value(wl_shm::Format::Xrgb8888) => (true, true),
            WEnum::Value(wl_shm::Format::Argb8888) => (false, true),
            _ => (false, false),
        };
        Self {
            pool,
            offset: offset.max(0) as usize,
            width: width.max(0) as u32,
            height: height.max(0) as u32,
            stride: stride.max(0) as usize,
            opaque,
            supported,
        }
    }

    /// Copy the pixels out (tightly packed, w*h). None if the buffer is
    /// bad: unsupported format or outside the pool.
    pub fn read(&self) -> Option<Vec<u32>> {
        if !self.supported || self.stride < self.width as usize * 4 {
            return None;
        }
        let m = self.pool.map.lock().unwrap();
        let m = m.as_ref()?;
        let end = self.offset + self.stride * self.height as usize;
        if self.height == 0 || end > m.len() {
            return None;
        }
        let row_bytes = self.width as usize * 4;
        let mut out = Vec::with_capacity((self.width * self.height) as usize);
        for y in 0..self.height as usize {
            let start = self.offset + y * self.stride;
            out.extend(
                m[start..start + row_bytes]
                    .chunks_exact(4)
                    .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])),
            );
        }
        if self.opaque {
            for p in &mut out {
                *p |= 0xff00_0000;
            }
        }
        Some(out)
    }
}
