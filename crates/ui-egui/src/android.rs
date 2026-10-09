//! Android: the shell (`apps/pdfcraft-android`) shows the system file picker and copies written
//! files back to the documents the user chose. This module is the seam between the two: the
//! shell registers its callbacks once at start-up, and [`crate::pickers`] and
//! `editing::write_atomically` call them.
//!
//! Paths the picker answers with are real files in the app's private storage (the shell copies
//! a picked document there, and creates an empty place for a document to be saved), so the rest
//! of the app keeps reading and writing paths as on the desktop.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::Receiver;

/// Which picker to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    /// One existing file.
    File,
    /// One or more existing files.
    Files,
    /// A folder (its files are copied in for reading; files written into it are copied out).
    Folder,
    /// Where to save a new file.
    Save,
}

/// What the app asks for (from [`crate::dialog::AsyncFileDialog`]).
#[derive(Clone, Debug)]
pub struct PickRequest {
    pub kind: PickKind,
    /// Accepted extensions, lower-case without the dot; empty = any file.
    pub extensions: Vec<String>,
    /// The suggested name for [`PickKind::Save`].
    pub file_name: Option<String>,
    pub title: Option<String>,
}

/// Shows a picker and answers on the channel with the chosen paths (empty = cancelled). `None`
/// when no picker could be shown.
pub type PickFn = Box<dyn Fn(PickRequest) -> Option<Receiver<Vec<PathBuf>>> + Send + Sync>;
/// A file was written at this path; the shell copies it to where the user wanted it.
pub type WroteFn = Box<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

static PICKER: OnceLock<PickFn> = OnceLock::new();
static WROTE: OnceLock<WroteFn> = OnceLock::new();

/// Register the shell's picker (once; later calls are ignored).
pub fn set_picker(picker: PickFn) {
    let _ = PICKER.set(picker);
}

/// Register the shell's write-back (once; later calls are ignored).
pub fn set_wrote(wrote: WroteFn) {
    let _ = WROTE.set(wrote);
}

pub(crate) fn pick(request: PickRequest) -> Option<Receiver<Vec<PathBuf>>> {
    PICKER.get().and_then(|picker| picker(request))
}

pub(crate) fn wrote(path: &Path) -> Result<(), String> {
    match WROTE.get() {
        Some(wrote) => wrote(path),
        None => Ok(()),
    }
}
