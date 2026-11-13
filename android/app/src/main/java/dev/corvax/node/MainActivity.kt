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
import android.webkit.WebView

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

        setContentView(WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            postDelayed({ loadUrl("http://127.0.0.1:4799/") }, 800)                // the node needs a moment on first start
        })
    }
}
