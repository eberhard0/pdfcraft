//! PdfCraft on Android.
//!
//! Runs the same [`pdfcraft_ui_egui::PdfCraftApp`] as the desktop app inside a `GameActivity`
//! (android-activity's `game-activity` backend, which eframe needs for the soft keyboard and
//! accesskit). Built with `cargo ndk` into `android/app/src/main/jniLibs`, then packaged by the
//! Gradle project in `android/`.
//!
//! Files: the app keeps reading and writing paths as on the desktop, inside its private storage.
//! `MainActivity` (Kotlin) shows the Storage Access Framework pickers and bridges the two:
//! - a picked document is copied into the app's storage and that path is answered; a file the
//!   user chose to save gets an empty place there;
//! - every file the app writes (`write_atomically`) is reported through [`wrote_file`], and the
//!   activity copies it back to the document the user picked (the opened file for Save, the
//!   created one for Save As, the chosen folder for batch output);
//! - PDFs shared or opened from other apps arrive as bytes through the app's inbox.

#![cfg(target_os = "android")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock, PoisonError};

use android_activity::AndroidApp;
use jni::objects::{JByteArray, JObject, JString};
use jni::sys::jlong;
use jni::{Env, EnvUnowned, JValue, JavaVM, jni_sig, jni_str};
use pdfcraft_ui_egui::android::{PickKind, PickRequest};
use pdfcraft_ui_egui::{Inbox, PdfCraftApp, RecoveryStore};

const LOG_TAG: &str = "pdfcraft";
/// Settings are saved under this key (same as the desktop app, so `restore` reads them).
const STORAGE_KEY: &str = "pdfcraft";

/// Files other apps hand us (name, bytes); the app opens them on its next frame.
static INBOX: OnceLock<Inbox> = OnceLock::new();
/// The egui context, to wake the app when an answer arrives from a Java thread.
static CTX: OnceLock<egui::Context> = OnceLock::new();
/// The process's Java VM (set once) and the current activity (a reference android-activity
/// owns, stored as an address; 0 = none).
static VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: Mutex<usize> = Mutex::new(0);
/// Pickers waiting for the activity's answer, by request id.
static PENDING: OnceLock<Mutex<HashMap<u64, Sender<Vec<PathBuf>>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn inbox() -> &'static Inbox {
    INBOX.get_or_init(Inbox::default)
}

fn pending() -> &'static Mutex<HashMap<u64, Sender<Vec<PathBuf>>>> {
    PENDING.get_or_init(Default::default)
}

/// The activity's entry point, called by android-activity's GameActivity glue on its own thread.
/// It returns when the activity is destroyed.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag(LOG_TAG));
        pdfcraft_ui_egui::android::set_picker(Box::new(start_pick));
        pdfcraft_ui_egui::android::set_wrote(Box::new(wrote_file));
    });
    // SAFETY: `vm_as_ptr` is the process's JavaVM, valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let _ = VM.set(vm);
    *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner) = app.activity_as_ptr() as usize;

    let data_dir = app.internal_data_path().unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    log::info!("PdfCraft {} starting; data in {}", env!("CARGO_PKG_VERSION"), data_dir.display());
    let options = eframe::NativeOptions {
        android_app: Some(app),
        // eframe saves the app's settings (`PdfCraftApp::save`) here on exit.
        persistence_path: Some(data_dir.join("app.ron")),
        ..Default::default()
    };
    let result = eframe::run_native(
        "PdfCraft",
        options,
        Box::new(move |cc| {
            let _ = CTX.set(cc.egui_ctx.clone());
            let mut app = PdfCraftApp::new();
            if let Some(json) = cc.storage.and_then(|s| s.get_string(STORAGE_KEY)) {
                app.restore(&json);
            }
            app.inbox = inbox().clone();
            app.os_key_store_ids = false;
            // Autosave unsaved changes; offer to recover documents a crashed session left behind.
            app.enable_recovery(RecoveryStore::new(data_dir.join("Recovery")));
            Ok(Box::new(app))
        }),
    );
    *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner) = 0;
    if let Err(e) = result {
        log::error!("PdfCraft stopped: {e}");
        // winit allows one event loop per process: when Android recreates the activity in the
        // same process, end the process so the next launch starts clean instead of a blank window.
        std::process::exit(0);
    }
}

// ---- Calls into MainActivity (Kotlin) ----------------------------------------------------------

/// Run `f` with a JNI environment on this thread and the current activity.
fn with_activity<T>(f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>) -> Result<T, String> {
    let vm = VM.get().ok_or("the Java VM is not available")?;
    let raw = *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner);
    if raw == 0 {
        return Err("the activity is not running".to_string());
    }
    let raw = raw as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<T> {
        // SAFETY: the reference comes from android-activity's `activity_as_ptr`, which keeps it
        // valid while the activity runs (ACTIVITY is cleared when `run_native` returns). `Cast`
        // neither owns nor deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject>(&raw)? };
        f(env, &activity)
    })
    .map_err(|e| e.to_string())
}

/// `MainActivity.pick(kind, extensions, fileName, title, id)`: show a system picker. The answer
/// comes back through `nativePickResult` on the channel returned here.
fn start_pick(request: PickRequest) -> Option<Receiver<Vec<PathBuf>>> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    pending().lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
    let kind: i32 = match request.kind {
        PickKind::File => 0,
        PickKind::Files => 1,
        PickKind::Folder => 2,
        PickKind::Save => 3,
    };
    let extensions = request.extensions.join(",");
    let file_name = request.file_name.unwrap_or_default();
    let title = request.title.unwrap_or_default();
    let started = with_activity(|env, activity| {
        let jexts = JString::from_str(env, &extensions)?;
        let jname = JString::from_str(env, &file_name)?;
        let jtitle = JString::from_str(env, &title)?;
        env.call_method(
            activity,
            jni_str!("pick"),
            jni_sig!("(ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;J)V"),
            &[JValue::Int(kind), (&jexts).into(), (&jname).into(), (&jtitle).into(), JValue::Long(id as jlong)],
        )?;
        Ok(())
    });
    match started {
        Ok(()) => Some(rx),
        Err(e) => {
            log::error!("couldn't show the file picker: {e}");
            pending().lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            None
        }
    }
}

/// `MainActivity.wroteFile(path)`: copy a written file to the document the user picked. `null`
/// on success (or when the path is not a picked document), else the error message.
fn wrote_file(path: &Path) -> Result<(), String> {
    let path = path.to_string_lossy().into_owned();
    with_activity(|env, activity| {
        let jpath = JString::from_str(env, &path)?;
        let ret = env.call_method(activity, jni_str!("wroteFile"), jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"), &[(&jpath).into()])?.l()?;
        if ret.is_null() {
            return Ok(Ok(()));
        }
        let message = env.cast_local::<JString>(ret)?;
        Ok(Err(message.to_string()))
    })?
}

// ---- Calls from MainActivity (Kotlin) ----------------------------------------------------------

/// `MainActivity.nativePickResult(id, paths)`: the picker's answer, one path per line (empty =
/// cancelled), from a Java thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_iameberhard_pdfcraft_MainActivity_nativePickResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    id: jlong,
    paths: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> jni::errors::Result<()> {
        let paths: Vec<PathBuf> = paths.to_string().lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect();
        log::info!("pick {id}: {} path(s)", paths.len());
        let tx = pending().lock().unwrap_or_else(PoisonError::into_inner).remove(&(id as u64));
        if let Some(tx) = tx {
            let _ = tx.send(paths);
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        Ok(())
    });
    outcome.resolve::<jni::errors::LogErrorAndDefault>()
}

/// `MainActivity.nativeDeliverFile(name, bytes)`: a PDF opened or shared from another app.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_iameberhard_pdfcraft_MainActivity_nativeDeliverFile<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    name: JString<'caller>,
    bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> jni::errors::Result<()> {
        let name = name.to_string();
        let bytes = env.convert_byte_array(&bytes)?;
        log::info!("received {name} ({} bytes)", bytes.len());
        inbox().lock().unwrap_or_else(PoisonError::into_inner).push((name, bytes));
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        Ok(())
    });
    outcome.resolve::<jni::errors::LogErrorAndDefault>()
}
