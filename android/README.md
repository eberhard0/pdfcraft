# PdfCraft for Android

The Gradle project that packages `apps/pdfcraft-android` (the Rust app as a `GameActivity`
shell) into an APK/AAB. CI (`.github/workflows/android.yml`) builds it on every push to the
`android` branch and attaches signed builds to a GitHub Release on `android-v*` tags.

## How it fits together

- `apps/pdfcraft-android/src/lib.rs`: `android_main` and the JNI bridge to `MainActivity`.
- `crates/ui-egui/src/dialog.rs` and `android.rs`: the file-dialog builder every picker uses, and
  the seam the shell plugs its picker and write-back into (rfd has no Android backend).
- `app/src/main/java/.../MainActivity.kt`: the Storage Access Framework pickers. A picked document
  is copied into the app's private storage; a file the app writes there is copied back to the
  document or folder the user chose. PDFs opened or shared from other apps arrive the same way.
- `cargo ndk` drops `libpdfcraft_android.so` into `app/src/main/jniLibs/arm64-v8a/` (ignored by
  git); Gradle packages it.

## Building locally

Needs the Android SDK (platform 35, build-tools 35), an NDK, a stable Rust toolchain with the
`aarch64-linux-android` target, and `cargo-ndk`:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/<version>
cargo ndk -t arm64-v8a --platform 30 -o android/app/src/main/jniLibs build --release -p pdfcraft-android
cd android && ./gradlew assembleDebug
```

## Known limits (first version)

- RSA digital IDs can't sign on Android (the desktop uses a C crypto library); P-256 IDs work.
- No system clipboard images (Create ▸ Clipboard); paste text through the keyboard.
- Printing needs a CUPS `lp` command, which Android lacks: export to PDF instead.
- The desktop layout needs a tablet-sized screen; on a phone's cover screen it is cramped.
