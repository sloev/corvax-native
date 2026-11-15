package dev.corvax.node

import org.json.JSONObject
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * A small HTTP/1.1 server for the node (one request per connection) plus RFC 6455 WebSockets. Each request goes to
 * the JS handler ("http" event) and waits for http_respond; if the client leaves first, "http-abort" ends the long poll.
 */
class Http(private val host: HostBridge) {
    private class Res(val status: Int, val headers: JSONObject, val body: ByteArray)
    private val servers = ConcurrentHashMap<Long, ServerSocket>()
    private val waiting = ConcurrentHashMap<Long, ArrayBlockingQueue<Res>>()
    private val sockets = ConcurrentHashMap<Long, Socket>()

    fun listen(address: String, port: Int): Long {
        val server = ServerSocket(port, 64, InetAddress.getByName(address)).apply { reuseAddress = true }
        val sid = host.id(); servers[sid] = server
        thread(name = "corvax-http") { while (!server.isClosed) runCatching { val s = server.accept(); thread { handle(sid, s) } } }
        return sid
    }

    private fun handle(sid: Long, s: Socket) { s.use {
        val input = DataInputStream(BufferedInputStream(s.getInputStream()))
        val (method, target) = line(input)?.split(" ")?.let { it[0] to it.getOrElse(1) { "/" } } ?: return
        val headers = JSONObject()
        while (true) { val l = line(input) ?: return; if (l.isEmpty()) break; val i = l.indexOf(':'); if (i > 0) headers.put(l.substring(0, i).trim().lowercase(), l.substring(i + 1).trim()) }
        val path = target.substringBefore('?')
        if (headers.optString("upgrade").equals("websocket", true)) return websocket(sid, s, input, path, headers)
        val len = headers.optString("content-length").toIntOrNull() ?: 0
        if (len > 24 * 1024) return write(s.getOutputStream(), Res(413, JSONObject(), ByteArray(0)))
        val body = ByteArray(len).also { input.readFully(it) }
        val rid = host.id(); val q = ArrayBlockingQueue<Res>(1); waiting[rid] = q
        host.emit("http", JSONObject().put("sid", sid).put("rid", rid).put("method", method).put("path", path).put("headers", headers)
            .put("ip", s.inetAddress.hostAddress).put("port", s.port).put("body", HostBridge.b64(body)).toString())
        val watcher = thread { if (runCatching { input.read() }.getOrDefault(-1) == -1) host.emit("http-abort", """{"rid":$rid}""") }   // client went away
        val res = q.poll(75, TimeUnit.SECONDS) ?: Res(504, JSONObject(), ByteArray(0))
        waiting.remove(rid); watcher.interrupt()
        runCatching { write(s.getOutputStream(), res) }
    } }

    fun respond(rid: Long, status: Int, headers: JSONObject, body: ByteArray) { waiting[rid]?.offer(Res(status, headers, body)) }

    private fun write(out: OutputStream, r: Res) {
        val head = StringBuilder("HTTP/1.1 ${r.status} OK\r\nconnection: close\r\ncontent-length: ${r.body.size}\r\n")
        r.headers.keys().forEach { head.append("$it: ${r.headers.getString(it)}\r\n") }
        out.write(head.append("\r\n").toString().toByteArray()); out.write(r.body); out.flush()
    }
    private fun line(i: DataInputStream): String? {
        val b = StringBuilder()
        while (true) { val c = i.read(); if (c < 0) return null; if (c == '\n'.code) return b.toString().trimEnd('\r'); b.append(c.toChar()); if (b.length > 8192) return null }
    }

    // ---- WebSocket ------------------------------------------------------------------------------
    private fun websocket(sid: Long, s: Socket, input: DataInputStream, path: String, headers: JSONObject) {
        val accept = HostBridge.b64(MessageDigest.getInstance("SHA-1").digest((headers.getString("sec-websocket-key") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").toByteArray()))
        s.getOutputStream().apply { write("HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: $accept\r\n\r\n".toByteArray()); flush() }
        val wid = host.id(); sockets[wid] = s
        host.emit("ws-open", JSONObject().put("sid", sid).put("wid", wid).put("path", path).toString())
        runCatching {
            while (true) {
                val b0 = input.readUnsignedByte()
                val b1 = input.readUnsignedByte()
                val opcode = b0 and 15
                val masked = b1 and 128 != 0
                var len = (b1 and 127).toLong()
                if ((b0 and 0x70) != 0 || (b0 and 0x80) == 0 || !masked) break // no extensions or fragmented messages
                if (len == 126L) {
                    len = input.readUnsignedShort().toLong()
                    if (len < 126L) break // non-minimal encoding
                } else if (len == 127L) {
                    len = input.readLong()
                    if (len < 65536L) break // non-minimal encoding
                }
                // readLong() is signed: a set high bit produces a negative value.
                // Cap allocation even for valid 64-bit lengths to keep one peer bounded.
                if (len < 0L || len > 16L * 1024L * 1024L) break
                if (opcode !in setOf(1, 2, 8, 9, 10)) break
                if (opcode >= 8 && ((b0 and 0x80) == 0 || len > 125L)) break // control frames must be final and <= 125 bytes
                val mask = ByteArray(4).also { input.readFully(it) }
                val data = ByteArray(len.toInt()).also { input.readFully(it) }
                for (i in data.indices) data[i] = (data[i].toInt() xor mask[i and 3].toInt()).toByte()
                when (opcode) {
                    1 -> host.emit("ws-msg", JSONObject().put("wid", wid).put("text", String(data, Charsets.UTF_8)).toString())
                    2 -> host.emit("ws-msg", JSONObject().put("wid", wid).put("data", HostBridge.b64(data)).toString())
                    8 -> { frame(s, 8, data); break }
                    9 -> frame(s, 10, data)
                    10 -> Unit // pong
                    else -> break
                }
            }
        }
        sockets.remove(wid); host.emit("ws-close", """{"wid":$wid}""")
    }
    private fun frame(s: Socket, op: Int, data: ByteArray) = synchronized(s) {
        if (data.size > 16 * 1024 * 1024 || (op >= 8 && data.size > 125)) return@synchronized
        val length = data.size.toLong()
        val head = when {
            length < 126L -> byteArrayOf((0x80 or op).toByte(), length.toByte())
            length <= 65535L -> byteArrayOf((0x80 or op).toByte(), 126.toByte(), (length shr 8).toByte(), length.toByte())
            else -> ByteArray(10).apply {
                this[0] = (0x80 or op).toByte()
                this[1] = 127.toByte()
                for (i in 0..7) this[2 + i] = (length ushr ((7 - i) * 8)).toByte()
            }
        }
        runCatching { s.getOutputStream().apply { write(head); write(data); flush() } }
    }
    fun wsSend(wid: Long, text: String?, data: ByteArray?) { val s = sockets[wid] ?: return; if (text != null) frame(s, 1, text.toByteArray()) else if (data != null) frame(s, 2, data) }
    fun wsClose(wid: Long) { sockets.remove(wid)?.let { frame(it, 8, ByteArray(0)); runCatching { it.close() } } }

    fun close(sid: Long) { servers.remove(sid)?.close() }
    fun closeAll() { servers.values.forEach { it.close() }; sockets.values.forEach { runCatching { it.close() } } }
}
