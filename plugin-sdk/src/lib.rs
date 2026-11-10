//! SDK for nuggtbot dynamic plugins.
//!
//! A plugin is a `cdylib` crate that exports five C functions. You don't write
//! them by hand: implement two plain Rust functions plus a manifest, then call
//! [`export_plugin!`]. Everything crosses the FFI boundary as JSON inside a C
//! string, so your plugin shares no Rust types with the bot and need not be
//! built with the same compiler.
//!
//! ```ignore
//! use serde_json::{json, Value};
//! fn manifest() -> Value { json!({ "name": "hello", "card": { .. }, "actions": [ .. ] }) }
//! fn run(ctx: Value) -> Value { json!({ "ok": true }) }
//! fn status(cfg: Value) -> Value { json!({ "enabled": true, "reachable": true }) }
//! plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
//! ```
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use serde_json::Value;

/// FFI contract version. The bot refuses a library whose version differs.
pub const ABI_VERSION: u32 = 1;

/// Serialize a `Value` into a C string the bot will hand back to `plugin_free`.
pub fn into_raw(v: &Value) -> *mut c_char {
    match CString::new(v.to_string()) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Parse an incoming C string (JSON) from the bot; null/invalid → `Value::Null`.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated C string.
pub unsafe fn parse(p: *const c_char) -> Value {
    if p.is_null() {
        return Value::Null;
    }
    CStr::from_ptr(p)
        .to_str()
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null)
}

/// Run a `Value`-producing closure, catching panics — a panic unwinding across
/// the FFI boundary is undefined behavior. Returns null on panic.
pub fn guard_ret<F: FnOnce() -> Value + std::panic::UnwindSafe>(f: F) -> *mut c_char {
    match std::panic::catch_unwind(f) {
        Ok(v) => into_raw(&v),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Parse `p`, run `f`, serialize the result; catch panics. Null on panic.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated C string.
pub unsafe fn guard_call<F: FnOnce(Value) -> Value>(p: *const c_char, f: F) -> *mut c_char {
    let v = parse(p);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || f(v))) {
        Ok(r) => into_raw(&r),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a string this plugin previously returned.
///
/// # Safety
/// `p` must be null or a pointer this plugin produced via [`into_raw`].
pub unsafe fn free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

/// Generate the exported C functions from your `manifest`/`run`/`status`, plus an
/// optional `page` for serving your own overlay/browser-dock HTML.
///
/// - `manifest`: an expression returning the manifest `Value` (name, card, actions).
/// - `run`: a `fn(Value) -> Value` — receives `{action,user,params,config}`.
/// - `status`: a `fn(Value) -> Value` — receives the config, returns `{enabled,reachable}`.
/// - `page` (optional): a `fn(Value) -> Value` — receives `{path,config}`, returns
///   `{ok:true, body:"<html>", content_type:"text/html"}`. Served at
///   `/plugin/<name>/<path>`. Declare the pages in your manifest's
///   `card.pages = [{label,path}]` so the panel shows their URLs. Omitting `page`
///   keeps the plugin at exactly the five base functions (still ABI 1).
#[macro_export]
macro_rules! export_plugin {
    (manifest = $manifest:expr, run = $run:path, status = $status:path $(, page = $page:path)? $(,)?) => {
        #[no_mangle]
        pub extern "C" fn plugin_abi_version() -> u32 {
            $crate::ABI_VERSION
        }
        #[no_mangle]
        pub extern "C" fn plugin_manifest() -> *mut ::std::os::raw::c_char {
            $crate::guard_ret(|| $manifest)
        }
        #[no_mangle]
        pub extern "C" fn plugin_run(ctx: *const ::std::os::raw::c_char) -> *mut ::std::os::raw::c_char {
            unsafe { $crate::guard_call(ctx, $run) }
        }
        #[no_mangle]
        pub extern "C" fn plugin_status(cfg: *const ::std::os::raw::c_char) -> *mut ::std::os::raw::c_char {
            unsafe { $crate::guard_call(cfg, $status) }
        }
        #[no_mangle]
        pub extern "C" fn plugin_free(p: *mut ::std::os::raw::c_char) {
            unsafe { $crate::free(p) }
        }
        $(
            #[no_mangle]
            pub extern "C" fn plugin_page(ctx: *const ::std::os::raw::c_char) -> *mut ::std::os::raw::c_char {
                unsafe { $crate::guard_call(ctx, $page) }
            }
        )?
    };
}
