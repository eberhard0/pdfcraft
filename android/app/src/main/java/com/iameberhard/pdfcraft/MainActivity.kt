package com.iameberhard.pdfcraft

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.util.Log
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import java.io.File

/**
 * The Android shell around the Rust app (`apps/pdfcraft-android`, loaded as
 * `libpdfcraft_android.so` by GameActivity's glue).
 *
 * The app reads and writes paths in this app's private storage. This activity bridges them to
 * the Storage Access Framework: [pick] shows the system pickers and answers with staged paths
 * through [nativePickResult]; [wroteFile] copies a file the app wrote back to the document or
 * folder the user picked; PDFs opened or shared from other apps go in through [nativeDeliverFile].
 */
class MainActivity : GameActivity() {

    companion object {
        private const val TAG = "pdfcraft"
        private const val KIND_FILE = 0
        private const val KIND_FILES = 1
        private const val KIND_FOLDER = 2
        private const val KIND_SAVE = 3
        /** Files copied in from a picked folder, at most. */
        private const val FOLDER_FILE_CAP = 500

        init {
            System.loadLibrary("pdfcraft_android")
        }
    }

    private external fun nativePickResult(id: Long, paths: String)
    private external fun nativeDeliverFile(name: String, bytes: ByteArray)

    private data class Pending(val id: Long, val kind: Int, val fileName: String)

    private var pending: Pending? = null

    /** Staged file path → the document it stands for (opened files, created documents). */
    private val fileUris = HashMap<String, Uri>()
    /** Staged folder path → the folder (document tree) it stands for. */
    private val folderUris = HashMap<String, Uri>()

    private val launcher = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val p = pending ?: return@registerForActivityResult
        pending = null
        Thread {
            val answer = try {
                if (result.resultCode == RESULT_OK && result.data != null) handleResult(p, result.data!!) else ""
            } catch (e: Exception) {
                Log.e(TAG, "pick ${p.id} failed", e)
                toast("Couldn't use the chosen file: ${e.message}")
                ""
            }
            nativePickResult(p.id, answer)
        }.start()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        hideSystemBars()
        deliverIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        deliverIntent(intent)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    /** Full screen for the page; a swipe from an edge shows the bars briefly. */
    private fun hideSystemBars() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    /** A PDF opened (VIEW) or shared (SEND) from another app: hand its bytes to the app. */
    private fun deliverIntent(intent: Intent?) {
        if (intent == null) return
        @Suppress("DEPRECATION")
        val shared: Uri? = intent.getParcelableExtra(Intent.EXTRA_STREAM)
        val uri: Uri = when (intent.action) {
            Intent.ACTION_VIEW -> intent.data
            Intent.ACTION_SEND -> shared
            else -> null
        } ?: return
        Thread {
            try {
                val name = displayName(uri) ?: "document.pdf"
                val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() }
                if (bytes == null) toast("Couldn't read $name") else nativeDeliverFile(name, bytes)
            } catch (e: Exception) {
                Log.e(TAG, "open failed", e)
                toast("Couldn't open the file: ${e.message}")
            }
        }.start()
    }

    // ---- Called from Rust (any thread) --------------------------------------------------------

    /**
     * Show a system picker: [kind] is file, files, folder or save; [extensions] a comma-separated
     * list (empty = any); [fileName] the suggested name for save. The answer goes to
     * [nativePickResult] with [id].
     */
    fun pick(kind: Int, extensions: String, fileName: String, title: String, id: Long) {
        runOnUiThread {
            try {
                if (pending != null) {
                    nativePickResult(id, "")
                    return@runOnUiThread
                }
                val exts = extensions.split(',').map { it.trim().lowercase() }.filter { it.isNotEmpty() }
                val intent = when (kind) {
                    KIND_FOLDER -> Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                    KIND_SAVE -> Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        type = mimeFor(fileName.substringAfterLast('.', ""))
                        putExtra(Intent.EXTRA_TITLE, fileName.ifEmpty { "document.pdf" })
                    }
                    else -> Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        // All files: PDF and PhotoCraft-style documents have no reliable MIME type
                        // in every file provider, and the app checks the bytes anyway.
                        type = "*/*"
                        val mimes = exts.map { mimeFor(it) }.distinct()
                        if (mimes.isNotEmpty() && mimes.none { it == "application/octet-stream" }) {
                            putExtra(Intent.EXTRA_MIME_TYPES, mimes.toTypedArray())
                        }
                        if (kind == KIND_FILES) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                    }
                }
                pending = Pending(id, kind, fileName)
                launcher.launch(intent)
            } catch (e: Exception) {
                Log.e(TAG, "picker failed", e)
                pending = null
                toast("Couldn't open the file picker: ${e.message}")
                nativePickResult(id, "")
            }
        }
    }

    /**
     * The app wrote [path]. If it stands for a picked document, copy the bytes to that document;
     * if it is inside a picked folder, create or overwrite the file there. Returns null on success
     * (or when the path is not a picked document), else a message.
     */
    fun wroteFile(path: String): String? {
        return try {
            val file = File(path)
            fileUris[path]?.let { uri ->
                writeTo(uri, file)
                toast("Saved ${file.name}")
                return null
            }
            val folder = folderUris.entries.firstOrNull { path.startsWith(it.key + "/") } ?: return null
            val tree = folder.value
            val existing = findChild(tree, file.name)
            val target = existing ?: DocumentsContract.createDocument(
                contentResolver,
                DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree)),
                mimeFor(file.extension),
                file.name,
            ) ?: return "couldn't create ${file.name} in the chosen folder"
            writeTo(target, file)
            null
        } catch (e: Exception) {
            Log.e(TAG, "write-back failed for $path", e)
            e.message ?: e.toString()
        }
    }

    // ---- Picker results --------------------------------------------------------------------------

    /** Stage the picked documents and answer with their paths, one per line. */
    private fun handleResult(p: Pending, data: Intent): String {
        return when (p.kind) {
            KIND_FILE, KIND_FILES -> {
                val uris = ArrayList<Uri>()
                data.clipData?.let { clip -> for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let(uris::add) }
                if (uris.isEmpty()) data.data?.let(uris::add)
                val dir = File(cacheDir, "picks/${p.id}").apply { mkdirs() }
                uris.mapNotNull { uri -> stageDocument(uri, dir, data.flags) }.joinToString("\n")
            }
            KIND_FOLDER -> {
                val tree = data.data ?: return ""
                try {
                    contentResolver.takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                } catch (_: SecurityException) {
                }
                val dir = File(filesDir, "folders/${p.id}").apply { mkdirs() }
                var copied = 0
                copyTree(tree, DocumentsContract.getTreeDocumentId(tree), dir) { copied++; copied <= FOLDER_FILE_CAP }
                folderUris[dir.path] = tree
                dir.path
            }
            KIND_SAVE -> {
                val uri = data.data ?: return ""
                val name = displayName(uri) ?: p.fileName.ifEmpty { "document.pdf" }
                val dir = File(filesDir, "saves/${p.id}").apply { mkdirs() }
                val staged = File(dir, name)
                fileUris[staged.path] = uri
                staged.path
            }
            else -> ""
        }
    }

    /** Copy [uri] into [dir] under its display name and remember where it came from. */
    private fun stageDocument(uri: Uri, dir: File, flags: Int): String? {
        val name = displayName(uri) ?: "document.pdf"
        val staged = File(dir, name)
        contentResolver.openInputStream(uri)?.use { input -> staged.outputStream().use { input.copyTo(it) } } ?: return null
        try {
            val keep = flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            if (keep != 0) contentResolver.takePersistableUriPermission(uri, keep)
        } catch (_: SecurityException) {
        }
        fileUris[staged.path] = uri
        return staged.path
    }

    /** Copy the documents of a tree into [dir], recursing into sub-folders, while [more] allows. */
    private fun copyTree(tree: Uri, documentId: String, dir: File, more: () -> Boolean) {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, documentId)
        val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE)
        contentResolver.query(children, cols, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                val id = c.getString(0)
                val name = c.getString(1) ?: continue
                val mime = c.getString(2) ?: ""
                if (mime == DocumentsContract.Document.MIME_TYPE_DIR) {
                    val sub = File(dir, name).apply { mkdirs() }
                    copyTree(tree, id, sub, more)
                } else {
                    if (!more()) return
                    val uri = DocumentsContract.buildDocumentUriUsingTree(tree, id)
                    try {
                        contentResolver.openInputStream(uri)?.use { input -> File(dir, name).outputStream().use { input.copyTo(it) } }
                    } catch (e: Exception) {
                        Log.w(TAG, "skipped $name: ${e.message}")
                    }
                }
            }
        }
    }

    /** The document named [name] directly inside the tree, if any. */
    private fun findChild(tree: Uri, name: String): Uri? {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
        val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME)
        contentResolver.query(children, cols, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                if (c.getString(1) == name) return DocumentsContract.buildDocumentUriUsingTree(tree, c.getString(0))
            }
        }
        return null
    }

    private fun writeTo(uri: Uri, file: File) {
        val out = contentResolver.openOutputStream(uri, "wt") ?: throw IllegalStateException("couldn't open the document for writing")
        out.use { o -> file.inputStream().use { it.copyTo(o) } }
    }

    // ---- Helpers ------------------------------------------------------------------------------

    private fun displayName(uri: Uri): String? {
        try {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    val i = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (i >= 0) c.getString(i)?.let { return it }
                }
            }
        } catch (_: Exception) {
        }
        return uri.lastPathSegment?.substringAfterLast('/')
    }

    private fun mimeFor(ext: String): String = when (ext.lowercase().trimStart('.')) {
        "pdf" -> "application/pdf"
        "png" -> "image/png"
        "jpg", "jpeg" -> "image/jpeg"
        "tif", "tiff" -> "image/tiff"
        "webp" -> "image/webp"
        "gif" -> "image/gif"
        "bmp" -> "image/bmp"
        "svg" -> "image/svg+xml"
        "txt" -> "text/plain"
        "csv" -> "text/csv"
        "html", "htm" -> "text/html"
        "json" -> "application/json"
        "xml" -> "application/xml"
        "fdf", "xfdf" -> "application/vnd.adobe.xfdf"
        "p12", "pfx" -> "application/x-pkcs12"
        "cer", "crt", "pem" -> "application/x-x509-ca-cert"
        else -> "application/octet-stream"
    }

    private fun toast(text: String) {
        runOnUiThread { Toast.makeText(this, text, Toast.LENGTH_SHORT).show() }
    }
}
