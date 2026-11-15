package dev.corvax.node

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebView

/**
 * The UI is the node's own homepage (DOX / APP / STATS) at http://127.0.0.1:4799/, the same page every CORVAX
 * node serves. Start the service only after the initial runtime-permission request has completed.
 */
class MainActivity : Activity() {
    private lateinit var ui: WebView
    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        ui = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
        }
        setContentView(ui)
        requestStartupPermissions()
    }

    private fun requestStartupPermissions() {
        val wanted = buildList {
            if (Build.VERSION.SDK_INT >= 33) {
                add(Manifest.permission.POST_NOTIFICATIONS)
                add(Manifest.permission.NEARBY_WIFI_DEVICES)
            }
            if (Build.VERSION.SDK_INT >= 31) {
                add(Manifest.permission.BLUETOOTH_SCAN)
                add(Manifest.permission.BLUETOOTH_ADVERTISE)
                add(Manifest.permission.BLUETOOTH_CONNECT)
            } else {
                add(Manifest.permission.ACCESS_FINE_LOCATION) // required for BLE scanning on Android 11 and below
            }
        }.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }

        if (wanted.isEmpty()) startNode()
        else requestPermissions(wanted.toTypedArray(), REQUEST_STARTUP_PERMISSIONS)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQUEST_STARTUP_PERMISSIONS) startNode()
    }

    @SuppressLint("BatteryLife")
    private fun startNode() {
        getSharedPreferences("corvax", MODE_PRIVATE).edit().putBoolean("enabled", true).apply()
        NodeService.start(this)
        // Permission prompts can take several seconds; only begin loading the local UI after the service starts.
        ui.postDelayed({ ui.loadUrl("http://127.0.0.1:4799/") }, 800)

        val power = getSystemService(PowerManager::class.java)
        if (!power.isIgnoringBatteryOptimizations(packageName)) {
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        }
    }

    companion object {
        private const val REQUEST_STARTUP_PERMISSIONS = 1
    }
}
