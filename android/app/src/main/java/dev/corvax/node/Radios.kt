package dev.corvax.node

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.content.pm.PackageManager
import android.net.wifi.p2p.WifiP2pConfig
import android.net.wifi.p2p.WifiP2pManager
import android.net.wifi.p2p.nsd.WifiP2pDnsSdServiceInfo
import android.net.wifi.p2p.nsd.WifiP2pDnsSdServiceRequest
import android.os.Build
import android.os.ParcelUuid
import android.util.Log
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * Local radios (SPEC §12): BLE and Wi-Fi Direct between nearby phones, no internet or access point needed.
 *  - BLE: advertise the CORVAX service (+ 8 bytes of our key), scan for others, carry KRPC in chunks over GATT.
 *    The core sees BLE peers as UDP addresses "ble:<device>", so its DHT and queries work over Bluetooth unchanged.
 *  - Wi-Fi Direct: DNS-SD "_corvax._udp" with our anchor line; found lines go to the anchor book; we join groups
 *    so nodes then speak UDP, HTTP and WebRTC over the group network.
 *
 * Each radio is initialized independently. Denying one nearby-device permission must not prevent the other radio
 * (or the core node) from starting.
 */
@SuppressLint("MissingPermission")
class Radios(private val ctx: Context, private val host: HostBridge) {
    companion object {
        private const val TAG = "CorvaxRadios"
        val SERVICE: UUID = UUID.fromString("c0a40001-0c0a-4c58-8000-00000000c0a4")
        val WRITE: UUID = UUID.fromString("c0a40002-0c0a-4c58-8000-00000000c0a4")
        val NOTIFY: UUID = UUID.fromString("c0a40003-0c0a-4c58-8000-00000000c0a4")
        val CCCD: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
    }
    private val bt = ctx.getSystemService(BluetoothManager::class.java)
    private var gatt: BluetoothGattServer? = null
    private val clients = ConcurrentHashMap<String, BluetoothGatt>()
    private val subscribers = ConcurrentHashMap<String, BluetoothDevice>()
    private val mtu = ConcurrentHashMap<String, Int>()
    private val partial = ConcurrentHashMap<String, ByteArrayOutputStream>()
    private val p2p = ctx.getSystemService(WifiP2pManager::class.java)
    private var channel: WifiP2pManager.Channel? = null

    init { host.radios = this }

    private fun granted(permission: String) = ctx.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

    private fun hasBlePermissions(): Boolean = if (Build.VERSION.SDK_INT >= 31) {
        granted(Manifest.permission.BLUETOOTH_SCAN) &&
            granted(Manifest.permission.BLUETOOTH_ADVERTISE) &&
            granted(Manifest.permission.BLUETOOTH_CONNECT)
    } else {
        granted(Manifest.permission.ACCESS_FINE_LOCATION)
    }

    private fun hasWifiDirectPermission(): Boolean = if (Build.VERSION.SDK_INT >= 33) {
        granted(Manifest.permission.NEARBY_WIFI_DEVICES)
    } else {
        granted(Manifest.permission.ACCESS_FINE_LOCATION)
    }

    fun start() {
        if (hasBlePermissions()) runCatching { startBle() }
            .onFailure { Log.w(TAG, "BLE initialization failed", it) }
        if (hasWifiDirectPermission()) runCatching {
            channel = p2p?.initialize(ctx, ctx.mainLooper, null)
        }.onFailure { Log.w(TAG, "Wi-Fi Direct initialization failed", it) }
    }

    private fun startBle() {
        val adapter = bt?.adapter ?: run {
            Log.i(TAG, "Bluetooth adapter unavailable; BLE disabled")
            return
        }
        gatt = adapter.openGattServer(ctx, server)
        val serviceAdded = gatt?.addService(BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY).apply {
            addCharacteristic(BluetoothGattCharacteristic(WRITE, BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE, BluetoothGattCharacteristic.PERMISSION_WRITE))
            addCharacteristic(BluetoothGattCharacteristic(NOTIFY, BluetoothGattCharacteristic.PROPERTY_NOTIFY, 0).apply {
                addDescriptor(BluetoothGattDescriptor(CCCD, BluetoothGattDescriptor.PERMISSION_WRITE or BluetoothGattDescriptor.PERMISSION_READ))
            })
        }) ?: false
        if (!serviceAdded) Log.w(TAG, "Could not register the BLE GATT service")

        val scanner = adapter.bluetoothLeScanner
        if (scanner == null) {
            Log.w(TAG, "BLE scanner unavailable")
            return
        }
        scanner.startScan(
            listOf(ScanFilter.Builder().setServiceUuid(ParcelUuid(SERVICE)).build()),
            ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_POWER).build(),
            scan
        )
    }

    // Called when the node is ready: advertise over BLE and Wi-Fi Direct.
    fun advertise(key: String, line: String) {
        if (hasBlePermissions()) runCatching {
            val adapter = bt?.adapter
            val advertiser = adapter?.bluetoothLeAdvertiser
            if (advertiser == null) {
                Log.w(TAG, "BLE advertiser unavailable")
            } else {
                val keyBytes = key.toByteArray().copyOf(8)
                advertiser.startAdvertising(
                    AdvertiseSettings.Builder().setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_POWER).setConnectable(true).build(),
                    AdvertiseData.Builder().addServiceUuid(ParcelUuid(SERVICE)).addServiceData(ParcelUuid(SERVICE), keyBytes).build(),
                    advertiseCallback
                )
            }
        }.onFailure { Log.w(TAG, "BLE advertising failed", it) }

        if (!hasWifiDirectPermission()) return
        runCatching {
            val ch = channel ?: return
            if (line.isNotEmpty()) p2p?.addLocalService(ch, WifiP2pDnsSdServiceInfo.newInstance("corvax", "_corvax._udp", mapOf("line" to line)), null)
            p2p?.setDnsSdResponseListeners(ch, null) { _, record, device ->
                record["line"]?.let { host.emit("radio", JSONObject().put("text", it).toString()) }
                p2p.connect(ch, WifiP2pConfig().apply { deviceAddress = device.deviceAddress }, null)
            }
            p2p?.addServiceRequest(ch, WifiP2pDnsSdServiceRequest.newInstance("_corvax._udp"), null)
            p2p?.discoverServices(ch, null)
        }.onFailure { Log.w(TAG, "Wi-Fi Direct discovery failed", it) }
    }

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartFailure(errorCode: Int) {
            Log.w(TAG, "BLE advertising failed with code $errorCode")
        }
    }

    // ---- BLE: the core sends to "ble:<id>" ----------------------------------------------------------
    fun send(peer: String, data: ByteArray) {
        val size = (mtu[peer] ?: 23) - 5
        data.toList().chunked(size).forEachIndexed { i, part ->
            val last = (i + 1) * size >= data.size
            val chunk = byteArrayOf(i.toByte(), if (last) 1 else 0) + part.toByteArray()
            clients[peer]?.let { g -> g.getService(SERVICE)?.getCharacteristic(WRITE)?.let { c -> g.writeCharacteristic(c, chunk, BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE) } }
                ?: subscribers[peer]?.let { d -> gatt?.getService(SERVICE)?.getCharacteristic(NOTIFY)?.let { c -> gatt?.notifyCharacteristicChanged(d, c, false, chunk) } }
        }
    }

    private fun received(peer: String, chunk: ByteArray) {
        if (chunk.size < 2) return
        val buf = partial.getOrPut(peer) { ByteArrayOutputStream() }
        if (chunk[0].toInt() == 0) buf.reset()
        buf.write(chunk, 2, chunk.size - 2)
        if (buf.size() > 16 * 1024) { buf.reset(); return }
        if (chunk[1].toInt() and 1 == 1) { host.fromRadio(peer, buf.toByteArray()); buf.reset() }
    }

    private val scan = object : ScanCallback() {
        override fun onScanResult(type: Int, r: ScanResult) {
            val id = r.device.address
            if (clients.containsKey(id) || subscribers.containsKey(id) || clients.size >= 4) return
            clients[id] = r.device.connectGatt(ctx, false, client, BluetoothDevice.TRANSPORT_LE)
        }
    }
    private val client = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(g: BluetoothGatt, status: Int, state: Int) {
            if (state == BluetoothProfile.STATE_CONNECTED) g.requestMtu(517) else { clients.remove(g.device.address); g.close() }
        }
        override fun onMtuChanged(g: BluetoothGatt, m: Int, status: Int) { mtu[g.device.address] = m; g.discoverServices() }
        override fun onServicesDiscovered(g: BluetoothGatt, status: Int) {
            val c = g.getService(SERVICE)?.getCharacteristic(NOTIFY) ?: return
            g.setCharacteristicNotification(c, true)
            c.getDescriptor(CCCD)?.let { g.writeDescriptor(it, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE) }
        }
        override fun onCharacteristicChanged(g: BluetoothGatt, c: BluetoothGattCharacteristic, value: ByteArray) = received(g.device.address, value)
    }
    private val server = object : BluetoothGattServerCallback() {
        override fun onConnectionStateChange(d: BluetoothDevice, status: Int, state: Int) {
            if (state == BluetoothProfile.STATE_CONNECTED) subscribers[d.address] = d else subscribers.remove(d.address)
        }
        override fun onMtuChanged(d: BluetoothDevice, m: Int) { mtu[d.address] = m }
        override fun onCharacteristicWriteRequest(d: BluetoothDevice, id: Int, c: BluetoothGattCharacteristic, prep: Boolean, resp: Boolean, off: Int, value: ByteArray) {
            if (c.uuid == WRITE) received(d.address, value)
            if (resp) gatt?.sendResponse(d, id, BluetoothGatt.GATT_SUCCESS, 0, null)
        }
        override fun onDescriptorWriteRequest(d: BluetoothDevice, id: Int, desc: BluetoothGattDescriptor, prep: Boolean, resp: Boolean, off: Int, value: ByteArray) {
            if (resp) gatt?.sendResponse(d, id, BluetoothGatt.GATT_SUCCESS, 0, null)
        }
    }

    fun stop() {
        if (hasBlePermissions()) runCatching {
            bt?.adapter?.bluetoothLeScanner?.stopScan(scan)
            bt?.adapter?.bluetoothLeAdvertiser?.stopAdvertising(advertiseCallback)
        }.onFailure { Log.w(TAG, "Stopping BLE discovery/advertising failed", it) }
        clients.values.forEach { runCatching { it.disconnect(); it.close() } }
        clients.clear()
        subscribers.clear()
        mtu.clear()
        partial.clear()
        runCatching { gatt?.close() }.onFailure { Log.w(TAG, "Closing BLE GATT server failed", it) }
        gatt = null
        if (hasWifiDirectPermission()) runCatching {
            channel?.let { p2p?.clearLocalServices(it, null); p2p?.clearServiceRequests(it, null) }
        }.onFailure { Log.w(TAG, "Stopping Wi-Fi Direct failed", it) }
        channel = null
    }
}
