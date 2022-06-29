//! Utility functions and wrappers for working with vhost devices in SPDK.
use nix::{errno::Errno,};
use snafu::{Snafu,};
use std::{
    ffi::{CStr, CString},
    fmt,
    os::raw::c_char,
};

// int spdk_vhost_blk_construct(const char *name, const char *cpumask,
//    const char *dev_name, const struct spdk_json_val *params)
// void spdk_vhost_lock(void);
// struct spdk_vhost_dev *spdk_vhost_dev_find(const char *ctrlr_name);
// int spdk_vhost_dev_remove(struct spdk_vhost_dev *vdev);
// void spdk_vhost_unlock(void);

// TODO：use set path, add get path
// int spdk_vhost_set_socket_path(const char *basename);
// const char *spdk_vhost_get_socket_path(void);

// struct spdk_json_write_ctx;
// #define SPDK_JSON_WRITE_FLAG_FORMATTED	0x00000001
// typedef int (*spdk_json_write_cb)(void *cb_ctx, const void *data, size_t size);
// struct spdk_json_write_ctx *spdk_json_write_begin(spdk_json_write_cb write_cb, void *cb_ctx, 		uint32_t flags);
// int spdk_json_write_end(struct spdk_json_write_ctx *w);
// int spdk_json_write_val(struct spdk_json_write_ctx *w, const struct spdk_json_val *val);
// ssize_t spdk_json_parse(void *json, size_t size, struct spdk_json_val *values, size_t num_values, void **end, uint32_t flags);

// how to generate struct spdk_json_val *params:
//  spdk_json_write_begin(cb_fn, ...);
//  spdk_json_write_named_...();
//  spdk_json_write_end();
//  spdk_json_parse like in test/app/jsoncat/..

use spdk_rs::libspdk::{
    spdk_vhost_dev,
    spdk_vhost_blk_construct,
    spdk_vhost_dev_find,
    spdk_vhost_dev_remove,
    spdk_vhost_lock,
    spdk_vhost_unlock,
    spdk_vhost_dev_get_name,
    spdk_vhost_get_socket_path,
    spdk_cpuset_alloc,
    spdk_cpuset_set_cpu,
    spdk_cpuset_free,
    spdk_cpuset_fmt,
};

use crate::{
    ffihelper::{errno_result_from_i32},
    core::{Cores, Reactors},
};

#[derive(Debug, Snafu)]
pub enum VhostblkError {
    #[snafu(display("Failed to construct vhostblk on {}", dev))]
    ConstructVhostblk { source: Errno, dev: String },
}


fn roundrobin_cpumask() -> String {
    static mut LAST_CORE: u32 = 255;

    let last_core = unsafe {LAST_CORE};
    let last_reactor = Reactors::iter().find(|c| c.core() >= last_core);
    let next_core = match last_reactor {
        Some(_r) => {
            unsafe { LAST_CORE = last_core + 1;}
            if let Some(_y) = Reactors::iter().find(|c| c.core() >= last_core) {
                unsafe {LAST_CORE}
            } else {
                unsafe {LAST_CORE = Cores::first();}
                unsafe {LAST_CORE}
            }
        }
        None => {
            unsafe {LAST_CORE = Cores::first();}
            unsafe {LAST_CORE}
        }
    };

    let str_buf: String;
    unsafe {

        let cpuset = spdk_cpuset_alloc();

        spdk_cpuset_set_cpu(cpuset, next_core, true);
        let c_buf: *const c_char = spdk_cpuset_fmt(cpuset);
        let c_str: &CStr = CStr::from_ptr(c_buf);
        let str_slice: &str = c_str.to_str().unwrap();
        str_buf = str_slice.to_owned();

        spdk_cpuset_free(cpuset);
    }
    
    return str_buf;
}

fn start(
    vhostblk_name: &str,
    bdev_name: &str,
) -> Result<*mut spdk_vhost_dev, VhostblkError> {
    let c_bdev_name = CString::new(bdev_name).unwrap();
    let vhostblk_name = CString::new(vhostblk_name).unwrap();
    let readonly = false;
    let packed_ring = false;

    let cpumask = roundrobin_cpumask();
    let next_cpumask = CString::new(cpumask.as_str()).unwrap();

    let success = unsafe {
        spdk_vhost_blk_construct(
            vhostblk_name.as_ptr(),
            next_cpumask.as_ptr(),
            c_bdev_name.as_ptr(),
            readonly,
            packed_ring,
        )
    };
    match errno_result_from_i32(success, success) {
        Err(errno) => {
            return Err(VhostblkError::ConstructVhostblk {
                source: errno,
                dev: bdev_name.to_owned(),
            });
        }
        Ok(_) =>{}
    }

    let vblk_ptr = unsafe {
        spdk_vhost_dev_find(vhostblk_name.as_ptr())
    };

    Ok(vblk_ptr)
}

/// VhostblkCtrlr representation.
pub struct VhostblkCtrlr {
    vblk_ptr: *mut spdk_vhost_dev,
}

impl VhostblkCtrlr {
    pub fn create(bdev_name: &str) -> Result<Self, VhostblkError> {
        let vblk_name = format!("{}_vhostblk", bdev_name);

        let vblk_ptr = start(&vblk_name, bdev_name)?;
        info!("Construct vhostblk disk {} for {}", vblk_name, bdev_name);

        Ok(Self {
            vblk_ptr,
        })
    }

    pub fn destroy(self) {
        let vblk_ptr = self.vblk_ptr;

        let name = self.get_name();
        debug!("Destroy vhostblk device {}...", name);

        unsafe {
            spdk_vhost_lock();
            let rc = spdk_vhost_dev_remove(vblk_ptr);
	        spdk_vhost_unlock();
            if rc < 0 {
                error!("Failed to destroy vhostblk device {}...", name);
            }
        }
        debug!("vhostblk device destroied successfully");
    }

    /// Get vhostblk device name.
    fn get_name(&self) -> String {
        unsafe {
            CStr::from_ptr(spdk_vhost_dev_get_name(self.vblk_ptr))
                .to_str()
                .unwrap()
                .to_string()
        }
    }

    /// Get vhostblk device sock dir path.
    fn get_sock_dir(&self) -> String {
        unsafe {
            CStr::from_ptr(spdk_vhost_get_socket_path())
                .to_str()
                .unwrap()
                .to_string()
        }
    }

    /// Get vhostblk device path uri (file://<sock-dir>/<sock-name>...).
    pub fn as_uri(&self) -> String {
        format!("file://{}{}", self.get_sock_dir(), self.get_name())
    }
}

impl fmt::Debug for VhostblkCtrlr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{:?}", self.get_name(), self.vblk_ptr)
    }
}

impl fmt::Display for VhostblkCtrlr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get_name())
    }
}
