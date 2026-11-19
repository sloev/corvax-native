package dev.corvax.node

import android.content.Context
import android.util.Log
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** The app's log: every step of the node, the hidden WebView's console, failed native calls. Shared from the app screen. */
object Trace {
    private var file: File? = null
    private val time = SimpleDateFormat("HH:mm:ss.SSS", Locale.ROOT)
    fun init(c: Context) { if (file == null) file = File(c.filesDir, "corvax.log") }
    @Synchronized fun log(msg: String) {
        Log.i("CORVAX", msg)
        val f = file ?: return
        runCatching {
            if (f.length() > 256_000) f.writeText(f.readLines().takeLast(1000).joinToString("\n", postfix = "\n"))   // keep it small
            f.appendText("${time.format(Date())} $msg\n")
        }
    }
    fun text(): String = file?.takeIf { it.exists() }?.readText() ?: ""
    fun tail(n: Int): String = text().lines().takeLast(n).joinToString("\n")
}
