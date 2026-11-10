//! Drop-in dynamic plugin system.
//!
//! A plugin is a separate Rust crate compiled to a C-ABI shared library
//! (`cdylib` → `.so` on Linux, `.dll` on Windows). Drop the built library into
//! `plugins/`, click "Reiniciar bot" in the panel, and the bot loads it at
//! startup. **Adding or removing a plugin never touches or rebuilds the bot.**
//!
//! The FFI surface is deliberately tiny and stable: everything crosses the
//! boundary as JSON inside a C string, so plugin and bot share no Rust types and
//! need not be built with the same compiler. A library exports five functions
//! (see `plugin-sdk`): `plugin_abi_version`, `plugin_manifest`, `plugin_run`,
//! `plugin_status`, `plugin_free`.
//!
//! Each plugin keeps its config in `plugins/<name>.config.json` (managed here
//! from the Integrations card); the bot passes that config into every call.
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped when the FFI contract changes; a library built against a different
/// version is refused at load time rather than invoked (which would be UB).
const ABI_VERSION: u32 = 1;

// The five C-ABI functions every plugin library exports.
type VersionFn = unsafe extern "C" fn() -> u32;
type ManifestFn = unsafe extern "C" fn() -> *mut c_char;
type CallFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);

/// Health reported for a plugin's card badge.
#[derive(Serialize, Deserialize, Default)]
pub struct StatusResult {
    pub enabled: bool,
    pub reachable: bool,
}

/// One loaded plugin library, kept alive for the whole process (dropping the
/// `Library` would unload its code and dangle the symbols we call).
pub struct Loaded {
    pub name: String,
    /// The plugin's manifest JSON: `{ name, card, actions }`.
    manifest: Value,
    lib: libloading::Library,
}

static REGISTRY: OnceLock<Vec<Loaded>> = OnceLock::new();

/// Scan `plugins/` once at startup and load every `.so`/`.dll` whose ABI matches.
/// Bad or mismatched libraries are logged and skipped, never fatal.
pub fn load_all() {
    let mut loaded = Vec::new();
    if let Ok(rd) = std::fs::read_dir("plugins") {
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && matches!(p.extension().and_then(|s| s.to_str()), Some("so") | Some("dll"))
            })
            .collect();
        files.sort();
        for path in files {
            match unsafe { load_one(&path) } {
                Ok(p) => {
                    tracing::info!("plugin loaded: {} ({})", p.name, path.display());
                    loaded.push(p);
                }
                Err(e) => tracing::warn!("plugin skipped {}: {e}", path.display()),
            }
        }
    }
    let _ = REGISTRY.set(loaded);
}

/// Every loaded plugin (empty until `load_all` runs).
pub fn registry() -> &'static [Loaded] {
    REGISTRY.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// # Safety: `path` must point to a library exporting the plugin ABI.
unsafe fn load_one(path: &Path) -> anyhow::Result<Loaded> {
    let lib = libloading::Library::new(path)?;
    let version: libloading::Symbol<VersionFn> = lib.get(b"plugin_abi_version\0")?;
    let v = version();
    if v != ABI_VERSION {
        anyhow::bail!("plugin ABI {v} != {ABI_VERSION}");
    }
    let manifest_fn: libloading::Symbol<ManifestFn> = lib.get(b"plugin_manifest\0")?;
    let free_fn: libloading::Symbol<FreeFn> = lib.get(b"plugin_free\0")?;
    // Required for dispatch/status later; fail fast now if missing.
    let _: libloading::Symbol<CallFn> = lib.get(b"plugin_run\0")?;
    let _: libloading::Symbol<CallFn> = lib.get(b"plugin_status\0")?;
    let manifest = take_json(manifest_fn(), *free_fn).ok_or_else(|| anyhow::anyhow!("bad manifest"))?;
    let name = manifest
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("manifest has no name"))?
        .to_string();
    // Symbols borrow `lib`; their last use is above, so `lib` is free to move now.
    Ok(Loaded { name, manifest, lib })
}

/// Copy a plugin-allocated C string to an owned `Value`, then free it with the
/// plugin's own allocator. `# Safety:` `ptr` is null or a plugin-returned string.
unsafe fn take_json(ptr: *mut c_char, free: FreeFn) -> Option<Value> {
    if ptr.is_null() {
        return None;
    }
    let owned = CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_owned());
    free(ptr);
    owned.and_then(|s| serde_json::from_str(&s).ok())
}

/// Names must map to a single file in `plugins/`; reject anything that could
/// escape it (path traversal) before touching the filesystem.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn config_path(name: &str) -> PathBuf {
    PathBuf::from("plugins").join(format!("{name}.config.json"))
}

/// A plugin's saved config (`plugins/<name>.config.json`), or `{}` if none.
pub fn read_config(name: &str) -> Value {
    if !safe_name(name) {
        return serde_json::json!({});
    }
    std::fs::read_to_string(config_path(name))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Overwrite a plugin's config. Caller must have checked `name` is registered.
pub fn write_config(name: &str, cfg: &Value) -> std::io::Result<()> {
    if !safe_name(name) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad plugin name"));
    }
    std::fs::write(config_path(name), serde_json::to_vec_pretty(cfg)?)
}

/// Serializable panel view of a plugin: its manifest plus the saved config.
pub fn view(p: &Loaded) -> Value {
    let mut v = p.manifest.clone();
    v["config"] = read_config(&p.name);
    v
}

/// Run a plugin sub-action. The sync FFI call runs on a blocking thread so it
/// never stalls the async runtime; the plugin owns its own I/O and concurrency.
pub async fn dispatch(plugin: &str, action: &str, params: BTreeMap<String, String>, user: &str) {
    let config = read_config(plugin);
    let ctx = serde_json::json!({
        "action": action,
        "user": user,
        "params": params,
        "config": config,
    });
    let plugin = plugin.to_string();
    let action = action.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        let Some(p) = registry().iter().find(|p| p.name == plugin) else {
            tracing::warn!("plugin not found: {plugin}");
            return;
        };
        let known = p
            .manifest
            .get("actions")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().any(|x| x.get("name").and_then(|n| n.as_str()) == Some(action.as_str())))
            .unwrap_or(true);
        if !known {
            tracing::warn!("plugin {plugin}: unknown action {action}");
            return;
        }
        unsafe {
            let (Ok(run), Ok(free)) = (
                p.lib.get::<CallFn>(b"plugin_run\0"),
                p.lib.get::<FreeFn>(b"plugin_free\0"),
            ) else {
                return;
            };
            let Ok(input) = CString::new(ctx.to_string()) else { return };
            let out = run(input.as_ptr());
            // A plugin may report trouble as {"ok":false,"error":"…"}; log it.
            if let Some(v) = take_json(out, *free) {
                if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
                    tracing::warn!(
                        "plugin {plugin} {action}: {}",
                        v.get("error").and_then(|e| e.as_str()).unwrap_or("error")
                    );
                }
            }
        }
    })
    .await;
}

/// Serve a plugin-provided page (its own overlay / browser dock) at
/// `/plugin/<name>/<path>`. Returns `(content_type, body)` or None if the plugin
/// has no `plugin_page` symbol, isn't found, or declines. The page is served
/// same-origin with the panel, so its JS can read `/events`, `/ws`, `/api/*`.
pub async fn serve_page(name: &str, path: &str) -> Option<(String, Vec<u8>)> {
    let config = read_config(name);
    let ctx = serde_json::json!({ "path": path, "config": config });
    let name = name.to_string();
    tokio::task::spawn_blocking(move || unsafe {
        let p = registry().iter().find(|p| p.name == name)?;
        // plugin_page is optional (added without an ABI bump); absent = no pages.
        let page = p.lib.get::<CallFn>(b"plugin_page\0").ok()?;
        let free = p.lib.get::<FreeFn>(b"plugin_free\0").ok()?;
        let input = CString::new(ctx.to_string()).ok()?;
        let out = page(input.as_ptr());
        let v = take_json(out, *free)?;
        if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
            return None;
        }
        let body = v.get("body").and_then(|b| b.as_str())?.to_string();
        let ct = v.get("content_type").and_then(|c| c.as_str())
            .unwrap_or("text/html; charset=utf-8").to_string();
        Some((ct, body.into_bytes()))
    })
    .await
    .ok()
    .flatten()
}

/// Current health for a plugin's badge (`{enabled,reachable}`), or all-false.
pub async fn status(name: &str) -> StatusResult {
    let config = read_config(name);
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        let Some(p) = registry().iter().find(|p| p.name == name) else {
            return StatusResult::default();
        };
        unsafe {
            let (Ok(f), Ok(free)) = (
                p.lib.get::<CallFn>(b"plugin_status\0"),
                p.lib.get::<FreeFn>(b"plugin_free\0"),
            ) else {
                return StatusResult::default();
            };
            let Ok(input) = CString::new(config.to_string()) else { return StatusResult::default() };
            let out = f(input.as_ptr());
            take_json(out, *free)
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        }
    })
    .await
    .unwrap_or_default()
}
