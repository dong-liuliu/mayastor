use std::{
    collections::HashMap,
    convert::TryFrom,
    ffi::CString,
    os::raw::{c_int, c_void},
};

use async_trait::async_trait;
use futures::channel::oneshot;
use snafu::ResultExt;
use url::Url;

use spdk_rs::libspdk::{
    vbdev_ocf,
    vbdev_ocf_construct,
    vbdev_ocf_get_by_name,
    vbdev_ocf_delete_clean,
};

use crate::{
    bdev::{dev::reject_unknown_parameters, util::uri, CreateDestroy, GetName},
    core::UntypedBdev,
    ffihelper::{cb_arg, errno_result_from_i32, done_errno_cb, ErrnoResult, IntoCString},
    nexus_uri::{self, NexusBdevError},
};

#[derive(Debug)]
pub(super) struct Ocf {
    name: String,
    cache_mode: String,
    cache_line_size: u64,
    cache_bdev_name: String,
    core_bdev_name: String,
    uuid: Option<uuid::Uuid>,
}

/// Convert a URI to an Ocf "object"
/// A standard URI for Ocf: ocf:///ocf1?cache_bdev=BdevA&core_bdev=BdevB&cache_mode=wb
/// the format is ocf:///<OCF bdev name>?cache_bdev=<cache bdev name>&core_bdev=<core bdev name>&cache_mode=<cache mode>
impl TryFrom<&Url> for Ocf {
    type Error = NexusBdevError;

    fn try_from(url: &Url) -> Result<Self, Self::Error> {
        let segments = uri::segments(url);

        if segments.is_empty() {
            return Err(NexusBdevError::UriInvalid {
                uri: url.to_string(),
                message: String::from("no path segments"),
            });
        }

        let mut parameters: HashMap<String, String> =
            url.query_pairs().into_owned().collect();

        let cache_mode: String = match parameters.remove("cache_mode") {
            Some(value) => {
                // TODO: value should be one of the six modes {wb,wt,pt,wa,wi,wo} 
                value 
            }
            None => "wb".into(),
        };

        let cache_line_size: u64 = match parameters.remove("cache_line_size") {
            Some(value) => {
                // TODO: {4,8,16,32,64}
                value.parse().context(nexus_uri::IntParamParseError {
                    uri: url.to_string(),
                    parameter: String::from("cache_line_size"),
                    value: value.clone(),
                })?
            }
            None => 0,
        };

        let cache_bdev: String;
        match parameters.remove("cache_bdev") {
            Some(value) => cache_bdev = value,
            None => return Err(NexusBdevError::UriInvalid {
                uri: url.to_string(),
                message: String::from("no cache_bdev"),
            }),
        }

        let core_bdev: String = match parameters.remove("core_bdev") {
            Some(value) => value,
            None => {
                return Err(NexusBdevError::UriInvalid {
                    uri: url.to_string(),
                    message: String::from("no core_bdev"),
                });
            }
        };
        
        let uuid = uri::uuid(parameters.remove("uuid")).context(
            nexus_uri::UuidParamParseError {
                uri: url.to_string(),
            },
        )?;

        reject_unknown_parameters(url, parameters)?;

        Ok(Ocf {
            name: url.path()[1 ..].into(),
            cache_mode,
            cache_line_size,
            cache_bdev_name: cache_bdev,
            core_bdev_name: core_bdev,
            uuid,
        })
    }
}

impl GetName for Ocf {
    fn get_name(&self) -> String {
        self.name.clone()
    }
}

#[async_trait(?Send)]
impl CreateDestroy for Ocf {
    type Error = NexusBdevError;

    /// Create a ocf bdev
    async fn create(&self) -> Result<String, Self::Error> { 
        extern "C" fn done_ocf_construct_cb(
            status: c_int,
            _vbdev: *mut vbdev_ocf,
            cb_arg: *mut c_void,
        ) {
            let sender = unsafe {
                Box::from_raw(cb_arg as *mut oneshot::Sender<ErrnoResult<()>>)
            };

            sender
                .send(errno_result_from_i32((), status))
                .expect("done callback receiver side disappeared");
        }

        if UntypedBdev::lookup_by_name(&self.name).is_some() {
            return Err(NexusBdevError::BdevExists {
                name: self.get_name(),
            });
        }

        let cname = CString::new(self.get_name()).unwrap();
        let ccache_mode = CString::new(self.cache_mode.clone()).unwrap();
        let ccache_name = CString::new(self.cache_bdev_name.clone()).unwrap();
        let ccore_name = CString::new(self.core_bdev_name.clone()).unwrap();
      
        let (sender, receiver) = oneshot::channel::<ErrnoResult<()>>();
  
        unsafe {
            vbdev_ocf_construct(
                cname.as_ptr(),
                ccache_mode.as_ptr(),
                self.cache_line_size,
                ccache_name.as_ptr(),
                ccore_name.as_ptr(),
                false,
                Some(done_ocf_construct_cb),
                cb_arg(sender),
            )
        };
        
        receiver
        .await
        .context(nexus_uri::CancelBdev {
            name: self.name.clone(),
        })?
        .context(nexus_uri::CreateBdev {
            name: self.name.clone(),
        })?;
        
        if let Some(bdev) = UntypedBdev::lookup_by_name(&self.get_name()) {
            if let Some(u) = self.uuid {
                if bdev.uuid_as_string() != u.to_hyphenated().to_string() {
                    error!("Connected to device {} but expect to connect to {} instead", bdev.uuid_as_string(), u.to_hyphenated().to_string());
                }
            };

            return Ok(self.get_name());
        };

        Err(NexusBdevError::BdevNotFound {
            name: self.get_name(),
        })
    }

    /// Destroy the given ocf bdev
    async fn destroy(self: Box<Self>) -> Result<(), Self::Error> {
        let name = String::from(&self.name).into_cstring();
        let ocf_vbdev = unsafe { vbdev_ocf_get_by_name(name.as_ptr()) };
        
        if ocf_vbdev.is_null() {
            return Err(NexusBdevError::BdevNotFound {
                name: self.get_name(),
            });
        }

        let (sender, receiver) = oneshot::channel::<ErrnoResult<()>>();
        let errno = unsafe {
            vbdev_ocf_delete_clean(
                ocf_vbdev,
                Some(done_errno_cb),
                cb_arg(sender),
            )
        };
        
        errno_result_from_i32((), errno).context(
            nexus_uri::DestroyBdev {
                name: self.name.clone(),
            },
        )?;

        receiver
            .await
            .context(nexus_uri::CancelBdev {
                name: self.get_name(),
            })?
            .context(nexus_uri::DestroyBdev {
                name: self.get_name(),
            })
    }
}
