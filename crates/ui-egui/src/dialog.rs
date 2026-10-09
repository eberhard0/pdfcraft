//! The file dialog builder every picker uses ([`crate::pickers`]).
//!
//! Desktop and browser builds use rfd's `AsyncFileDialog`. rfd has no Android backend, so there
//! the same builder API records the request (filters, file name, title) for the shell's picker
//! ([`crate::android`]), and nothing else in this crate needs to know the difference.

#[cfg(not(target_os = "android"))]
pub use rfd::AsyncFileDialog;

/// A dialog request for the Android shell: the subset of rfd's builder the app uses.
#[cfg(target_os = "android")]
#[derive(Clone, Debug, Default)]
pub struct AsyncFileDialog {
    filters: Vec<(String, Vec<String>)>,
    file_name: Option<String>,
    title: Option<String>,
}

#[cfg(target_os = "android")]
impl AsyncFileDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        self.filters.push((name.into(), extensions.iter().map(|e| e.to_string()).collect()));
        self
    }

    /// Android's picker chooses its own starting place.
    pub fn set_directory<P: AsRef<std::path::Path>>(self, _path: P) -> Self {
        self
    }

    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn set_can_create_directories(self, _can: bool) -> Self {
        self
    }

    /// The request the shell's picker gets ([`crate::android::PickRequest`]).
    pub(crate) fn request(self, kind: crate::android::PickKind) -> crate::android::PickRequest {
        let extensions = self.filters.into_iter().flat_map(|(_, exts)| exts).map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
        crate::android::PickRequest { kind, extensions, file_name: self.file_name, title: self.title }
    }
}
