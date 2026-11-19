package dev.corvax.node

import android.content.Context
import android.util.Base64
import android.webkit.JavascriptInterface
import android.webkit.WebView
import org.json.JSONObject
import java.net.DatagramPacket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.MulticastSocket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

/**
 * The Kotlin half of the host interface (SPEC §11), called from shared/host-bridge.js:
 * CorvaxHost.call(id, name, json) → work on a thread → window.__host.reply(id, result). Events: window.__host.emit.
 * UDP hosts named "ble:<id>" go over Bluetooth (Radios), so the core's DHT and KRPC work over BLE unchanged.
 */
class HostBridge(private val ctx: Context, private val web: WebView, private val onFailed: (String) -> Unit = {}, private val onReady: (String, Boolean) -> Unit) {
    private val pool = Executors.newCachedThreadPool()
    private val ids = AtomicLong()
    private val udp = ConcurrentHashMap<Long, MulticastSocket>()
    val http = Http(this)
    var radios: Radios? = null
    private val kv = ctx.getSharedPreferences("corvax", Context.MODE_PRIVATE)

    // The service's WebView is never attached to a window, so web.post would queue forever: post to the main looper.
    private val main = android.os.Handler(android.os.Looper.getMainLooper())
    fun id() = ids.incrementAndGet()
    fun emit(event: String, json: String) = main.post { web.evaluateJavascript("window.__host.emit(${JSONObject.quote(event)}, $json)", null) }
    private fun reply(id: Long, result: Any?, error: String? = null) {
        val r = when (result) { null -> "null"; is String -> JSONObject.quote(result); else -> result.toString() }
        main.post { web.evaluateJavascript("window.__host.reply($id, $r, ${if (error == null) "null" else JSONObject.quote(error)})", null) }
    }

    @JavascriptInterface
    fun call(id: Long, name: String, json: String) {
        val a = JSONObject(json)
        if (name in setOf("udp_bind", "http_listen", "kv_get", "kv_set")) Trace.log("host: $name ${json.take(80)}")
        pool.execute {
            try { reply(id, dispatch(name, a)) } catch (e: Exception) { Trace.log("host: $name failed: $e"); reply(id, null, e.message ?: "error") }
        }
    }

    private fun dispatch(name: String, a: JSONObject): Any? = when (name) {
        "udp_bind" -> udpBind(a)
        "udp_send" -> { udpSend(a.getLong("id"), b64(a.getString("data")), a.getInt("port"), a.getString("host")); null }
        "udp_close" -> { udp.remove(a.getLong("id"))?.close(); null }
        "http_listen" -> JSONObject().put("sid", http.listen(a.getString("address"), a.getInt("port")))
        "http_respond" -> { http.respond(a.getLong("rid"), a.getInt("status"), a.getJSONObject("headers"), b64(a.getString("body"))); null }
        "http_close" -> { http.close(a.getLong("sid")); null }
        "ws_send" -> { http.wsSend(a.getLong("wid"), a.optString("text", null), a.optString("data", null)?.let(::b64)); null }
        "ws_close" -> { http.wsClose(a.getLong("wid")); null }
        "kv_get" -> kv.getString(a.getString("name"), null)
        "kv_set" -> { kv.edit().putString(a.getString("name"), a.getString("value")).apply(); null }
        "dns_txt" -> ""                                                     // optional; anchors come from other channels
        "log" -> { Trace.log("node: ${a.optString("m")}"); null }
        "node_failed" -> { Trace.log("node FAILED: ${a.optString("error")}")
             val e = a.optString("error", "unknown error"); kv.edit().putString("error", e).apply(); onFailed(e); null }
        "node_ready" -> { Trace.log("node ready: ${a.optString("key")} port ${a.optInt("port")}"); kv.edit().remove("error").apply(); onReady(a.getString("key"), a.getString("line").isNotEmpty()); radios?.advertise(a.getString("key"), a.getString("line")); null }
        else -> throw IllegalArgumentException("unknown $name")
    }

    private fun udpBind(a: JSONObject): JSONObject {
        val s = MulticastSocket(null).apply {
            reuseAddress = a.optBoolean("reuse"); broadcast = a.optBoolean("broadcast")
            bind(InetSocketAddress(a.getString("address"), a.getInt("port")))
        }
        if (!a.isNull("group")) runCatching { s.joinGroup(InetAddress.getByName(a.getString("group"))) }
        val id = id(); udp[id] = s
        pool.execute {
            val buf = ByteArray(65536)
            while (!s.isClosed) {
                val p = DatagramPacket(buf, buf.size)
                runCatching { s.receive(p) }.onFailure { return@execute }
                emit("udp", JSONObject().put("id", id).put("data", b64(buf.copyOf(p.length))).put("address", p.address.hostAddress).put("port", p.port).toString())
            }
        }
        return JSONObject().put("id", id).put("port", s.localPort)
    }
    private fun udpSend(id: Long, data: ByteArray, port: Int, host: String) {
        if (host.startsWith("ble:")) return radios?.send(host.removePrefix("ble:"), data) ?: Unit
        udp[id]?.send(DatagramPacket(data, data.size, InetAddress.getByName(host), port))
    }
    // Bluetooth → the core, as if it were a UDP datagram from "ble:<id>" (delivered on the node's main UDP socket).
    fun fromRadio(peer: String, data: ByteArray) {
        val main = udp.keys.minOrNull() ?: return
        emit("udp", JSONObject().put("id", main).put("data", b64(data)).put("address", "ble:$peer").put("port", 0).toString())
    }

    fun close() { udp.values.forEach { it.close() }; http.closeAll(); pool.shutdownNow() }

    companion object {
        fun b64(s: String): ByteArray = Base64.decode(s, Base64.NO_WRAP)
        fun b64(b: ByteArray): String = Base64.encodeToString(b, Base64.NO_WRAP)
    }
}
