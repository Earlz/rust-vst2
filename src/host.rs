//! Host specific structures.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::error::Error;
use std::{fmt, ptr, mem};

use dylib::DynamicLibrary;
use libc::c_void;

use interfaces;
use plugin::{self, Plugin, Info, Category};
use api::{AEffect, PluginMain};
use api::consts::*;

#[repr(usize)]
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub enum OpCode {
    /// [index]: parameter index
    /// [opt]: parameter value
    Automate = 0,
    /// [return]: host vst version (e.g. 2400 for VST 2.4)
    Version,
    /// [return]: current plugin ID (useful for shell plugins to figure out which plugin to load in
    ///           `VSTPluginMain()`).
    CurrentId,
    /// No arguments. Give idle time to Host application, e.g. if plug-in editor is doing mouse
    /// tracking in a modal loop.
    Idle,
    /// Deprecated.
    _PinConnected = 4,

    /// Deprecated.
    _WantMidi = 6, // Not a typo
    /// [value]: request mask. see `VstTimeInfoFlags`
    /// [return]: `VstTimeInfo` pointer or null if not supported.
    GetTime,
    /// Deprecated.
    _SetTime,
    /// Deprecated.
    _TempoAt,
    /// Deprecated.
    _GetNumAutomatableParameters,
    /// Deprecated.
    _GetParameterQuantization,

    /// Notifies the host that the input/output setup has changed. This can allow the host to check
    /// numInputs/numOutputs or call `getSpeakerArrangement()`
    /// [return]: 1 if supported.
    IOChanged,

    /// Deprecated.
    _NeedIdle,
}
impl_clike!(OpCode);

/// Implemented by all VST hosts.
#[allow(unused_variables)]
pub trait Host {
    /// Automate a parameter; the value has been changed.
    fn automate(&mut self, index: i32, value: f32) {}

    /// Get the plugin ID of the currently loading plugin.
    ///
    /// This is only useful for shell plugins where this value will change the plugin returned.
    /// `TODO: implement shell plugins`
    fn get_plugin_id(&self) -> i32 {
        // TODO: Handle this properly
        0
    }

    /// An idle call.
    ///
    /// This is useful when the plugin is doing something such as mouse tracking in the UI.
    fn idle(&self) {}
}

/// All possible errors that can occur when loading a VST plugin.
#[derive(Debug)]
pub enum PluginLoadError {
    /// Could not load given path.
    InvalidPath,

    /// Given path is not a VST plugin.
    NotAPlugin,

    /// Failed to create an instance of this plugin.
    ///
    /// This can happen for many reasons, such as if the plugin requires a different version of
    /// the VST API to be used, or due to improper licensing.
    InstanceFailed,
}

impl fmt::Display for PluginLoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl Error for PluginLoadError {
    fn description(&self) -> &str {
        use self::PluginLoadError::*;

        match *self {
            InvalidPath => "Could not open the requested path",
            NotAPlugin => "The given path does not contain a VST2.4 compatible library",
            InstanceFailed => "Failed to create a plugin instance",
        }
    }
}

/// Wrapper for an externally loaded VST plugin.
///
/// The only functionality this struct provides is loading plugins, which can be done via the
/// [`load`](#method.load) method.
pub struct PluginLoader<T: Host> {
    main: PluginMain,
    lib: Arc<DynamicLibrary>,
    host: Arc<Mutex<T>>,
}

/// An instance of an externally loaded VST plugin.
#[allow(dead_code)] // To keep `lib` around.
pub struct PluginInstance {
    effect: *mut AEffect,
    lib: Arc<DynamicLibrary>,
    info: Info,
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        self.dispatch(plugin::OpCode::Shutdown, 0, 0, ptr::null_mut(), 0.0);
    }
}


impl<T: Host> PluginLoader<T> {
    /// Load a plugin at the given path with the given host.
    ///
    /// Because of the possibility of multi-threading problems that can occur when using plugins,
    /// the host must be passed in via an `Arc<Mutex<T>>` object. This makes sure that even if the
    /// plugins are multi-threaded no data race issues can occur.
    ///
    /// Upon success, this method returns a [`PluginLoader`](.) object which you can use to call
    /// [`instance`](#method.instance) to create a new instance of the plugin.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::path::Path;
    /// # use std::sync::{Arc, Mutex};
    /// # use vst2::host::{Host, PluginLoader};
    /// # let path = Path::new(".");
    /// # struct MyHost;
    /// # impl MyHost { fn new() -> MyHost { MyHost } }
    /// # impl Host for MyHost {
    /// #     fn automate(&mut self, _: i32, _: f32) {}
    /// #     fn get_plugin_id(&self) -> i32 { 0 }
    /// # }
    /// // ...
    /// let host = Arc::new(Mutex::new(MyHost::new()));
    ///
    /// let mut plugin = PluginLoader::load(path, host.clone()).unwrap();
    ///
    /// let instance = plugin.instance().unwrap();
    /// // ...
    /// ```
    ///
    /// # Linux/Windows
    ///   * This should be a path to the library, typically ending in `.so`/`.dll`.
    ///   * Possible full path: `/home/overdrivenpotato/.vst/u-he/Zebra2.64.so`
    ///   * Possible full path: `C:\Program Files (x86)\VSTPlugins\iZotope Ozone 5.dll`
    ///
    /// # OS X
    ///   * This should point to the mach-o file within the `.vst` bundle.
    ///   * Plugin: `/Library/Audio/Plug-Ins/VST/iZotope Ozone 5.vst`
    ///   * Possible full path:
    ///     `/Library/Audio/Plug-Ins/VST/iZotope Ozone 5.vst/Contents/MacOS/PluginHooksVST`
    pub fn load(path: &Path, host: Arc<Mutex<T>>) -> Result<PluginLoader<T>, PluginLoadError> {
        // Try loading the library at the given path
        let lib = match DynamicLibrary::open(Some(&path)) {
            Ok(l) => l,
            Err(_) => return Err(PluginLoadError::InvalidPath)
        };

        Ok(PluginLoader {
            main: unsafe {
                      // Search the library for the VSTAPI entry point
                      match lib.symbol("VSTPluginMain") {
                          // Use `Fn(...)` instead of `*mut Fn(...)`.
                          Ok(s) => mem::transmute::<*mut PluginMain, PluginMain>(s),
                          _ => return Err(PluginLoadError::NotAPlugin)
                      }
                  },
            lib: Arc::new(lib),
            host: host,
        })
    }

    /// Call the VST entry point and retrieve a (possibly null) pointer.
    unsafe fn call_main(&mut self) -> *mut AEffect {
        load_pointer = mem::transmute(Box::new(self.host.clone()));
        (self.main)(callback_wrapper::<T>)
    }

    /// Try to create an instance of this VST plugin.
    ///
    /// If the instance is successfully created, a [`PluginInstance`](struct.PluginInstance.html)
    /// is returned. This struct implements the [`Plugin` trait](../plugin/trait.Plugin.html).
    pub fn instance(&mut self) -> Result<PluginInstance, PluginLoadError> {
        // Call the plugin main function. This also passes the plugin main function as the closure
        // could not return an error if the symbol wasn't found
        let effect = unsafe { self.call_main() };

        if effect.is_null() {
            return Err(PluginLoadError::InstanceFailed);
        }

        unsafe {
            // Move the host to the heap and add it to the `AEffect` struct for future reference
            (*effect).reserved1 = mem::transmute(Box::new(self.host.clone()));
        }

        Ok(PluginInstance::new(
            effect,
            self.lib.clone()
        ))
    }
}

impl PluginInstance {
    fn new(effect: *mut AEffect, lib: Arc<DynamicLibrary>) -> PluginInstance {
        use plugin::OpCode as op;

        let mut plug = PluginInstance {
            effect: effect,
            lib: lib,
            info: Default::default()
        };

        unsafe {
            use api::flags::*;

            let effect: &mut AEffect = mem::transmute(effect);
            let flags = Plugin::from_bits_truncate(effect.flags);

            plug.info = Info {
                name: plug.read_string(op::GetProductName, MAX_PRODUCT_STR_LEN as u64),
                vendor: plug.read_string(op::GetVendorName, MAX_VENDOR_STR_LEN as u64),

                presets: effect.numPrograms,
                parameters: effect.numParams,
                inputs: effect.numInputs,
                outputs: effect.numOutputs,

                unique_id: effect.uniqueId,
                version: effect.version,

                category: Category::from(plug.opcode(op::GetCategory)),

                initial_delay: effect.initialDelay,

                preset_chunks: flags.intersects(PROGRAM_CHUNKS),
                f64_precision: flags.intersects(CAN_DOUBLE_REPLACING),
                silent_when_stopped: flags.intersects(NO_SOUND_IN_STOP),
            };
        }

        plug
    }

    /// Send a dispatch message to the plugin.
    fn dispatch(&mut self,
                opcode: plugin::OpCode,
                index: i32,
                value: isize,
                ptr: *mut c_void,
                opt: f32)
                -> isize {
        let dispatcher = unsafe {
            (*self.effect).dispatcher
        };
        if (dispatcher as *mut u8).is_null() {
            panic!("Plugin was not loaded correctly.");
        }
        dispatcher(self.effect, opcode.into(), index, value, ptr, opt)
    }

    fn read_string(&mut self, opcode: plugin::OpCode, max: u64) -> String {
        let mut buf = vec![0; max as usize];
        self.dispatch(opcode, 0, 0, unsafe { mem::transmute(buf.as_mut_ptr()) }, 0.0);
        String::from_utf8_lossy(&buf).chars().take_while(|c| *c != '\0').collect()
    }

    fn opcode(&mut self, opcode: plugin::OpCode) -> isize {
        self.dispatch(opcode, 0, 0, ptr::null_mut(), 0.0)
    }
}

impl Plugin for PluginInstance {
    fn init(&mut self) {
        self.opcode(plugin::OpCode::Initialize);
    }

    fn get_info(&self) -> plugin::Info {
        self.info.clone()
    }
}

/// HACK: a pointer to store the host so that it can be accessed from the `callback_wrapper`
/// function passed to the plugin.
///
/// When the plugin is being loaded, a `Box<Arc<Mutex<T>>>` is transmuted to a *mut c_void pointer
/// and placed here. When the plugin calls the callback during initialization, the host refers to
/// this pointer to get a handle to the Host. After initialization, this pointer is invalidated and
/// the host pointer is placed into a [reserved field] in the instance `AEffect` struct.
///
/// The issue with this approach is that if 2 plugins are simultaneously loaded with 2 different
/// host instances, this might fail as one host may receive a pointer to the other one. In practice
/// this is a rare situation as you normally won't have 2 seperate host instances loading at once.
///
/// [reserved field]: ../api/struct.AEffect.html#structfield.reserved1
static mut load_pointer: *mut c_void = 0 as *mut c_void;

/// Function passed to plugin to handle dispatching host opcodes.
fn callback_wrapper<T: Host>(effect: *mut AEffect, opcode: i32, index: i32,
                             value: isize, ptr: *mut c_void, opt: f32) -> isize {
    unsafe {
        // Convert `*mut` to `&mut` for easier usage
        let effect_ref: &mut AEffect = mem::transmute(effect);

        // If the effect pointer is not null and the host pointer is not null, the plugin has
        // already been initialized
        if !effect.is_null() && effect_ref.reserved1 != 0 {
            let host: &mut Arc<Mutex<T>> = mem::transmute(effect_ref.reserved1);

            let host = &mut *host.lock().unwrap();

            interfaces::host_dispatch(host, effect, opcode, index, value, ptr, opt)
        // In this case, the plugin is still undergoing initialization and so `load_pointer` is
        // dereferenced
        } else {
            // Used only during the plugin initialization
            let host: &mut Arc<Mutex<T>> = mem::transmute(load_pointer);
            let host = &mut *host.lock().unwrap();

            interfaces::host_dispatch(host, effect, opcode, index, value, ptr, opt)
        }
    }
}
