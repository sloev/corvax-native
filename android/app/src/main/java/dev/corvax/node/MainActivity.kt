package dev.corvax.node

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import java.net.InetSocketAddress
import java.net.Socket
import kotlin.concurrent.thread

/**
 * The UI is the node's own homepage (DOX / APP / STATS) at http://127.0.0.1:4799/, the same page every CORVAX
 * node serves. First run: ask for notifications and nearby devices, offer to skip battery optimisation, start the node.
 */
class MainActivity : Activity() {
    @SuppressLint("SetJavaScriptEnabled", "BatteryLife")
    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        val wanted = buildList {
            if (Build.VERSION.SDK_INT >= 33) { add(Manifest.permission.POST_NOTIFICATIONS); add(Manifest.permission.NEARBY_WIFI_DEVICES) }
            if (Build.VERSION.SDK_INT >= 31) { add(Manifest.permission.BLUETOOTH_SCAN); add(Manifest.permission.BLUETOOTH_ADVERTISE); add(Manifest.permission.BLUETOOTH_CONNECT) }
            else add(Manifest.permission.ACCESS_FINE_LOCATION)                     // BLE scanning before Android 12
        }
        requestPermissions(wanted.toTypedArray(), 1)
        val power = getSystemService(PowerManager::class.java)
        if (!power.isIgnoringBatteryOptimizations(packageName))
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        getSharedPreferences("corvax", MODE_PRIVATE).edit().putBoolean("enabled", true).apply()
        NodeService.start(this)

        // Show "Starting…" until the node answers on 127.0.0.1:4799 (or reports why it can't), then the app.
        val web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            webViewClient = object : WebViewClient() {
                override fun onReceivedError(v: WebView, r: WebResourceRequest, e: WebResourceError) { if (r.isForMainFrame) v.postDelayed({ v.reload() }, 1500) }
            }
        }
        setContentView(web)
        status(web, "Starting your CORVAX node…")
        val prefs = getSharedPreferences("corvax", MODE_PRIVATE)
        thread(name = "corvax-wait") {
            for (i in 0 until 120) {                                               // up to 60 s
                if (up()) { runOnUiThread { web.loadUrl("http://127.0.0.1:4799/") }; return@thread }
                prefs.getString("error", null)?.let { e -> runOnUiThread { status(web, "CORVAX couldn't start", e) }; return@thread }
                Thread.sleep(500)
            }
            runOnUiThread { status(web, "Still starting…", "The node hasn't answered after a minute. Check the CORVAX notification.") }
        }
    }

    private fun up() = runCatching { Socket().use { it.connect(InetSocketAddress("127.0.0.1", 4799), 300) }; true }.getOrDefault(false)
    private fun status(web: WebView, title: String, detail: String = "") {
        val esc = { s: String -> s.replace("&", "&amp;").replace("<", "&lt;") }
        web.loadDataWithBaseURL(null, "<meta name=viewport content='width=device-width'><body style='font:16px system-ui;padding:24px;color:#17181c;background:#fcfbf8'>" +
            "<h3>${esc(title)}</h3><p style='color:#6b6e78'>${esc(detail)}</p></body>", "text/html", "utf-8", null)
    }
}
