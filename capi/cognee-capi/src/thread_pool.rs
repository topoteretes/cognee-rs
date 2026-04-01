use cognee_core::RayonThreadPool;

use crate::error::set_last_error;

pub struct CgRayonThreadPool {
    pub(crate) inner: RayonThreadPool,
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_rayon_thread_pool_new(num_threads: usize) -> *mut CgRayonThreadPool {
    match RayonThreadPool::new(num_threads) {
        Ok(pool) => Box::into_raw(Box::new(CgRayonThreadPool { inner: pool })),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_rayon_thread_pool_default() -> *mut CgRayonThreadPool {
    match RayonThreadPool::with_default_threads() {
        Ok(pool) => Box::into_raw(Box::new(CgRayonThreadPool { inner: pool })),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_rayon_thread_pool_destroy(pool: *mut CgRayonThreadPool) {
    if !pool.is_null() {
        unsafe { drop(Box::from_raw(pool)) };
    }
}
