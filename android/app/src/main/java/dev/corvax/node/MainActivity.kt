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
import android.view.View
import android.webkit.WebView
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
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

        // Show the node's log until it answers on 127.0.0.1:4799 (or says why it can't), then the app. Share log: any app.
        Trace.init(this)
        val web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            webViewClient = object : WebViewClient() {
                override fun onReceivedError(v: WebView, r: WebResourceRequest, e: WebResourceError) { if (r.isForMainFrame) v.postDelayed({ v.reload() }, 1500) }
            }
            visibility = View.GONE
        }
        val title = TextView(this).apply { textSize = 18f; setPadding(32, 32, 32, 8); text = "Starting your CORVAX node…" }
        val log = TextView(this).apply { textSize = 11f; typeface = android.graphics.Typeface.MONOSPACE; setPadding(32, 8, 32, 32); setTextIsSelectable(true) }
        val share = Button(this).apply { text = "Share log"; setOnClickListener { share() } }
        val waiting = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; addView(title); addView(share); addView(ScrollView(context).apply { addView(log) }) }
        setContentView(FrameLayout(this).apply { addView(waiting); addView(web) })
        val prefs = getSharedPreferences("corvax", MODE_PRIVATE)
        thread(name = "corvax-wait") {
            for (i in 0 until 240) {                                               // up to 2 minutes, log refreshed every 0.5 s
                runOnUiThread { log.text = Trace.tail(60) }
                if (up()) { Trace.log("app: node answered, opening the app"); runOnUiThread { waiting.visibility = View.GONE; web.visibility = View.VISIBLE; web.loadUrl("http://127.0.0.1:4799/") }; return@thread }
                prefs.getString("error", null)?.let { e -> runOnUiThread { title.text = "CORVAX couldn't start: $e" } }   // keep waiting: it may still come up
                Thread.sleep(500)
            }
            Trace.log("app: the node didn't answer within 2 minutes")
            runOnUiThread { title.text = "The node didn't start. Please share the log."; log.text = Trace.tail(60) }
        }
    }

    private fun up() = runCatching { Socket().use { it.connect(InetSocketAddress("127.0.0.1", 4799), 300) }; true }.getOrDefault(false)
    private fun share() = startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"; putExtra(Intent.EXTRA_SUBJECT, "CORVAX log"); putExtra(Intent.EXTRA_TEXT, Trace.text().takeLast(90_000))
    }, "Share CORVAX log"))
}
