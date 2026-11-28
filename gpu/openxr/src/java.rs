//! The Java side of the session: the process's VM and application
//! `Context` (kept by `xr_android_init`), the activity the runtime is given,
//! the app's external files directory (where the debug files are), and the
//! log (logcat, tag `exact-xr`).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

// ---------------------------------------------------------------- logging

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// One line to logcat under the tag `exact-xr` (info), or warn.
pub fn log(warn: bool, line: &str) {
    let text = CString::new(line.replace('\0', " ")).unwrap_or_default();
    // SAFETY: two NUL-terminated strings.
    unsafe {
        __android_log_write(
            if warn { 5 } else { 4 },
            c"exact-xr".as_ptr(),
            text.as_ptr(),
        )
    };
}

// ---------------------------------------------------------------- Java

/// The process's Java VM and a global reference to the application
/// `Context`, which [`android_init`] keeps.
pub(crate) struct Java {
    pub(crate) vm: *mut jni_sys::JavaVM,
    context: jni_sys::jobject,
}
// SAFETY: a JavaVM pointer and a global reference are valid on every thread.
unsafe impl Send for Java {}
unsafe impl Sync for Java {}
pub(crate) static JAVA: OnceLock<Java> = OnceLock::new();

/// `xr_android_init`: keep the VM and the application's `Context` for the
/// session made later.
///
/// # Safety
/// `env` is the calling thread's live `JNIEnv*` and `context` a live
/// reference to the application `Context`.
pub(crate) unsafe fn android_init(
    vm: *mut c_void,
    env: *mut c_void,
    context: *mut c_void,
) -> Result<(), String> {
    let env = env as *mut jni_sys::JNIEnv;
    // SAFETY: the caller's contract.
    let context = unsafe {
        ((**env).NewGlobalRef.ok_or("no NewGlobalRef")?)(env, context as jni_sys::jobject)
    };
    if context.is_null() {
        return Err("no global reference to the application Context".into());
    }
    let _ = JAVA.set(Java {
        vm: vm as *mut jni_sys::JavaVM,
        context,
    });
    Ok(())
}

/// This thread attached to the VM for the guard's life (detached after, if
/// it was not attached before).
pub(crate) struct Attached {
    vm: *mut jni_sys::JavaVM,
    pub(crate) env: *mut jni_sys::JNIEnv,
    detach: bool,
}

impl Attached {
    pub(crate) fn new(vm: *mut jni_sys::JavaVM) -> Result<Self, String> {
        let mut env: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live VM.
        unsafe {
            let invoke = &**vm;
            if (invoke.GetEnv.unwrap())(vm, &mut env, jni_sys::JNI_VERSION_1_6) == jni_sys::JNI_OK {
                return Ok(Self {
                    vm,
                    env: env.cast(),
                    detach: false,
                });
            }
            if (invoke.AttachCurrentThread.unwrap())(vm, &mut env, std::ptr::null_mut())
                != jni_sys::JNI_OK
            {
                return Err("could not attach this thread to the Java VM".into());
            }
        }
        Ok(Self {
            vm,
            env: env.cast(),
            detach: true,
        })
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        if self.detach {
            // SAFETY: attached by `new` on this thread.
            unsafe { ((**self.vm).DetachCurrentThread.unwrap())(self.vm) };
        }
    }
}

/// The activity the reader registered (`dev.exact.canvas.Immersive.activity`),
/// as a global reference, else the application Context.
pub(crate) fn activity(java: &Java, env: *mut jni_sys::JNIEnv) -> jni_sys::jobject {
    // SAFETY: JNI calls on an attached thread with live references; every
    // exception is cleared before the next call.
    unsafe {
        let e = &**env;
        let clear = || {
            if (e.ExceptionCheck.unwrap())(env) != 0 {
                (e.ExceptionClear.unwrap())(env);
                true
            } else {
                false
            }
        };
        let class = (e.GetObjectClass.unwrap())(env, java.context);
        let get_loader = (e.GetMethodID.unwrap())(
            env,
            class,
            c"getClassLoader".as_ptr(),
            c"()Ljava/lang/ClassLoader;".as_ptr(),
        );
        let loader =
            (e.CallObjectMethodA.unwrap())(env, java.context, get_loader, std::ptr::null());
        if clear() || loader.is_null() {
            return java.context;
        }
        let loader_class = (e.GetObjectClass.unwrap())(env, loader);
        let load = (e.GetMethodID.unwrap())(
            env,
            loader_class,
            c"loadClass".as_ptr(),
            c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr(),
        );
        let name = (e.NewStringUTF.unwrap())(env, c"dev.exact.canvas.Immersive".as_ptr());
        let args = [jni_sys::jvalue { l: name }];
        let immersive = (e.CallObjectMethodA.unwrap())(env, loader, load, args.as_ptr());
        if clear() || immersive.is_null() {
            log(
                true,
                "no dev.exact.canvas.Immersive: the session is given the application Context",
            );
            return java.context;
        }
        let field = (e.GetStaticFieldID.unwrap())(
            env,
            immersive,
            c"activity".as_ptr(),
            c"Landroid/app/Activity;".as_ptr(),
        );
        if clear() || field.is_null() {
            return java.context;
        }
        // The activity is made before the host boots; give it a moment.
        for _ in 0..200 {
            let activity = (e.GetStaticObjectField.unwrap())(env, immersive, field);
            if !activity.is_null() {
                return (e.NewGlobalRef.unwrap())(env, activity);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        log(
            true,
            "no activity registered: the session is given the application Context",
        );
        java.context
    }
}

/// The application's external files directory
/// (`Context.getExternalFilesDir(null)`, made if missing): where `adb` reads
/// and writes a debug build's and a release build's files alike.
fn external_files_dir(java: &Java) -> Option<PathBuf> {
    let attached = Attached::new(java.vm).ok()?;
    let env = attached.env;
    // SAFETY: JNI calls on an attached thread with live references; every
    // exception is cleared before the next call.
    unsafe {
        let e = &**env;
        let failed = || {
            if (e.ExceptionCheck.unwrap())(env) != 0 {
                (e.ExceptionClear.unwrap())(env);
                true
            } else {
                false
            }
        };
        let class = (e.GetObjectClass.unwrap())(env, java.context);
        let method = (e.GetMethodID.unwrap())(
            env,
            class,
            c"getExternalFilesDir".as_ptr(),
            c"(Ljava/lang/String;)Ljava/io/File;".as_ptr(),
        );
        if failed() || method.is_null() {
            return None;
        }
        let args = [jni_sys::jvalue {
            l: std::ptr::null_mut(),
        }];
        let file = (e.CallObjectMethodA.unwrap())(env, java.context, method, args.as_ptr());
        if failed() || file.is_null() {
            return None;
        }
        let file_class = (e.GetObjectClass.unwrap())(env, file);
        let path = (e.GetMethodID.unwrap())(
            env,
            file_class,
            c"getAbsolutePath".as_ptr(),
            c"()Ljava/lang/String;".as_ptr(),
        );
        let text = (e.CallObjectMethodA.unwrap())(env, file, path, std::ptr::null());
        if failed() || text.is_null() {
            return None;
        }
        let chars = (e.GetStringUTFChars.unwrap())(env, text, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let dir = CStr::from_ptr(chars).to_string_lossy().into_owned();
        (e.ReleaseStringUTFChars.unwrap())(env, text, chars);
        Some(PathBuf::from(dir))
    }
}

/// The app's external files directory, where the debug files are.
pub(crate) fn files_dir() -> Option<PathBuf> {
    JAVA.get().and_then(external_files_dir)
}

/// The debug switches (`docs/quest.md`, "Debug switches"): the `xr-debug`
/// file's words, read once, before the device exists.
pub(crate) fn debug_switches() -> &'static str {
    static WORDS: OnceLock<String> = OnceLock::new();
    WORDS.get_or_init(|| {
        files_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("xr-debug")).ok())
            .unwrap_or_default()
    })
}

/// Whether debug switch `name` is on.
pub(crate) fn debug_switch(name: &str) -> bool {
    debug_switches().split_whitespace().any(|w| w == name)
}
