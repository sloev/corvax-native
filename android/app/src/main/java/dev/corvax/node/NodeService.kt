package dev.corvax.node

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Build
import android.os.IBinder
import android.webkit.WebView
import androidx.webkit.WebViewAssetLoader
import androidx.webkit.WebViewClientCompat

/**
 * The always-on CORVAX node: a foreground service (type connectedDevice) that runs CORVAX's JS node bundle in a
 * headless WebView. The WebView renderer keeps IMPORTANT priority while invisible, so Doze doesn't starve it.
 * Policy: relay and store for others only on Wi-Fi while charging; presence and messages always.
 */
class NodeService : Service() {
    private lateinit var web: WebView
    private lateinit var host: HostBridge
    private lateinit var radios: Radios
    private var multicast: WifiManager.MulticastLock? = null

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate() {
        super.onCreate()
        // Android 14+ refuses a connectedDevice service without its prerequisites: report it instead of dying silently.
        try { startForeground(1, notification("Starting…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE) } catch (e: Exception) {
            getSharedPreferences("corvax", MODE_PRIVATE).edit().putString("error", "Android didn't allow the background service: ${e.message}").apply()
            stopSelf(); return
        }
        multicast = (getSystemService(WIFI_SERVICE) as WifiManager).createMulticastLock("corvax-lan").apply { setReferenceCounted(false); acquire() }

        web = WebView(applicationContext)
        web.settings.javaScriptEnabled = true
        web.setRendererPriorityPolicy(WebView.RENDERER_PRIORITY_IMPORTANT, false)          // stays important when not visible
        val assets = WebViewAssetLoader.Builder().addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(this)).build()
        web.webViewClient = object : WebViewClientCompat() {
            override fun shouldInterceptRequest(v: WebView, r: android.webkit.WebResourceRequest) = assets.shouldInterceptRequest(r.url)
        }
        getSharedPreferences("corvax", MODE_PRIVATE).edit().remove("error").apply()
        host = HostBridge(this, web, onFailed = { update("Couldn't start: $it") }) { key, isAnchor -> update(if (isAnchor) "Anchor ${key.take(6)}… · sharing" else "Online ${key.take(6)}…") }
        web.addJavascriptInterface(host, "CorvaxHost")
        radios = Radios(this, host)
        web.loadUrl("https://appassets.androidplatform.net/assets/node.html")
        radios.start()
        watchPolicy()
    }

    // Wi-Fi + charging → relay and store for others. Re-evaluated on every change.
    private fun watchPolicy() {
        val cm = getSystemService(ConnectivityManager::class.java)
        fun apply() {
            val wifi = cm.getNetworkCapabilities(cm.activeNetwork)?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true
            val charging = getSystemService(BatteryManager::class.java).isCharging
            host.emit("policy", """{"relay":${wifi && charging},"storage":${wifi && charging}}""")
        }
        cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(n: Network, c: NetworkCapabilities) { web.post { apply() } }
            override fun onLost(n: Network) { web.post { apply() } }
        })
        val power = IntentFilter().apply { addAction(Intent.ACTION_POWER_CONNECTED); addAction(Intent.ACTION_POWER_DISCONNECTED) }
        registerReceiver(object : BroadcastReceiver() { override fun onReceive(c: Context, i: Intent) = apply() }, power)
    }

    private fun notification(text: String): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("node", "CORVAX node", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 1, Intent(this, NodeService::class.java).setAction("stop"), PendingIntent.FLAG_IMMUTABLE)
        return Notification.Builder(this, "node").setSmallIcon(R.drawable.ic_corvax).setContentTitle("CORVAX").setContentText(text)
            .setOngoing(true).setContentIntent(open).addAction(Notification.Action.Builder(null, "Pause", stop).build()).build()
    }
    private fun update(text: String) = getSystemService(NotificationManager::class.java).notify(1, notification(text))

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "stop") { stopSelf(); return START_NOT_STICKY }
        return START_STICKY
    }
    override fun onDestroy() {
        if (::radios.isInitialized) radios.stop()
        if (::host.isInitialized) host.close()
        if (::web.isInitialized) web.destroy()
        multicast?.release(); super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        fun start(c: Context) = if (Build.VERSION.SDK_INT >= 26) c.startForegroundService(Intent(c, NodeService::class.java)) else c.startService(Intent(c, NodeService::class.java))
    }
}
